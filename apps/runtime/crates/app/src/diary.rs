//! Explicit, bounded diary retrieval. Diary matches never enter normal recall.
//! Only derived opaque Core artifacts are persisted; original encrypted records stay intact.
use crate::core_sdk::{self, search::Embedder, Snapshot};
use crate::transport::local::LocalStore;
use crate::{MemoryEngine, MemoryEntry, MemoryQuery, MemoryTransport, SessionKeys, StoredMemory};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[path = "diary_tests.rs"]
mod tests;

#[path = "diary_fragments.rs"]
mod fragments;
#[path = "diary_candidate.rs"]
mod candidate;
pub use candidate::{
    candidate_cache_path, candidate_needs_rebuild, rebuild_candidate_with_progress,
};
const LIMIT_MAX: usize = 20;
const BUDGET_MAX: usize = 12000;
const CACHE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS diary_sources(id TEXT PRIMARY KEY, source TEXT NOT NULL); CREATE TABLE IF NOT EXISTS diary_artifacts(id TEXT PRIMARY KEY, source_id TEXT NOT NULL, generation TEXT NOT NULL, artifact BLOB NOT NULL)";

#[derive(Clone, Serialize)]
pub struct Fragment {
    pub source_kind: &'static str,
    pub evidence_state: &'static str,
    pub source_id: String,
    pub fragment_id: String,
    pub source_version: String,
    pub date: String,
    pub title: String,
    pub project: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Ranking only, not credibility or factual confidence.
    pub score: f32,
    pub truncated: bool,
    #[serde(skip)]
    anchor: usize,
    #[serde(skip)]
    index_id: String,
}

#[derive(Serialize)]
pub struct FragmentResult {
    pub items: Vec<Fragment>,
    pub semantic_ready: bool,
    pub pending_fragments: usize,
    pub truncated: bool,
    pub max_chars: usize,
    pub offset_unit: &'static str,
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn cache_path() -> std::path::PathBuf {
    let root = crate::service::data_dir();
    // Existing library directories may have legitimate aliases (macOS /var).
    // Resolve only the directory; SQLite NOFOLLOW still protects the cache file.
    root.canonicalize()
        .unwrap_or(root)
        .join("diary-fragments-v1.db")
}

fn model_cache_path(model: &str) -> Result<std::path::PathBuf> {
    if core_sdk::profiles::current()?.is_none() { return Ok(cache_path()); }
    let generation = core_sdk::generation_key(model)?;
    candidate_cache_path(&generation)
}
/// Existing target diary state stays with the established model switch path.
pub fn has_candidate_diary_cache(generation: &str) -> Result<bool> {
    Ok(candidate_cache_path(generation)?.try_exists()?)
}
fn model_cache(model: &str, readonly: bool) -> Result<Option<Connection>> {
    cache_at(&model_cache_path(model)?, readonly)
}
#[derive(Debug)]
struct DamagedCache;
impl std::fmt::Display for DamagedCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("derived diary cache needs reconstruction")
    }
}
impl std::error::Error for DamagedCache {}
fn schema_error(error: rusqlite::Error) -> anyhow::Error {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code == 1 => DamagedCache.into(),
        _ => error.into(),
    }
}
fn cache_at(path: &std::path::Path, readonly: bool) -> Result<Option<Connection>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "unsafe diary cache path"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                anyhow::ensure!(
                    metadata.nlink() == 1,
                    "linked diary cache is not writable derived state"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if readonly {
                return Ok(None);
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_NOFOLLOW
            | if readonly {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            } else {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            },
    )?;
    db.busy_timeout(std::time::Duration::from_millis(200))?;
    // Refuse an unrelated database before any schema write, even at this name.
    let unknown:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('diary_sources','diary_artifacts')",[],|r|r.get(0))?;
    anyhow::ensure!(
        unknown == 0,
        "unexpected database at diary cache path; refusing mutation"
    );
    if !readonly {
        db.execute_batch(CACHE_SCHEMA)?;
    }
    db.prepare("SELECT id,source FROM diary_sources LIMIT 0")
        .map_err(schema_error)?;
    db.prepare("SELECT id,source_id,generation,artifact FROM diary_artifacts LIMIT 0")
        .map_err(schema_error)?;
    let invalid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_sources WHERE typeof(id)!='text' OR typeof(source)!='text') OR EXISTS(SELECT 1 FROM diary_artifacts WHERE typeof(id)!='text' OR typeof(source_id)!='text' OR typeof(generation)!='text' OR typeof(artifact)!='blob')",[],|r|r.get(0))?;
    if invalid {
        return Err(DamagedCache.into());
    }
    Ok(Some(db))
}
#[cfg(test)]
fn cache(readonly: bool) -> Result<Option<Connection>> {
    cache_at(&cache_path(), readonly)
}
fn writable_cache(path: &std::path::Path) -> Result<Connection> {
    match cache_at(path, false) {
        Ok(Some(db)) => Ok(db),
        Ok(None) => bail!("derived diary cache unavailable"),
        Err(error) => {
            let damaged=error.is::<DamagedCache>() || error.downcast_ref::<rusqlite::Error>().is_some_and(|error|
                matches!(error,rusqlite::Error::SqliteFailure(code,_) if matches!(code.code,rusqlite::ErrorCode::DatabaseCorrupt|rusqlite::ErrorCode::NotADatabase)));
            if !damaged {
                return Err(error);
            }
            // Only discardable derived state is moved; original bytes remain for diagnosis.
            let quarantine = path.with_file_name(format!(
                "diary-fragments-corrupt-{}.db",
                uuid::Uuid::new_v4()
            ));
            std::fs::rename(path, quarantine)?;
            cache_at(path, false)?.context("cannot reconstruct diary cache")
        }
    }
}

fn diaries(store: &LocalStore) -> Result<Vec<StoredMemory>> {
    Ok(store
        .all(false)?
        .into_iter()
        .filter(|m| m.local_importance == "trivial")
        .collect())
}
fn version(m: &StoredMemory) -> String {
    digest(&format!("{}:{}", m.nonce, m.ciphertext))
}
fn day(entry: &MemoryEntry) -> String {
    chrono::DateTime::parse_from_rfc3339(&entry.created_at)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|_| entry.created_at.chars().take(10).collect())
}
fn piece(
    entry: &MemoryEntry,
    source_version: &str,
    chars: &[char],
    start: usize,
    end: usize,
) -> Fragment {
    let text: String = chars[start..end].iter().collect();
    // Embedding identity is stable for unchanged text/metadata after an append.
    let key = digest(&format!(
        "{}:{start}:{text}:{}:{}:{:?}:{}:{:?}",
        entry.id, entry.title, entry.project, entry.tags, entry.computer, entry.kind
    ));
    // A public locator additionally binds the exact source revision and range.
    let locator = digest(&format!("{key}:{source_version}:{end}"));
    Fragment {
        source_kind: "diary",
        evidence_state: "reference",
        source_id: entry.id.clone(),
        fragment_id: format!("d1.{start}.{end}.{}", &locator[..32]),
        index_id: key[..32].into(),
        source_version: source_version.into(),
        date: day(entry),
        title: entry.title.chars().take(80).collect(),
        project: entry.project.chars().take(80).collect(),
        start,
        end,
        text,
        score: 0.0,
        truncated: false,
        anchor: 0,
    }
}
fn split(entry: &MemoryEntry, source_version: &str) -> Vec<Fragment> {
    let chars: Vec<char> = entry.content.chars().collect();
    fragments::ranges(&entry.content)
        .into_iter()
        .map(|(start, end)| piece(entry, source_version, &chars, start, end))
        .collect()
}
fn indexed_entry(source: &MemoryEntry, fragment: &Fragment) -> MemoryEntry {
    // Never clone a whole day's plaintext to immediately replace its content.
    MemoryEntry {
        metadata: source.metadata.clone(),
        unknown_fields: source.unknown_fields.clone(),
        id: format!("diary-fragment:{}", fragment.index_id),
        content: fragment.text.clone(),
        kind: source.kind,
        tags: source.tags.clone(),
        title: source.title.clone(),
        user: source.user.clone(),
        computer: source.computer.clone(),
        project: source.project.clone(),
        created_at: source.created_at.clone(),
        updated_at: source.updated_at.clone(),
        emotion: source.emotion,
        importance: source.importance.clone(),
        device: source.device.clone(),
        modified_by: source.modified_by.clone(),
        parent_id: String::new(),
        supersedes: String::new(),
        superseded_by: String::new(),
        see_also: Vec::new(),
    }
}

/// Match the authoritative full text, not the lossy indexing windows. Map folded
/// UTF-8 positions back to original Unicode scalars, including expanded casing.
fn lexical(entry: &MemoryEntry, revision: &str, terms: &[String]) -> (Vec<Fragment>, bool) {
    let folded = entry.content.to_lowercase();
    let title = entry.title.to_lowercase();
    let project = entry.project.to_lowercase();
    if !terms
        .iter()
        .all(|term| title.contains(term) || project.contains(term) || folded.contains(term))
    {
        return (Vec::new(), false);
    }
    let mut omitted = false;
    let spans: Vec<_> = terms
        .iter()
        .flat_map(|term| {
            let mut positions: Vec<_> = folded
                .match_indices(term.as_str())
                .take(LIMIT_MAX + 1)
                .map(|(at, _)| (at, at + term.len()))
                .collect();
            omitted |= positions.len() > LIMIT_MAX;
            positions.truncate(LIMIT_MAX);
            positions
        })
        .collect();
    let mut mapped = vec![(0, 0); spans.len()];
    let mut points: Vec<_> = spans
        .iter()
        .enumerate()
        .flat_map(|(i, &(a, b))| [(a, i, false), (b - 1, i, true)])
        .collect();
    points.sort_unstable();
    let chars: Vec<char> = entry.content.chars().collect();
    let (mut cursor, mut next) = (0, 0);
    for (original, ch) in chars.iter().enumerate() {
        cursor += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        while next < points.len() && points[next].0 < cursor {
            let (_, i, end) = points[next];
            if end {
                mapped[i].1 = original + 1;
            } else {
                mapped[i].0 = original;
            }
            next += 1;
        }
    }
    mapped.sort_unstable();
    mapped.dedup();
    if mapped.is_empty() {
        mapped.push((0, 0));
    }
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for (start, end) in mapped {
        if let Some(last) = groups
            .last_mut()
            .filter(|last| end.max(last.1) - last.0 <= fragments::CHUNK_CHARS)
        {
            last.1 = last.1.max(end);
        } else {
            groups.push((start, end));
        }
    }
    omitted |= groups.len() > LIMIT_MAX;
    let hits = groups
        .into_iter()
        .take(LIMIT_MAX)
        .map(|(at, end)| {
            let start = at.saturating_sub((fragments::CHUNK_CHARS - (end - at)) / 3);
            let stop = (start + fragments::CHUNK_CHARS).min(chars.len());
            let mut hit = piece(entry, revision, &chars, start, stop);
            hit.anchor = at - start;
            hit.score = 2.0;
            hit
        })
        .collect();
    (hits, omitted)
}

fn select(mut hits: Vec<Fragment>, limit: usize) -> Vec<Fragment> {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.date.cmp(&a.date))
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.start.cmp(&b.start))
    });
    let mut selected: Vec<Fragment> = Vec::new();
    for hit in hits {
        if !selected.iter().any(|other| {
            other.source_id == hit.source_id && hit.start < other.end && other.start < hit.end
        }) {
            selected.push(hit);
            if selected.len() == limit {
                break;
            }
        }
    }
    selected
}

pub fn needs_rebuild(store: &LocalStore) -> Result<bool> {
    let sources = diaries(store)?;
    let result = (|| -> Result<bool> {
        let model = core_sdk::profiles::current()?.map(|s| s.model).unwrap_or(store.retrieval_model()?);
        let Some(db) = model_cache(&model, true)? else {
            return Ok(!sources.is_empty());
        };
        cache_needs_rebuild(&db, &sources, || core_sdk::generation_key(&model))
    })();
    // Broken or incompatible derived state requests rebuild, never blocks original text.
    Ok(result.unwrap_or(true))
}

fn cache_needs_rebuild(
    db: &Connection,
    sources: &[StoredMemory],
    generation: impl FnOnce() -> Result<String>,
) -> Result<bool> {
    let tx = db.unchecked_transaction()?;
    let cached = {
        let mut statement = tx.prepare("SELECT id,source FROM diary_sources")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
    };
    if cached.len() != sources.len()
        || sources.iter().any(|source| cached.get(&source.id) != Some(&version(source)))
    {
        return Ok(true);
    }
    let artifacts = {
        let mut statement = tx.prepare("SELECT source_id,generation,artifact FROM diary_artifacts")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    tx.commit()?;
    if artifacts.is_empty() {
        return Ok(false);
    }
    let generation = generation()?;
    if artifacts.iter().any(|(id, key, _)| !cached.contains_key(id) || key != &generation) {
        return Ok(true);
    }
    let artifacts: Vec<_> = artifacts.into_iter().map(|(_, _, artifact)| artifact).collect();
    Ok(!core_sdk::index_ready(&artifacts)?)
}

include!("diary/generation.rs");
include!("diary/queries.rs");
