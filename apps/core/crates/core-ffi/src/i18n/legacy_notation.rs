//! Historical punctuation preserved in prompts, anchors, and model-response parsing.

pub(crate) const SEGMENT_LABELS: &str = "【Cause】【Action】【Result】";
pub(crate) const FULLWIDTH_SEMICOLON: char = '；';
pub(crate) const FULLWIDTH_COLON: char = '：';
pub(crate) const FULLWIDTH_ARROW: char = '＞';
