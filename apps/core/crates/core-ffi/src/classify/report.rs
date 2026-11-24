//! Classification summaries and human-readable tables.

use super::Verdict;
use unicode_width::UnicodeWidthStr;

/// Result for one entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemResult {
    pub id: String,
    pub title: String,
    /// Current top-level (category title; empty = no parent / broken chain).
    pub current_root: String,
    pub verdict: Option<Verdict>,
    pub error: Option<String>,
    /// Suggested parent from tree mode (id / title / its top-level root) - filled only with --tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest_parent: Option<(String, String, String)>,
}

/// Structured classify report consumed by the CLI renderer or a JSON envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassifyReport {
    pub command: String,
    pub status: String,
    pub summary: ReportSummary,
    pub items: Vec<ReportItem>,
    pub actions: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportSummary {
    pub total: usize,
    pub matched: usize,
    pub mismatch: usize,
    pub unrooted: usize,
    pub low_confidence: usize,
    pub failed: usize,
    pub min_confidence: f32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportItem {
    pub id: String,
    pub class: String,
    pub status: String,
    pub current: String,
    pub choice: String,
    pub confidence: Option<f32>,
    pub title: String,
    pub error: Option<String>,
}

/// Build a structured report. Classification itself remains read-only.
pub fn build_report(items: &[ItemResult], min_confidence: f32) -> ClassifyReport {
    let short = |id: &str| id.chars().take(8).collect::<String>();
    let mut mismatch: Vec<&ItemResult> = Vec::new();
    let mut unrooted: Vec<&ItemResult> = Vec::new();
    let mut low: Vec<&ItemResult> = Vec::new();
    let mut failed: Vec<&ItemResult> = Vec::new();
    let mut ok = 0usize;
    for it in items {
        let Some(v) = it.verdict.as_ref() else {
            failed.push(it);
            continue;
        };
        if it.error.is_some() {
            failed.push(it);
            continue;
        }
        if v.confidence < min_confidence {
            low.push(it);
            continue;
        }
        if it.current_root.is_empty() {
            unrooted.push(it); // no current parent (loose root / broken chain) - JEV's category is the suggested attach
            continue;
        }
        if v.choice != it.current_root {
            mismatch.push(it);
        } else {
            ok += 1;
        }
    }
    let status = if !failed.is_empty() {
        "fail"
    } else if !mismatch.is_empty() || !unrooted.is_empty() || !low.is_empty() {
        "warn"
    } else {
        "ok"
    };
    let mut report_items =
        Vec::with_capacity(mismatch.len() + unrooted.len() + low.len() + failed.len());
    for (class, group, item_status) in [
        ("MISMATCH", mismatch.as_slice(), "warn"),
        ("UNROOTED", unrooted.as_slice(), "warn"),
        ("LOW_CONFIDENCE", low.as_slice(), "warn"),
        ("FAILED", failed.as_slice(), "fail"),
    ] {
        for it in group {
            let verdict = it.verdict.as_ref();
            report_items.push(ReportItem {
                id: short(&it.id),
                class: class.to_owned(),
                status: item_status.to_owned(),
                current: it.current_root.clone(),
                choice: verdict.map(|v| v.choice.clone()).unwrap_or_default(),
                confidence: verdict.map(|v| v.confidence),
                title: it.title.clone(),
                error: it.error.clone(),
            });
        }
    }
    ClassifyReport {
        command: "classify".to_owned(),
        status: status.to_owned(),
        summary: ReportSummary {
            total: items.len(),
            matched: ok,
            mismatch: mismatch.len(),
            unrooted: unrooted.len(),
            low_confidence: low.len(),
            failed: failed.len(),
            min_confidence,
        },
        items: report_items,
        actions: Vec::new(),
        errors: failed.iter().filter_map(|it| it.error.clone()).collect(),
    }
}

fn clean_cell(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| clean_cell(cell)).collect())
        .collect();
    for row in &rows {
        for (index, cell) in row.iter().enumerate().take(widths.len()) {
            widths[index] = widths[index].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let format_row = |row: &[String]| {
        row.iter()
            .enumerate()
            .take(widths.len())
            .map(|(index, cell)| {
                format!(
                    "{cell}{}",
                    " ".repeat(widths[index].saturating_sub(UnicodeWidthStr::width(cell.as_str())))
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let header = headers.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>();
    let separator = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>()
        .join("  ");
    let mut output = vec![format_row(&header), separator];
    output.extend(rows.iter().map(|row| format_row(row)));
    output.join("\n")
}

/// Render a compact, icon-free text report.
pub fn render_report(items: &[ItemResult], min_confidence: f32) -> String {
    let report = build_report(items, min_confidence);
    let summary = &report.summary;
    let mut output = render_table(
        &[
            "COMMAND",
            "STATUS",
            "TOTAL",
            "MATCH",
            "MISMATCH",
            "UNROOTED",
            "LOW_CONFIDENCE",
            "FAILED",
            "THRESHOLD",
        ],
        &[vec![
            report.command.clone(),
            report.status.to_uppercase(),
            summary.total.to_string(),
            summary.matched.to_string(),
            summary.mismatch.to_string(),
            summary.unrooted.to_string(),
            summary.low_confidence.to_string(),
            summary.failed.to_string(),
            format!("{:.2}", summary.min_confidence),
        ]],
    );
    if !report.items.is_empty() {
        let rows = report
            .items
            .iter()
            .map(|item| {
                vec![
                    item.id.clone(),
                    item.class.clone(),
                    item.status.to_uppercase(),
                    item.current.clone(),
                    item.choice.clone(),
                    item.confidence
                        .map(|v| format!("{v:.2}"))
                        .unwrap_or_default(),
                    item.title.clone(),
                    item.error.clone().unwrap_or_default(),
                ]
            })
            .collect::<Vec<_>>();
        output.push_str("\n\n");
        output.push_str(&render_table(
            &[
                "ID",
                "CLASS",
                "STATUS",
                "CURRENT",
                "CHOICE",
                "CONFIDENCE",
                "TITLE",
                "ERROR",
            ],
            &rows,
        ));
    }
    output
}
