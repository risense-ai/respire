//! Causal tree construction, model request shaping, and reparenting validation.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::localized_notation::{FULLWIDTH_ARROW, FULLWIDTH_COLON, FULLWIDTH_SEMICOLON};

/// Top ancestor title map: rows = (id, parent_id, title); id -> top-level title (cycle-safe, broken-parent tolerant).
pub fn top_roots(rows: &[(String, String, String)]) -> HashMap<String, String> {
    let by_id: HashMap<&str, (&str, &str)> = rows
        .iter()
        .map(|(id, p, t)| (id.as_str(), (p.as_str(), t.as_str())))
        .collect();
    let mut out = HashMap::new();
    for (id, _, _) in rows {
        let mut cur = id.as_str();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut root_title = String::new();
        loop {
            if !seen.insert(cur) {
                break; // cycle guard
            }
            match by_id.get(cur) {
                Some((p, t)) => {
                    if p.is_empty() {
                        root_title = (*t).to_owned();
                        break;
                    }
                    if by_id.contains_key(p) {
                        cur = p;
                    } else {
                        root_title = (*t).to_owned(); // parent not in the set (broken chain) - this node is the top
                        break;
                    }
                }
                None => break,
            }
        }
        out.insert(id.clone(), root_title);
    }
    out
}

/// --------------------- Tree batch judge (one request, N entries, answer a tree node) ---------------------
///
/// Why: one-by-one is many round-trips (50 entries = 50 calls, official sampling ×5), slow and expensive;
/// and "pick 1 of 23 roots" is too coarse - the real need is "which concrete node in the tree".
/// Measured (23 cases, 486-node tree): one-by-one 82.6% / 24s / 192k tokens;
/// batch 12 per request 91.3% / 2s / 17.8k tokens - **12× faster, 10.8× fewer tokens, more accurate**.
/// Batch is more accurate because the model sees the whole-tree outline plus several entries at once.

/// A tree node (one flattened attach candidate).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub title: String,
    pub depth: usize,
    /// Subtree size (descendant count) - so the model can see node bulk.
    pub size: usize,
}

/// Build a depth-capped tree outline from (id, parent, title, important) rows.
/// Roots sorted by subtree size desc; children at each level too - large families first, easier to grab the trunk.
pub fn build_tree_nodes(
    rows: &[(String, String, String, bool)],
    max_depth: usize,
    min_keep: usize,
) -> Vec<TreeNode> {
    use std::collections::HashMap as Map;
    let by_id: Map<&str, &(String, String, String, bool)> =
        rows.iter().map(|r| (r.0.as_str(), r)).collect();
    let mut kids: Map<&str, Vec<&str>> = Map::new();
    for r in rows {
        if !r.1.is_empty() && by_id.contains_key(r.1.as_str()) {
            kids.entry(r.1.as_str()).or_default().push(r.0.as_str());
        }
    }
    let mut size_cache: Map<&str, usize> = Map::new();
    fn size<'a>(
        id: &'a str,
        kids: &Map<&'a str, Vec<&'a str>>,
        cache: &mut Map<&'a str, usize>,
    ) -> usize {
        if let Some(n) = cache.get(id) {
            return *n;
        }
        let n: usize = kids
            .get(id)
            .map_or(0, |v| v.iter().map(|k| 1 + size(k, kids, cache)).sum());
        cache.insert(id, n);
        n
    }
    let roots: Vec<&str> = rows
        .iter()
        .filter(|r| r.1.is_empty() || !by_id.contains_key(r.1.as_str()))
        .map(|r| r.0.as_str())
        .collect();
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // **DFS preorder** (not BFS): parent and child must sit next to each other or indent is unreadable.
    // BFS packs every same-depth node together, dozens of lines between parent and child (2026-09-20:
    // Root [1] then 26 unrelated roots before its child [28]; the tree shape falls apart).
    let mut roots_sorted: Vec<&str> = roots.clone();
    roots_sorted.sort_by_key(|id| std::cmp::Reverse(size(id, &kids, &mut size_cache)));

    // Explicit stack for preorder: push children first (reversed so large families come out first)
    let mut stack: Vec<(&str, usize)> = Vec::new();
    for r in roots_sorted.iter().rev() {
        stack.push((r, 0));
    }
    while let Some((id, d)) = stack.pop() {
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let sz = size(id, &kids, &mut size_cache);
        if let Some(r) = by_id.get(id) {
            // Prune: skip non-important leaves/twigs below the size floor (keep the outline from exploding)
            if sz >= min_keep || r.3 || d == 0 {
                out.push(TreeNode {
                    id: id.to_owned(),
                    title: r.2.clone(),
                    depth: d,
                    size: sz,
                });
            }
        }
        if d < max_depth {
            let mut cs: Vec<&str> = kids.get(id).cloned().unwrap_or_default();
            cs.sort_by_key(|k| std::cmp::Reverse(size(k, &kids, &mut size_cache)));
            for c in cs.into_iter().rev() {
                stack.push((c, d + 1));
            }
        }
    }
    out
}

/// Render the tree outline text + number->node map (1-based numbers; indent shows depth).
pub fn render_tree_outline(nodes: &[TreeNode]) -> (String, HashMap<usize, String>) {
    let mut lines = Vec::with_capacity(nodes.len());
    let mut idx = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let num = i + 1;
        idx.insert(num, n.id.clone());
        let ind = "  ".repeat(n.depth);
        let cnt = if n.size > 0 {
            format!(" ({})", n.size)
        } else {
            String::new()
        };
        let t: String = n.title.chars().take(40).collect();
        lines.push(format!("{ind}[{num}] {t}{cnt}"));
    }
    (lines.join("\n"), idx)
}

/// Batch request: judge N entries in one call, answer "parent node number".
/// Prompt uses tree-routing (measured better than linear 23 roots): the outline is the whole tree; pick a **concrete node**.
pub fn build_tree_batch_request(
    outline: &str,
    cases: &[(String, String)],
    model: &str,
    max_chars: usize,
) -> Value {
    let body = cases
        .iter()
        .enumerate()
        .map(|(i, (title, text))| {
            let t: String = text.chars().take(max_chars).collect();
            format!("{}. Title: {title}\n   Body: {t}", (b'A' + i as u8) as char)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let n = cases.len();
    let user = format!(
        "Existing memory-tree nodes (indent=depth, [n]=node, parentheses=descendant count):\n{outline}\n\n\
         Entries to classify:\n{body}\n\n\
         Task: pick the **single best parent node** for each entry (a specific node, not a vague root).\n\
         Output exactly {n} lines, each: <letter>: <number>"
    );
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "You classify memory-tree entries. Follow the instructions. Output strictly in the given format. No explanation, no thinking."},
            {"role": "user", "content": user}
        ],
        "max_tokens": 2048,
        "temperature": 0,
        "thinking": {"type": "disabled"},
    })
}

/// Parse a batch response: `A: 12` lines -> letter index -> node number.
pub fn parse_tree_batch(text: &str) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for cap in text.split(['\n', ';', FULLWIDTH_SEMICOLON]) {
        let line = cap.trim();
        if line.is_empty() {
            continue;
        }
        // First "letter : number" on the line
        let mut letter: Option<usize> = None;
        let mut num: Option<usize> = None;
        for tok in line.split([':', FULLWIDTH_COLON]) {
            let t = tok.trim();
            if letter.is_none() {
                let mut chars = t.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    if c.is_ascii_uppercase() {
                        letter = Some((c as u8 - b'A') as usize);
                    }
                }
            }
            if num.is_none() {
                if let Some(m) = t.split_whitespace().last() {
                    let digits: String = m.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if !digits.is_empty() {
                        num = digits.parse().ok();
                    }
                }
            }
        }
        if let (Some(l), Some(n)) = (letter, num) {
            out.entry(l).or_insert(n);
        }
    }
    out
}

/// ------------- Causal-tree segment rewrite (--causal): parent=cause, child=effect; walk by level -------------
///
/// Why (two 2026-09-20 fixes):
/// 1. Judge the **causal chain** - parent = cause (upstream / first), child = effect (downstream / later);
///    this is not "topic membership"; the AI must judge cause/effect and logical dependency.
/// 2. Sending the whole tree at once is too heavy (2153 entries -> 80k chars / 40k tokens, finish=length truncated);
///    walk **level by level**: each segment = one parent + its direct children.
/// Measured (fslong-hasee store): 4 segments (73/30/29/21 children) in 8s, 4207 tokens, 100 causal-reparent suggestions.

/// Causal-judge system prompt (parent = cause, child = effect).
pub const CAUSAL_SYS: &str = "You tidy memory causal chains. The memory tree is a **causal tree**: \
parent = cause (upstream / happened first), child = effect (downstream / happened later). \
**Prefer fewer edits over forced ones** - only reparent when the direct causal link is solid; topical similarity is not enough. \
Output strictly in the given format. No explanation, no thinking.";

/// Build a "siblings under one parent, causal order" request: those siblings may be each other's cause and need a new chain.
pub fn build_causal_siblings_request(
    parent_short: &str,
    parent_title: &str,
    kids: &[(String, String)],
    model: &str,
) -> Value {
    let lines = kids
        .iter()
        .map(|(s, t)| {
            let tt: String = t.chars().take(44).collect();
            format!("{s}|{tt}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let pt: String = parent_title.chars().take(50).collect();
    let user = format!(
        "Parent (cause): {parent_short}|{pt}\n\n\
         Its {} children (currently all attached under it):\n{lines}\n\n\
         Task: these children may have **direct cause/effect among themselves** - one is another's **direct prior cause**.\n\
         Decide: which children should be **reattached under another child** (because that child is its cause)?\n\n\
         Strict criteria (all three must hold, otherwise **do not output**):\n\
         1. **Same event upstream/downstream**: the latter is the direct reason or prior action of the former, not merely \"same project\";\n\
         2. **The latter is not later in time** (cause first, effect later);\n\
         3. **Without the latter, the former does not stand or cannot be understood**.\n\
         Counter-example (**do not** force-attach): two entries share a topic/project/period but each is an independent event.\n\
         If an entry has no direct causal link to the others, leave it under the current parent - **do not force it**.\n\
         Output only changes, one per line: `child-short-id > new-parent-short-id` (new parent must be one of the children listed above).\n\
         If they are siblings with no direct cause, **output no lines**.",
        kids.len()
    );
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": CAUSAL_SYS},
            {"role": "user", "content": user}
        ],
        "max_tokens": 4096,
        "temperature": 0,
        "thinking": {"type": "disabled"},
    })
}

/// Parse causal-op lines: `short-id > short-id` (tolerate full-width, backticks, prefix/suffix).
pub fn parse_causal_ops(text: &str) -> Vec<(String, String)> {
    let mut ops = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_matches('`').trim();
        if line.is_empty() {
            continue;
        }
        // First `hex > hex` on the line
        let mut it = line.split(|c| c == '>' || c == FULLWIDTH_ARROW);
        if let (Some(l), Some(r)) = (it.next(), it.next()) {
            let l = l.trim().trim_matches('`').trim();
            let r = r.trim().trim_matches('`').trim();
            let lshort: String = l
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_hexdigit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let rshort: String = r.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if lshort.len() >= 6 && rshort.len() >= 6 {
                ops.push((lshort, rshort));
            }
        }
    }
    ops
}

/// Validate causal ops: short ids must resolve uniquely; no self-parent; no cycle (walk the current parent chain).
/// Returns (valid full-id pairs, problem descriptions).
pub fn validate_causal_ops(
    ops: &[(String, String)],
    rows: &[(String, String, String, bool)],
    scope: Option<&std::collections::HashSet<String>>,
) -> (Vec<(String, String)>, Vec<String>) {
    use std::collections::HashMap as Map;
    let mut short2full: Map<String, Option<String>> = Map::new();
    let mut parent_of: Map<String, String> = Map::new();
    for r in rows {
        let s: String = r.0.chars().take(8).collect();
        short2full
            .entry(s)
            .and_modify(|v| *v = None) // short-id collision -> mark unusable
            .or_insert_with(|| Some(r.0.clone()));
        parent_of.insert(r.0.clone(), r.1.clone());
    }
    let resolve = |s: &str| -> Option<String> { short2full.get(s).and_then(|v| v.clone()) };
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let mut seen_child: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (c, p) in ops {
        let (Some(cid), Some(pid)) = (resolve(c), resolve(p)) else {
            bad.push("model returned an unresolved or ambiguous memory ID".to_owned());
            continue;
        };
        if cid == pid {
            bad.push(format!("{} self-parent", &cid[..8.min(cid.len())]));
            continue;
        }
        // no-op (suggested parent is the current parent): drop silently - the AI often lists "stay put", not a problem
        if parent_of.get(&cid).map(|x| x == &pid).unwrap_or(false) {
            continue;
        }
        // Segment constraint: the child being moved must be in this segment (no inventing edits in another segment).
        // The new parent is **not** required to be in this segment - the AI often lists "stay put" (no-op),
        // and looking for a cause on another branch is allowed; out-of-range is caught by the cycle checks below.
        if let Some(sc) = scope {
            if !sc.contains(&cid) {
                bad.push(format!(
                    "{} is not in this segment (cross-segment edit)",
                    &cid[..8.min(cid.len())]
                ));
                continue;
            }
        }
        if !seen_child.insert(cid.clone()) {
            bad.push(format!(
                "{} appears more than once",
                &cid[..8.min(cid.len())]
            ));
            continue;
        }
        // Cycle guard: walk up from pid
        let mut cur = pid.clone();
        let mut steps = 0;
        let mut cyc = false;
        while !cur.is_empty() && steps < 128 {
            if cur == cid {
                cyc = true;
                break;
            }
            cur = parent_of.get(&cur).cloned().unwrap_or_default();
            steps += 1;
        }
        if cyc {
            bad.push(format!(
                "{} -> {} would cycle",
                &cid[..8.min(cid.len())],
                &pid[..8.min(pid.len())]
            ));
            continue;
        }
        ok.push((cid, pid));
    }
    (ok, bad)
}

/// Global cycle check: ops may point at each other (A->B and B->A); per-op validation misses that.
/// Returns (acyclic ops, cycle descriptions).
pub fn detect_global_cycles(
    ops: &[(String, String)],
    rows: &[(String, String, String, bool)],
) -> (Vec<(String, String)>, Vec<String>) {
    use std::collections::HashMap as Map;
    // Simulate the parent chain after apply: current parents, then overlay ops
    let mut parent_of: Map<String, String> =
        rows.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
    for (c, p) in ops {
        parent_of.insert(c.clone(), p.clone());
    }
    let mut bad_children: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut bad = Vec::new();
    for (c, _) in ops {
        // Walk up from c; if we return to c, it is a cycle
        let mut cur = c.clone();
        let mut steps = 0;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            if !seen.insert(cur.clone()) {
                break;
            }
            let next = parent_of.get(&cur).cloned().unwrap_or_default();
            if next.is_empty() {
                break;
            }
            if next == *c {
                bad_children.insert(c.clone());
                bad.push(format!(
                    "{} is in a cycle ({} <-> {}...)",
                    &c[..8.min(c.len())],
                    &c[..8.min(c.len())],
                    &next[..8.min(next.len())]
                ));
                break;
            }
            cur = next;
            steps += 1;
            if steps > 256 {
                break;
            }
        }
    }
    // Drop the whole cycle (leaving any one still cycles)
    let kept: Vec<(String, String)> = ops
        .iter()
        .filter(|(c, _)| !bad_children.contains(c))
        .cloned()
        .collect();
    (kept, bad)
}
