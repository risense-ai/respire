use super::*;
#[test]
fn capability_requires_exact_version_not_truthiness() {
    assert!(require_handoff_capability(&BTreeMap::new()).is_err());
    for value in ["", "true", "0", "2", "01", "1 "] {
        assert!(require_handoff_capability(&BTreeMap::from([("offline_keygen_handoff".into(),value.into())])).is_err());
    }
    assert!(require_handoff_capability(&BTreeMap::from([("offline_keygen_handoff".into(),"1".into())])).is_ok());
}
