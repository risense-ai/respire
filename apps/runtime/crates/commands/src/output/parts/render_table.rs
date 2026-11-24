/// Render a compact, dependency-free table for TTY and redirected text output.
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let rows = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    cell.chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut widths: Vec<usize> = headers.iter().map(|h| UnicodeWidthStr::width(*h)).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate().take(widths.len()) {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let line = |row: &[String]| {
        row.iter()
            .enumerate()
            .take(widths.len())
            .map(|(i, cell)| {
                format!(
                    "{cell}{}",
                    " ".repeat(widths[i].saturating_sub(UnicodeWidthStr::width(cell.as_str())))
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let head = headers.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let mut out = vec![line(&head)];
    out.extend(rows.iter().map(|row| line(row)));
    out.join("\n")
}

pub fn status_from_items(items: &[Item]) -> Status {
    if items.iter().any(|i| matches!(i.status, Status::Fail)) {
        Status::Fail
    } else if items
        .iter()
        .any(|i| matches!(i.status, Status::Warn | Status::Pending))
    {
        Status::Warn
    } else {
        Status::Ok
    }
}

pub fn status_label(status: Status) -> &'static str {
    crate::i18n::chrome(match status {
        Status::Ok => "pass",
        Status::Warn => "warn",
        Status::Fail => "fail",
        Status::Skip => "skip",
        Status::Pending => "pending",
    })
}

#[cfg(test)]
mod tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/output/tests.rs")); }
