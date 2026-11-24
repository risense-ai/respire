#[path = "fixtures/i18n/legacy_text.rs"]
mod legacy_text;

use anyhow::{Context, Result};
use respire_core::engine::build_tree_meta;
use respire_core::model::MemoryEntry;
use respire_core::query::{ancestor_budget_split, MemoryEngine, QuerySettings, ANCESTOR_PARENT_CAP};
use respire_core::search::HashingEmbedder;
use respire_core::{MemoryQuery, StoredMemory};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const WIRE: &str = include_str!("fixtures/legacy_stored_memory.json");
const PAYLOAD: &str = include_str!("fixtures/legacy_payload_v2.json");
const VECTOR: &[u8] = include_bytes!("fixtures/legacy_embedding.f32le");

// Fixture-only shape: intentionally not part of Core's runtime protocol.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HistoricalEnvelope<'a> {
    id: &'a str,
    user: &'a str,
    ciphertext: &'a str,
    nonce: &'a str,
    embedding_enc: &'a str,
    updated_at: &'a str,
    deleted: bool,
}

#[test]
fn historical_wire_has_only_the_outer_envelope_fields() -> Result<()> {
    let record: HistoricalEnvelope<'_> = serde_json::from_str(WIRE)?;
    let expected: Value = serde_json::from_str(WIRE)?;
    assert_eq!(serde_json::to_value(&record)?, expected);
    assert_eq!(record.nonce, "000102030405060708090a0b");
    let (nonce, ciphertext) = record
        .embedding_enc
        .split_once(':')
        .context("legacy embedding fixture uses nonce:ciphertext")?;
    assert_eq!(nonce, "0c0d0e0f1011121314151617");
    assert_eq!(ciphertext.len(), (VECTOR.len() + 16) * 2); // 16-byte GCM tag
    assert_eq!(record.ciphertext.len(), (PAYLOAD.len() + 16) * 2);
    Ok(())
}

#[test]
fn historical_payload_and_vector_capture_stay_stable() -> Result<()> {
    let payload: Value = serde_json::from_str(PAYLOAD)?;
    assert_eq!(payload["title"], "Legacy fixture");
    assert!(payload.get("parent_id").is_none());
    assert!(payload.get("emotion").is_none());
    // Compile-time capture, not a replacement decoder or a runtime file read.
    assert_eq!(VECTOR, &[0, 0, 128, 62, 0, 0, 0, 191, 0, 0, 128, 63]);
    Ok(())
}

#[test]
fn tree_depth_cascades_across_cached_parent_links() {
    let stored = |id: &str, parent: &str| {
        let mut record = StoredMemory::new_pending(id.into());
        record.local_parent_id = parent.into();
        record
    };
    let rows = [
        stored("deep", "leaf"),
        stored("leaf", "mid"),
        stored("mid", "root"),
        stored("root", ""),
    ];
    for refs in [
        rows.iter().collect::<Vec<_>>(),
        rows.iter().rev().collect::<Vec<_>>(),
    ] {
        let tree = build_tree_meta(&refs);
        assert_eq!(tree["root"].0, 0);
        assert_eq!(tree["mid"].0, 1);
        assert_eq!(tree["leaf"].0, 2);
        assert_eq!(tree["deep"].0, 3);
    }
}

#[test]
fn ancestor_budget_preserves_root_floor_and_respects_lengths() {
    let caps = ancestor_budget_split(&[1000; 7], 800, 200);
    assert_eq!(caps.len(), 7);
    assert_eq!(caps.iter().sum::<usize>(), 800);
    assert!(caps[0] > 0);
    assert!(caps[6] <= ANCESTOR_PARENT_CAP);
    assert!(caps[..5].iter().sum::<usize>() >= 200);
    let short_root = ancestor_budget_split(&[10, 1000, 1000, 1000, 1000, 1000, 1000], 800, 200);
    assert_eq!(short_root[0], 10);
    assert_eq!(short_root.iter().sum::<usize>(), 800);
    assert!(short_root[5] > 200);
    assert!(short_root[6] <= ANCESTOR_PARENT_CAP);
    assert_eq!(ancestor_budget_split(&[500, 500], 300, 200), [0, 300]);
    assert_eq!(ancestor_budget_split(&[1000; 7], 800, 0)[0], 0);
    assert_eq!(ancestor_budget_split(&[30, 50, 40, 20], 5000, 200), [30, 50, 40, 20]);
}

#[test]
fn contextual_recall_reads_host_entries_and_applies_explicit_budget() -> Result<()> {
    let mut entries = HashMap::new();
    let mut records = Vec::new();
    for (id, parent, date, content) in [
        ("root", "", "2026-01-01T00:00:00Z", legacy_text::ROOT_TEXT),
        ("mid", "root", "2026-01-02T00:00:00Z", legacy_text::MIDDLE_TEXT),
        ("leaf", "mid", "2026-01-03T00:00:00Z", legacy_text::LEAF_TEXT),
    ] {
        let mut record = StoredMemory::new_pending(id.into());
        record.local_parent_id = parent.into();
        records.push(record);
        let entry: MemoryEntry = serde_json::from_value(json!({
            "id": id, "parent_id": parent, "kind": "Knowledge", "title": id,
            "tags": [], "content": content, "user": "fixture", "computer": "",
            "project": "", "created_at": date, "updated_at": date, "emotion": -1.0
        }))?;
        entries.insert(id.into(), entry);
    }

    let embedder = HashingEmbedder::default();
    let query = MemoryQuery::new("").limit(1);
    let mut settings = QuerySettings::default();
    settings.ancestor_budget = 100;
    settings.ancestor_root_floor = 0;
    let ample =
        MemoryEngine::recall_local_contextual(&entries, &embedder, &records, &query, &settings)?;
    let hit = ample.first().context("expected most recent entry")?;
    assert_eq!(hit.entry.id, "leaf");
    assert_eq!(
        hit.ancestors.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["root", "mid"]
    );

    settings.ancestor_budget = 3;
    let capped =
        MemoryEngine::recall_local_contextual(&entries, &embedder, &records, &query, &settings)?;
    let hit = capped.first().context("expected capped entry")?;
    assert_eq!(hit.ancestors[0].content, legacy_text::ABBREVIATED_ROOT);
    assert_eq!(hit.ancestors[1].content, legacy_text::MIDDLE_TEXT);

    settings.ancestor_budget = 0;
    let disabled =
        MemoryEngine::recall_local_contextual(&entries, &embedder, &records, &query, &settings)?;
    assert!(disabled
        .first()
        .context("expected entry without context")?
        .ancestors
        .is_empty());
    Ok(())
}
