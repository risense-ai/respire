//! share — subtree share: pack a subtree into a prompt an AI can paste.
//!
//! Why (2026-09-21): users must be able to share a subtree — copy produces a prompt,
//! the other side pastes it to an AI, the AI imports. Existing `export/import` is a whole-library JSON file; `tree --material`
//! is Markdown for humans; neither is a cross-account move.
//!
//! Design:
//!   · payload = `1MEMSHARE1:` + base64(gzip(JSON)), plaintext (no in-library ciphertext — the other account's key cannot unwrap it,
//!     so moving ciphertext would not help). gzip only shortens the prompt, it is not encryption; **share is plaintext exposure**.
//!   · prompt = payload + import instructions for the AI (save to disk → see candidates → pick attach → write → verify).
//!   · Import always mints new ids and rebuilds the parent chain (same as `import_json`); the root hangs under --parent or a new same-titled root.
//!   · Attach point is the AI's call: `share-import` without `--parent` only emits the candidate bill (no write),
//!     the AI reads it, picks the attach, then runs with `--parent` or `--go`.

use std::io::{Read, Write};

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Payload prefix (plaintext channel).
pub const PAYLOAD_PREFIX: &str = "1MEMSHARE1:";
/// Suggested on-disk filename (written into the prompt for the AI).
pub const SUGGESTED_FILE: &str = "respire-share.txt";
/// Start/end markers of the payload block in the prompt — the AI takes the text between these two lines.
pub const MARK_BEGIN: &str = "-----BEGIN respire SHARE-----";
pub const MARK_END: &str = "-----END respire SHARE-----";

/// One memory in the payload (plaintext form, no embedding/ciphertext/user).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareItem {
    #[serde(default, flatten)]
    pub metadata: crate::memory::model::MemoryMeta,
    #[serde(default, flatten)]
    pub unknown_fields: crate::memory::model::ExtraFields,
    #[serde(default)]
    pub supersedes: String,
    #[serde(default)]
    pub superseded_by: String,
    #[serde(default)]
    pub see_also: Vec<String>,
    /// Source-library id — only for rebuilding the parent chain inside the payload; import always mints new ids.
    pub id: String,
    /// Parent id inside the payload; empty = root of this subtree (attach point is the importer's).
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub importance: String,
}

/// One subtree share payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePayload {
    pub v: u32,
    /// Payload kind (currently only "subtree"; field kept for later compat).
    pub kind: String,
    #[serde(default)]
    pub root_title: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub source_user: String,
    pub items: Vec<ShareItem>,
}

impl SharePayload {
    /// Total payload body chars (for estimating prompt size).
    pub fn total_chars(&self) -> usize {
        self.items.iter().map(|i| i.content.chars().count()).sum()
    }
}

/// Payload → text (prefix + base64(gzip(JSON))).
pub fn encode(payload: &SharePayload) -> Result<String> {
    let json = serde_json::to_vec(payload)?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&json)?;
    let packed = gz.finish()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&packed);
    Ok(format!("{PAYLOAD_PREFIX}{b64}"))
}

/// Text → payload. Tolerates newlines, spaces, and leading/trailing noise (the AI often saves the whole prompt as a file and feeds it back).
pub fn decode(text: &str) -> Result<SharePayload> {
    let candidates = extract_all(text);
    if candidates.is_empty() {
        // No decent candidate: same guidance as extract
        extract(text)?;
        return Err(anyhow!("payload too short \u{2014} text may have been truncated on copy"));
    }
    let mut last_err = None;
    for body in &candidates {
        match decode_body(body) {
            Ok(p) => return Ok(p),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("payload parse failed")))
}

/// One base64 body → payload.
fn decode_body(body: &str) -> Result<SharePayload> {
    let packed = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|e| anyhow!("payload base64 decode failed: {e}"))?;
    let mut gz = flate2::read::GzDecoder::new(packed.as_slice());
    let mut json = Vec::new();
    gz.read_to_end(&mut json)
        .map_err(|e| anyhow!("payload decompress failed (maybe truncated): {e}"))?;
    let payload: SharePayload =
        serde_json::from_slice(&json).map_err(|e| anyhow!("payload JSON parse failed: {e}"))?;
    if payload.v != 1 {
        return Err(anyhow!(
            "unknown payload version {} (this CLI only accepts v1) \u{2014} upgrade rsrs and retry",
            payload.v
        ));
    }
    if payload.items.is_empty() {
        return Err(anyhow!("payload has no memories \u{2014} the shared subtree may be empty"));
    }
    Ok(payload)
}

/// Take the base64 body after `1MEMSHARE1:`: drop whitespace, stop on illegal chars, and `=` is only allowed as padding
/// (base64 padding can only be at the end — otherwise prose after `…AAA=` would be swallowed; 2026-09-21).
fn take_body(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/') {
            out.push(c);
        } else if c == '=' {
            out.push(c);
        } else {
            break;
        }
    }
    // padding only at the end: truncate after the first '=' and its following '='s
    // (otherwise prose after `…AAA=` would be swallowed; measured 2026-09-21)
    if let Some(i) = out.find('=') {
        let pad = out[i..].chars().take_while(|c| *c == '=').count();
        out.truncate(i + pad);
    }
    out
}

/// Extract a base64 payload body from arbitrary text (strip markers and whitespace).
pub fn extract(text: &str) -> Result<String> {
    let inner = inner_of(text);
    // The prefix may appear more than once: the prompt body mentions it as "after `1MEMSHARE1:` is…",
    // taking only the first hit is misleading (measured 2026-09-21). Collect every candidate and let the caller try each.
    let mut found = false;
    for (idx, _) in inner.match_indices(PAYLOAD_PREFIX) {
        found = true;
        let body = take_body(&inner[idx + PAYLOAD_PREFIX.len()..]);
        if !body.is_empty() {
            return Ok(body);
        }
    }
    if found {
        return Err(anyhow!("payload is empty \u{2014} text may have been truncated on copy"));
    }
    Err(anyhow!(
        "payload not found (missing `{PAYLOAD_PREFIX}` prefix) \u{2014} feed the whole BEGIN/END block from the prompt unchanged"
    ))
}

/// Payload window: if BEGIN/END markers exist, take between them; else the whole text.
fn inner_of(text: &str) -> &str {
    match (text.find(MARK_BEGIN), text.find(MARK_END)) {
        (Some(b), Some(e)) if e > b => &text[b + MARK_BEGIN.len()..e],
        _ => text,
    }
}

/// Every candidate payload body (decode tries each).
pub fn extract_all(text: &str) -> Vec<String> {
    let inner = inner_of(text);
    let mut out = Vec::new();
    for (idx, _) in inner.match_indices(PAYLOAD_PREFIX) {
        let body = take_body(&inner[idx + PAYLOAD_PREFIX.len()..]);
        // Keep only bodies that look like a real payload (base64 length floor) — a prose mention is just "1MEMSHARE1"
        if body.len() >= 40 {
            out.push(body);
        }
    }
    out
}

/// Build the import prompt for the AI: payload + step-by-step instructions (save → candidates → pick attach → write → verify).
pub fn build_prompt(payload: &SharePayload, encoded: &str) -> String {
    let n = payload.items.len();
    let chars = payload.total_chars();
    let root = if payload.root_title.trim().is_empty() {
        "(untitled subtree)".to_owned()
    } else {
        payload.root_title.clone()
    };
    let from = if payload.device.trim().is_empty() {
        String::new()
    } else {
        format!(", from {}", payload.device)
    };
    format!(
        "# Respire subtree share \u{b7} import task

Someone shared a Respire subtree with you: **{root}** ({n} items \u{b7} {chars} chars{from}).
Install it into this machine's Respire library. Follow the steps; do not rewrite memory bodies.

## 1. Ready the CLI

If this machine has no rsrs yet: `npm i -g rsrs-cli` (or `cargo build --release -p respire` from source).
Skip if you already have it; `rsrs status` checks. If the library was never initialized, start local-only with `rsrs keygen --pass <password>`.

## 2. Save the payload

Save the **whole** text between BEGIN / END below, unchanged, as `{file}` (keep the `{prefix}` prefix; do not wrap or split):

{begin}
{encoded}
{end}

## 3. See attach candidates (judge first; this step does not write)

```
rsrs share-import {file}
```

This command only prints a candidate bill: nearby existing entries proposed by Core. **Read it, then decide** \u{2014}
whether this subtree hangs under an existing memory, or becomes its own root.

## 4. Write (pick one)

- **A close candidate exists** \u{2192} attach under it (use the 8-char short id from the bill):
  ```
  rsrs share-import {file} --parent <candidate-id> --go
  ```
- **No close candidate** \u{2192} create a same-titled root (title `{root}`) and hang the tree under it:
  ```
  rsrs share-import {file} --go
  ```

`--go` is the write switch; without it you only ever get a bill. Import always mints new ids; a repeat import will not overwrite old rows.

**Conflict gate**: if your library already has Core-reported same-topic rows, `--go` without `--parent` is **refused** and lists conflicts \u{2014}
this is the hard gate against sibling same-topic. Then pick one: (1) attach under the same-topic row (`--parent <short-id>`);
(2) import with `--go --force`, then `remember \"<merged>\" --merge-ids \"<old-id>,<new-id>\"` into one;
(3) confirm it is a new fact that should sit as a sibling, and force with `--force`.

## 5. Verify

```
rsrs tree --from <new-root-id> --depth 2
rsrs recall \"{root}\" --limit 3
```

If the tree is in place and recall sees it, it is installed. If you made a new root, you can still `rsrs attach <new-root-id> --parent <better-parent>` to move it.

## Appendix: what the payload is

After `{prefix}` is gzip-compressed JSON (base64). It holds this subtree's plaintext bodies and parent links.
**It is plaintext** \u{2014} treat both ends of a share as public; do not send a subtree that holds keys or private data.
",
        file = SUGGESTED_FILE,
        prefix = PAYLOAD_PREFIX,
        begin = MARK_BEGIN,
        end = MARK_END,
        encoded = encoded,
    )
}

/// Subtree entries → payload items (given walk order; a parent outside the subtree is emptied so no orphans).
///
/// `lookup` fetches a plaintext entry by id (caller decrypts); a miss is skipped (a bad ciphertext row must not block share).
pub fn items_from(
    root_id: &str,
    order: &[String],
    lookup: &dyn Fn(&str) -> Option<crate::memory::model::MemoryEntry>,
    members: &std::collections::HashSet<String>,
) -> Vec<ShareItem> {
    let mut out = Vec::with_capacity(order.len());
    for id in order {
        let Some(e) = lookup(id) else { continue };
        // Keep the chain only if the parent is in the subtree; the root and an outside parent are emptied (attach is the importer's)
        let parent = if id == root_id || !members.contains(&e.parent_id) {
            String::new()
        } else {
            e.parent_id.clone()
        };
        out.push(ShareItem {
            metadata: e.metadata.clone(),
            unknown_fields: e.unknown_fields.clone(),
            supersedes: if members.contains(&e.supersedes) { e.supersedes.clone() } else { String::new() },
            superseded_by: if members.contains(&e.superseded_by) { e.superseded_by.clone() } else { String::new() },
            see_also: e.see_also.iter().filter(|id| members.contains(*id)).cloned().collect(),
            id: e.id.clone(),
            parent_id: parent,
            kind: e.kind.as_str().to_owned(),
            tags: e.tags.clone(),
            title: e.title.clone(),
            content: e.content.clone(),
            created_at: e.created_at.clone(),
            updated_at: e.updated_at.clone(),
            importance: if e.importance.trim().is_empty() {
                "trivial".to_owned()
            } else {
                e.importance.clone()
            },
        });
    }
    out
}

/// Payload parent chain → new-library id map. Returns (final parent id per item, rebuilt count, orphan count).
///
/// If `attach_parent` is non-empty, the payload root hangs under it; if empty the root is its own root (parent stays empty).
/// Cycles and duplicate ids always error — refuse the import rather than write a broken tree.
pub fn remap_parents(
    items: &[ShareItem],
    new_ids: &[String],
    attach_parent: &str,
) -> Result<(Vec<String>, usize, usize)> {
    if items.len() != new_ids.len() {
        return Err(anyhow!("internal error: new id count does not match item count"));
    }
    let mut old_index = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        if it.id.is_empty() {
            return Err(anyhow!("payload item {} missing id", i + 1));
        }
        if old_index.insert(it.id.as_str(), i).is_some() {
            return Err(anyhow!("duplicate id in payload: {}", it.id));
        }
    }
    // Parent-chain cycle check: iterate so a deep tree cannot overflow the stack
    let mut done = std::collections::HashSet::new();
    for start in 0..items.len() {
        let mut chain = std::collections::HashSet::new();
        let mut cursor = Some(start);
        while let Some(i) = cursor {
            if done.contains(&i) {
                break;
            }
            if !chain.insert(i) {
                return Err(anyhow!("payload parent chain contains a cycle \u{2014} import refused"));
            }
            cursor = old_index.get(items[i].parent_id.as_str()).copied();
        }
        done.extend(chain);
    }
    let mut parents = Vec::with_capacity(items.len());
    let mut reattached = 0usize;
    let mut orphaned = 0usize;
    for it in items.iter() {
        if it.parent_id.is_empty() {
            parents.push(attach_parent.to_owned());
            continue;
        }
        match old_index.get(it.parent_id.as_str()) {
            Some(j) => {
                parents.push(new_ids[*j].clone());
                reattached += 1;
            }
            None => {
                // Payload named a parent that is not in the payload (truncated/hand-edited) — treat as a root, do not hard-bind someone else's id
                parents.push(attach_parent.to_owned());
                orphaned += 1;
            }
        }
    }
    Ok((parents, reattached, orphaned))
}

/// Semantic query string for the AI attach-point pick: root title + each title + root body head.
pub fn mount_query(payload: &SharePayload) -> String {
    let mut s = payload.root_title.clone();
    for it in payload.items.iter().take(30) {
        if !it.title.trim().is_empty() {
            s.push('\n');
            s.push_str(&it.title);
        }
    }
    if let Some(root) = payload.items.iter().find(|i| i.parent_id.is_empty()) {
        let head: String = root.content.chars().take(300).collect();
        s.push('\n');
        s.push_str(&head);
    }
    s
}

#[cfg(test)]
mod tests;
