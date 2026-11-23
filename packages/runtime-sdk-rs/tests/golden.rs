use respire_runtime_sdk::types::{
    MemoryItem, ModelCatalogResponse, OperationEvent, ProblemDetails,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn assert_roundtrip<T: Serialize + DeserializeOwned>(fixture: &str) {
    let original: Value = serde_json::from_str(fixture).expect("valid golden JSON");
    let data =
        serde_json::from_value::<T>(original.clone()).expect("golden matches generated type");
    assert_eq!(
        serde_json::to_value(data).expect("generated type serializes"),
        original
    );
}

#[test]
fn round_trips_contract_fixtures() {
    assert_roundtrip::<ProblemDetails>(include_str!(
        "../../../contracts/runtime-api/fixtures/problem-details.json"
    ));
    assert_roundtrip::<OperationEvent>(include_str!(
        "../../../contracts/runtime-api/fixtures/operation-event.json"
    ));
    assert_roundtrip::<MemoryItem>(include_str!(
        "../../../contracts/runtime-api/fixtures/memory-item.json"
    ));
    assert_roundtrip::<ModelCatalogResponse>(include_str!(
        "../../../contracts/runtime-api/fixtures/model-catalog-response.json"
    ));
}

#[test]
fn optional_nullable_memory_field_preserves_all_three_wire_states() {
    let raw = include_str!("../../../contracts/runtime-api/fixtures/memory-item.json");
    let mut source: Value = serde_json::from_str(raw).expect("valid fixture");
    let null = serde_json::from_value::<MemoryItem>(source.clone()).expect("valid null");
    assert_eq!(null.parent_id, Some(None));
    let object = source.as_object_mut().expect("fixture object");
    object.remove("parent_id");
    let absent = serde_json::from_value::<MemoryItem>(source.clone()).expect("valid absence");
    assert_eq!(absent.parent_id, None);
    assert_eq!(serde_json::to_value(absent).expect("serializable"), source);
    source["parent_id"] = Value::String("parent-example".into());
    let present = serde_json::from_value::<MemoryItem>(source.clone()).expect("valid string");
    assert_eq!(present.parent_id, Some(Some("parent-example".into())));
    assert_eq!(serde_json::to_value(present).expect("serializable"), source);
}
