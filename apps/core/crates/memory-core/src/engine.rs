//! Private query helpers. Ciphertext orchestration has moved to public crates.
use anyhow::Result;
use crate::model::StoredMemory;
pub fn build_tree_meta(list: &[&StoredMemory]) -> std::collections::HashMap<String, (usize, i64)> {
    use std::collections::HashMap;
    let stamp_ms = |s: &StoredMemory| -> i64 {
        let raw = if s.local_created_at.is_empty() {
            s.updated_at.as_str()
        } else {
            s.local_created_at.as_str()
        };
        chrono::DateTime::parse_from_rfc3339(raw)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0)
    };
    let by_id: HashMap<&str, &StoredMemory> = list.iter().map(|s| (s.id.as_str(), *s)).collect();
    // Depth (memoized walk up)
    let mut depth: HashMap<String, usize> = HashMap::new();
    for s in list {
        if depth.contains_key(&s.id) {
            continue;
        }
        let mut chain: Vec<&StoredMemory> = Vec::new();
        let mut cur = Some(*s);
        // d = depth of the last chain element. On a cache hit that element is the child of the cached node (v+1);
        // at the root, 0. Used to take cached v as the last element's depth — off-by-one, deep branches collapsed to zero.
        let d = loop {
            match cur {
                Some(m) => {
                    if let Some(v) = depth.get(&m.id) {
                        break v + 1;
                    }
                    chain.push(m);
                    if chain.len() > list.len() + 1 {
                        break 0; // cycle floor
                    }
                    match by_id.get(m.local_parent_id.as_str()).copied() {
                        Some(p) => cur = Some(p),
                        // m has no parent in the set — m is a root, depth 0
                        None => break 0,
                    }
                }
                None => break 0,
            }
        };
        let mut run = d;
        for m in chain.into_iter().rev() {
            depth.insert(m.id.clone(), run);
            run += 1;
        }
    }
    // Subtree latest (aggregate from leaves: process depth desc, leaves first, parent takes children)
    let mut latest: HashMap<String, i64> = HashMap::new();
    let mut order: Vec<&StoredMemory> = list.to_vec();
    order.sort_by_key(|s| std::cmp::Reverse(depth.get(&s.id).copied().unwrap_or(0)));
    let mut children_latest: HashMap<&str, i64> = HashMap::new();
    for s in &order {
        let own = stamp_ms(s);
        let child_max = children_latest.get(s.id.as_str()).copied().unwrap_or(0);
        let value = own.max(child_max);
        latest.insert(s.id.clone(), value);
        children_latest.entry(s.local_parent_id.as_str())
            .and_modify(|previous| *previous = (*previous).max(value)).or_insert(value);
    }
    list.iter()
        .map(|s| {
            let d = depth.get(&s.id).copied().unwrap_or(0);
            let l = latest.get(&s.id).copied().unwrap_or(0);
            (s.id.clone(), (d, l))
        })
        .collect()
}

pub(crate) fn validate_embedding(vector: &[f32], dimensions: usize) -> Result<()> {
    anyhow::ensure!(
        vector.len() == dimensions
            && dimensions > 0
            && vector.iter().all(|value| value.is_finite())
            && vector.iter().any(|value| value.abs() > 1e-8),
        "inference failed or produced an invalid embedding; run `rsrs model reset-cpu`"
    );
    Ok(())
}
