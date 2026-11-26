use super::{Problem, StatusCode, V1Result};
use respire_app::{
    service::{SplitOp, SplitSpec},
    Kind, MemoryEntry,
};
use std::ops::Range;

const PART_CHARS: usize = 2_000;

pub(super) fn build(entry: &MemoryEntry) -> V1Result<SplitSpec> {
    let text = entry.content.as_str();
    if text.trim().chars().take(PART_CHARS + 1).count() <= PART_CHARS {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "split_too_short",
            "Memory must exceed 2000 characters to split",
        ));
    }
    if matches!(
        entry.kind,
        Kind::Hypothesis | Kind::Reflection | Kind::SelfNote
    ) {
        return Err(ineligible());
    }
    let breaks = Breaks::scan(text);
    let mut items = Vec::new();
    let mut summary = String::from("## Outline\n");
    let mut start = 0;
    let last_text_byte = text.trim_end().len();
    while start < text.len() {
        let end = breaks.end(text, start, last_text_byte)?;
        let content = &text[start..end];
        let first = content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .ok_or_else(ineligible)?;
        let number = items.len() + 1;
        let mut title = format!("{number}. ");
        let heading = first.trim_start_matches('#').trim_start();
        let label = if heading.is_empty() { first } else { heading };
        title.extend(label.chars().take(20 - title.len()));
        summary.push_str(&title);
        summary.push('\n');
        items.push(SplitOp {
            title,
            content: content.to_owned(),
            kind: entry.kind.as_str().to_owned(),
            tags: entry.tags.clone(),
            parent: String::new(),
        });
        start = end;
    }
    Ok(SplitSpec {
        summary: Some(summary),
        items,
    })
}

#[derive(Default)]
struct Breaks {
    headings: Vec<usize>,
    paragraphs: Vec<usize>,
    fences: Vec<Range<usize>>,
}

impl Breaks {
    fn scan(text: &str) -> Self {
        let mut breaks = Self::default();
        let mut fence: Option<(usize, u8, usize)> = None;
        let mut paragraph = None;
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let start = offset;
            offset += line.len();
            let line = line.trim_end_matches(['\r', '\n']).trim_end();
            let left = line.trim_start_matches(' ');
            let markdown = line.len() - left.len() <= 3;
            let first = left.as_bytes().first().copied();
            let run = left.bytes().take_while(|byte| Some(*byte) == first).count();
            if let Some((opened, marker, length)) = fence {
                if markdown && first == Some(marker) && run >= length && left[run..].is_empty() {
                    breaks.fences.push(opened..offset);
                    fence = None;
                }
                continue;
            }
            if markdown && matches!(first, Some(b'`' | b'~')) && run >= 3 {
                if let Some(marker) = first {
                    fence = Some((start, marker, run));
                }
                paragraph = None;
            } else if markdown
                && first == Some(b'#')
                && (1..=6).contains(&run)
                && (left[run..].is_empty() || left[run..].starts_with([' ', '\t']))
            {
                breaks.headings.push(start);
                paragraph = None;
            } else if line.trim().is_empty() {
                breaks.paragraphs.push(offset);
                paragraph = None;
            } else if markdown && matches!(first, Some(b'=' | b'-')) && run == left.len() {
                if let Some(start) = paragraph.take() {
                    breaks.headings.push(start);
                }
            } else if paragraph.is_none() {
                paragraph = Some(start);
            }
        }
        if let Some((opened, _, _)) = fence {
            breaks.fences.push(opened..text.len());
        }
        breaks
    }

    fn end(&self, text: &str, start: usize, last_text_byte: usize) -> V1Result<usize> {
        let mut chars = text[start..].char_indices();
        let Some((min, _)) = chars.nth(PART_CHARS / 2) else {
            return Ok(text.len());
        };
        let Some((max, next)) = chars.nth(PART_CHARS / 2 - 1) else {
            return Ok(text.len());
        };
        let min = start + min;
        let mut end = start + max;
        if next == '\n' && text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        if let Some(fence) = self.inside_fence(end) {
            end = fence.start;
        }
        if end <= start {
            return Err(ineligible());
        }
        let max = end.min(last_text_byte - 1);
        Ok(last_break(&self.headings, min, max)
            .or_else(|| last_break(&self.paragraphs, min, max))
            .or_else(|| {
                text[start..end]
                    .char_indices()
                    .rev()
                    .find_map(|(offset, ch)| {
                        let at = start + offset;
                        let splits_crlf =
                            ch == '\n' && at > start && text.as_bytes()[at - 1] == b'\r';
                        (at >= min
                            && at <= max
                            && ch.is_whitespace()
                            && !splits_crlf
                            && self.inside_fence(at).is_none())
                        .then_some(at)
                    })
            })
            .unwrap_or(end))
    }

    fn inside_fence(&self, at: usize) -> Option<&Range<usize>> {
        let preceding = self.fences.partition_point(|range| range.start < at);
        self.fences[..preceding]
            .last()
            .filter(|range| at < range.end)
    }
}

fn last_break(offsets: &[usize], min: usize, max: usize) -> Option<usize> {
    offsets[..offsets.partition_point(|offset| *offset <= max)]
        .last()
        .copied()
        .filter(|offset| *offset >= min)
}

fn ineligible() -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "split_ineligible",
        "Memory cannot be split into bounded sections",
    )
}
