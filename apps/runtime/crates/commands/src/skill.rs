//! Procedural-memory helpers.
//!
//! Three responsibilities, all local to the CLI shell (no Core change):
//! 1. Skill-body template check (a skill entry is a manual, not an essay);
//! 2. How-to intent detection for recall routing;
//! 3. How-to ordering: merge a skill-only pass and keep one procedure visible.
//!
//! Run-history reads (see `skill_runs` in the local store) only break ties
//! between procedures. Fact entries keep Core's order and score.

use respire_app::memory::engine::RecalledWithContext;
use std::collections::HashMap;

/// Canonical labels a skill body should carry, inside the standard
/// `Cause/Action/Result` markers (required for new structured bodies); legacy Chinese remains readable.
pub const SECTIONS: [&str; 6] = ["Trigger", "Prerequisites", "Steps", "Verification", "Rollback", "Scope"];

/// Accepted spellings per section. Matching is substring-based so
/// the bracketed trigger label, the label with a full-width colon and `Trigger:` all count.
const ALIASES: [(&str, &[&str]); 6] = [
    (
        "Trigger",
        &["\u{89e6}\u{53d1}\u{6761}\u{4ef6}", "\u{89e6}\u{53d1}", "\u{4f55}\u{65f6}\u{7528}", "when to use", "trigger"],
    ),
    (
        "Prerequisites",
        &["\u{524d}\u{7f6e}\u{6761}\u{4ef6}", "\u{524d}\u{7f6e}", "\u{524d}\u{63d0}", "prerequisites", "prerequisite", "requires"],
    ),
    ("Steps", &["\u{6267}\u{884c}\u{6b65}\u{9aa4}", "\u{6b65}\u{9aa4}", "steps", "procedure"]),
    (
        "Verification",
        &["\u{6210}\u{529f}\u{5224}\u{636e}", "\u{6821}\u{9a8c}", "\u{9a8c}\u{8bc1}", "verification", "verify"],
    ),
    (
        "Rollback",
        &[
            "\u{5931}\u{8d25}\u{56de}\u{9000}",
            "\u{5931}\u{8d25}\u{5904}\u{7406}",
            "\u{56de}\u{9000}",
            "\u{515c}\u{5e95}",
            "rollback",
            "fallback",
        ],
    ),
    (
        "Scope",
        &["\u{9002}\u{7528}\u{8fb9}\u{754c}", "\u{9002}\u{7528}\u{8303}\u{56f4}", "\u{8fb9}\u{754c}", "\u{4e0d}\u{9002}\u{7528}", "scope", "limits"],
    ),
];

/// A label only counts as a section heading when it is followed by a separator
/// (the trigger label with a full-width colon, the bracketed steps label, `- ` plus the
/// rollback label), so prose such as "not verified" cannot stand in for the verification
/// section.
fn heading(lower: &str, alias: &str) -> bool {
    const SEPARATORS: [char; 11] = ['\u{ff1a}', ':', '\u{3011}', ']', ')', '-', '\u{2014}', '\u{3001}', '.', '\u{3002}', '\n'];
    lower.match_indices(alias).any(|(at, _)| {
        let rest = lower[at + alias.len()..].trim_start();
        match rest.chars().next() {
            Some(c) => SEPARATORS.contains(&c),
            None => false,
        }
    })
}

/// Missing canonical sections. Empty means the body satisfies the template.
pub fn missing_sections(content: &str) -> Vec<&'static str> {
    let lower = content.to_lowercase();
    ALIASES
        .iter()
        .filter(|(_, needles)| !needles.iter().any(|n| heading(&lower, n)))
        .map(|(label, _)| *label)
        .collect()
}

/// Procedures are `skill` and `knowledge` entries; everything else is a fact.
pub fn is_procedure(kind: &str) -> bool {
    matches!(kind, "skill" | "knowledge")
}

/// Hit count from which a fact is worth re-reading during tidy (advisory only:
/// the AI still decides whether it is a procedure). Cumulative, because
/// `recall_count` carries no timestamps.
pub const HOT_RECALL_THRESHOLD: i64 = 5;

/// Loose operational smell: commands, paths, or numbered steps. Used only to
/// narrow the audit list; the upgrade decision stays with the AI.
pub fn looks_operational(content: &str) -> bool {
    const MARKERS: [&str; 14] = [
        "```", "$ ", "sudo ", "cd ", "cargo ", "npm ", "git ", "rsrs ", "python ", "node ", "\u{6b65}\u{9aa4}",
        "\u{547d}\u{4ee4}", "1. ", "1) ",
    ];
    let lower = content.to_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
        || content.contains('\u{2192}')
        || content.contains('\u{ff0f}')
        || content.contains("\\")
}

/// Conservative how-to detector. Only widens recall (adds a skill pass);
/// it never filters facts out, so a false positive costs one extra pass.
pub fn howto_intent(query: &str) -> bool {
    const NEEDLES: [&str; 26] = [
        "\u{600e}\u{4e48}", "\u{5982}\u{4f55}", "\u{600e}\u{6837}", "\u{548b}\u{529e}", "\u{62a5}\u{9519}", "\u{5931}\u{8d25}", "\u{9519}\u{8bef}", "\u{6b65}\u{9aa4}", "\u{6d41}\u{7a0b}", "\u{547d}\u{4ee4}", "\u{811a}\u{672c}",
        "\u{914d}\u{7f6e}", "\u{5b89}\u{88c5}", "\u{90e8}\u{7f72}", "\u{8c03}\u{8bd5}", "\u{6392}\u{67e5}", "\u{4fee}\u{590d}", "\u{8dd1}\u{901a}", "\u{64cd}\u{4f5c}", "\u{4e0a}\u{4f20}", "\u{5bfc}\u{5165}", "\u{6253}\u{5305}",
        "how to", "how do", "how can", "steps",
    ];
    let lower = query.to_lowercase();
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

/// Run history used for procedure ordering (ok/fail counts, 0/0 = unrun).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunScore {
    pub ok: i64,
    pub fail: i64,
}

impl RunScore {
    /// Small, bounded nudge: ±0.03 for all-ok / all-fail, 0 when unrun.
    fn delta(&self) -> f32 {
        let total = self.ok + self.fail;
        if total <= 0 {
            return 0.0;
        }
        0.03 * (self.ok - self.fail) as f32 / total as f32
    }
}

/// Merge a skill-only pass into the primary hits for a how-to query.
///
/// Union by id, order by score, then — if no procedure is already in the top
/// three and one exists — move the best-scored procedure to index 2.
/// "Best" ranks procedures by `score + run delta`, so a procedure that keeps
/// failing sinks below one that keeps working. Scores are never rewritten:
/// the caller still prints Core's own values, and `promoted` says whether the
/// order departs from them.
pub fn merge_howto(
    primary: Vec<RecalledWithContext>,
    secondary: Vec<RecalledWithContext>,
    limit: usize,
    runs: &HashMap<String, RunScore>,
) -> (Vec<RecalledWithContext>, bool) {
    let mut merged: Vec<RecalledWithContext> = primary;
    for row in secondary {
        if !merged.iter().any(|r| r.entry.id == row.entry.id) {
            merged.push(row);
        }
    }
    // Facts sort on Core's score; procedures get the bounded run nudge, so a
    // procedure that keeps failing sinks below one that keeps working.
    let procedure_rank = |r: &RecalledWithContext| {
        if !is_procedure(r.entry.kind.as_str()) {
            return r.score;
        }
        r.score
            + runs
                .get(&r.entry.id)
                .map(RunScore::delta)
                .unwrap_or_default()
    };
    merged.sort_by(|a, b| {
        procedure_rank(b)
            .partial_cmp(&procedure_rank(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    if merged.is_empty() {
        return (merged, false);
    }
    let best = merged
        .iter()
        .enumerate()
        .filter(|(_, r)| is_procedure(r.entry.kind.as_str()))
        .max_by(|(_, a), (_, b)| {
            procedure_rank(a)
                .partial_cmp(&procedure_rank(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(idx, _)| idx);
    let mut promoted = false;
    if let Some(idx) = best {
        if idx >= 3
            && merged[..3]
                .iter()
                .all(|r| !is_procedure(r.entry.kind.as_str()))
        {
            let row = merged.remove(idx);
            merged.insert(2, row);
            promoted = true;
        }
    }
    merged.truncate(limit);
    (merged, promoted)
}

#[cfg(test)]
mod tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/skill/tests.rs")); }
