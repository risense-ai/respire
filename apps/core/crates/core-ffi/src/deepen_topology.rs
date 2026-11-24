//! Indexed topology eligibility for the existing tree-deepen planner.
use anyhow::{ensure, Context, Result};
use std::collections::{HashMap, HashSet};

pub(crate) struct Topology<'a> {
    children: HashMap<&'a str, Vec<&'a str>>,
    ids: HashSet<&'a str>,
    subtree_sizes: HashMap<&'a str, usize>,
    // Test-only count of input rows, stack frames and traversed edges.
    #[cfg(test)]
    work: usize,
}

#[derive(Debug)]
pub(crate) struct Eligibility {
    pub(crate) eligible: bool,
    pub(crate) direct_count: usize,
    pub(crate) descendant_count: usize,
}

impl<'a> Topology<'a> {
    pub(crate) fn new(edges: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut tree = Self {
            children: HashMap::new(),
            ids: HashSet::new(),
            subtree_sizes: HashMap::new(),
            #[cfg(test)]
            work: 0,
        };
        for (id, parent) in edges {
            #[cfg(test)]
            {
                tree.work += 1;
            }
            ensure!(tree.ids.insert(id), "invalid tree: repeated node ID");
            tree.children.entry(parent).or_default().push(id);
        }
        Ok(tree)
    }

    fn children(&self, id: &str) -> &[&'a str] {
        self.children.get(id).map_or(&[], Vec::as_slice)
    }

    /// Traverse each reachable subtree once across the request. An explicit
    /// stack handles deeply nested trees without running out of call stack.
    fn measure(&mut self, root: &'a str) -> Result<usize> {
        let mut pending = vec![(root, false)];
        let mut visiting = HashSet::new();
        while let Some((id, complete)) = pending.pop() {
            #[cfg(test)]
            {
                self.work += 1;
                if complete || !self.subtree_sizes.contains_key(id) {
                    self.work += self.children(id).len();
                }
            }
            if complete {
                visiting.remove(id);
                let size = self.children(id).iter().try_fold(1, |size, child| {
                    let child_size = self
                        .subtree_sizes
                        .get(child)
                        .context("missing descendant count")?;
                    Ok::<_, anyhow::Error>(size + child_size)
                })?;
                self.subtree_sizes.insert(id, size);
            } else if !self.subtree_sizes.contains_key(id) {
                ensure!(visiting.insert(id), "invalid tree: repeated descendant ID");
                pending.push((id, true));
                pending.extend(self.children(id).iter().rev().map(|child| (*child, false)));
            }
        }
        self.subtree_sizes
            .get(root)
            .copied()
            .context("missing root count")
    }

    pub(crate) fn check(&mut self, id: &'a str) -> Result<Eligibility> {
        ensure!(self.ids.contains(id), "no such root: {id}");
        let direct = self.children(id).len();
        let descendants = self.measure(id)? - 1 - direct;
        Ok(Eligibility {
            // Equivalent to descendants * 3 <= direct * 2, without overflow.
            eligible: direct >= 15 && descendants <= direct - direct.div_ceil(3),
            direct_count: direct,
            descendant_count: descendants,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_preserves_counts_across_wide_nested_and_disconnected_trees() -> Result<()> {
        let mut edges = vec![
            ("root".to_owned(), "".to_owned()),
            ("orphan".to_owned(), "missing".to_owned()),
        ];
        edges.extend((0..6_000).map(|i| (format!("leaf-{i}"), "root".to_owned())));
        edges.extend((0..4_000).map(|i| (format!("below-{i}"), format!("leaf-{i}"))));
        edges.push(("deep".to_owned(), "below-0".to_owned()));
        let mut index = Topology::new(
            edges
                .iter()
                .map(|(id, parent)| (id.as_str(), parent.as_str())),
        )?;
        let below = index.check("leaf-0")?;
        assert_eq!((below.direct_count, below.descendant_count), (1, 1));
        let main = index.check("root")?;
        assert_eq!(
            (main.direct_count, main.descendant_count, main.eligible),
            (6_000, 4_001, false)
        );
        let orphan = index.check("orphan")?;
        assert_eq!((orphan.direct_count, orphan.descendant_count), (0, 0));
        assert_eq!(index.subtree_sizes.len(), edges.len());
        Ok(())
    }

    #[test]
    fn rejects_duplicate_nodes_missing_roots_and_reachable_cycles() -> Result<()> {
        assert!(Topology::new([("root", ""), ("root", "parent")]).is_err());
        let mut cyclic = Topology::new([("root", "child"), ("child", "root"), ("valid", "")])?;
        assert!(cyclic.check("root").is_err());
        assert!(cyclic.check("missing").is_err());
        assert_eq!(cyclic.check("valid")?.direct_count, 0);
        Ok(())
    }
}

#[cfg(test)]
mod scaling_tests {
    use super::*;
    use std::{hint::black_box, time::Instant};

    fn edges(n: usize) -> Vec<(String, String)> {
        let mut rows = Vec::with_capacity(n);
        rows.push(("root".into(), "".into()));
        rows.extend((1..n).map(|i| {
            let parent = if i % 4 != 0 {
                "root".into()
            } else {
                format!("row-{}", i - 1)
            };
            (format!("row-{i}"), parent)
        }));
        rows
    }

    // Preserve the legacy flatness walk as a test-only comparison oracle.
    // It scans all rows for direct children, then again for every reachable node.
    fn legacy_count(rows: &[(String, String)]) -> (usize, usize, usize) {
        let mut pending: Vec<_> = rows
            .iter()
            .filter(|row| row.1 == "root")
            .map(|row| &row.0)
            .collect();
        let direct = pending.len();
        let mut below = 0;
        let mut checks = rows.len();
        while let Some(parent) = pending.pop() {
            for (id, candidate_parent) in rows {
                checks += 1;
                if candidate_parent == parent {
                    below += 1;
                    pending.push(id);
                }
            }
        }
        (direct, below, checks)
    }

    fn indexed_count(rows: &[(String, String)]) -> Result<(usize, usize, usize)> {
        let mut topology = Topology::new(rows.iter().map(|(id, p)| (id.as_str(), p.as_str())))?;
        let counted = topology.check("root")?;
        assert_eq!(
            counted.eligible,
            counted.direct_count >= 15 && counted.descendant_count * 3 <= counted.direct_count * 2
        );
        Ok((
            counted.direct_count,
            counted.descendant_count,
            topology.work,
        ))
    }

    #[test]
    fn deepen_plan_topology_relative_work_at_10k_and_20k() -> Result<()> {
        let mut previous = None;
        for n in [10_000, 20_000] {
            let rows = edges(n);
            let (direct, below, indexed) = indexed_count(&rows)?;
            let (old_direct, old_below, inspected) = legacy_count(&rows);
            assert_eq!((direct, below), (old_direct, old_below));
            assert_eq!(direct + below, n - 1);
            assert_eq!(inspected, n * n);
            // Each row is indexed once; each node is pushed/popped twice and
            // each tree edge is walked twice. No vector clustering is measured.
            assert_eq!(indexed, n + 2 * n + 2 * (n - 1));
            if let Some((before_index, before_inspected)) = previous {
                assert_eq!(indexed, 2 * before_index + 2);
                assert_eq!(inspected, 4 * before_inspected);
            }
            previous = Some((indexed, inspected));
        }
        Ok(())
    }

    #[test]
    #[ignore = "manual wall-clock comparison; deterministic work regression runs by default"]
    fn deepen_plan_topology_relative_benchmark() -> Result<()> {
        // Fixture construction and vector clustering are outside the timings.
        // Ratios are observations, not flaky CI gates.
        for n in [10_000, 20_000] {
            let rows = edges(n);
            let mut indexed = Vec::new();
            let mut legacy = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                black_box(indexed_count(black_box(&rows))?);
                indexed.push(started.elapsed());
                let started = Instant::now();
                black_box(legacy_count(black_box(&rows)));
                legacy.push(started.elapsed());
            }
            indexed.sort_unstable();
            legacy.sort_unstable();
            eprintln!(
                "{n} rows: indexed median {:?}; legacy rescan median {:?}",
                indexed[1], legacy[1]
            );
        }
        Ok(())
    }
}
