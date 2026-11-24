use crate::localized_test_data::{NON_UNIQUE_PREFIX};
use super::*;

fn node(id: &str, parent: &str) -> StoredMemory {
    let mut row = StoredMemory::new_pending(id.into());
    row.local_parent_id = parent.into();
    row
}

#[test]
fn planner_preserves_thresholds_and_original_child_order() -> Result<()> {
    let mut all = vec![node("root", "")];
    all.extend((0..14).rev().map(|i| node(&format!("leaf-{i}"), "root")));
    assert!(deepen_plan(&all, "root", 0.3, 4).is_err());
    all.push(node("leaf-14", "root"));
    all.extend((0..10).map(|i| node(&format!("below-{i}"), "leaf-0")));
    let (root, plan) = deepen_plan(&all, "root", 0.3, 4)?;
    assert_eq!(root, "root");
    assert_eq!(plan.leftovers.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), (0..14).rev().chain([14]).map(|i| format!("leaf-{i}")).collect::<Vec<_>>());
    all.push(node("below-10", "leaf-0"));
    assert!(deepen_plan(&all, "root", 0.3, 4).is_err());
    assert!(deepen_plan(&all, NON_UNIQUE_PREFIX, 0.3, 4).is_err());
    Ok(())
}

#[test]
fn eligible_plan_preserves_two_vector_clusters() -> Result<()> {
    let mut rows = vec![node("root", "")];
    rows.extend((0..15).map(|index| {
        let mut row = node(&format!("child-{index:02}"), "root");
        row.local_embedding = match index {
            0..=3 => Some(vec![1.0, 0.0]),
            4..=7 => Some(vec![0.0, 1.0]),
            _ => None,
        };
        row
    }));
    let (_, plan) = deepen_plan(&rows, "root", 0.9, 4)?;
    let mut memberships = plan.sub_roots.into_iter()
        .map(|cluster| cluster.member_ids).collect::<Vec<_>>();
    memberships.sort();
    assert_eq!(memberships, [(0..4).collect::<Vec<_>>(), (4..8).collect::<Vec<_>>()]
        .map(|cluster| cluster.into_iter().map(|index| format!("child-{index:02}"))
            .collect::<Vec<_>>()));
    assert_eq!(plan.leftovers.len(), 7);
    Ok(())
}
