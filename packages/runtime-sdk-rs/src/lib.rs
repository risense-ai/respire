//! Typed Runtime HTTP client generated from the pinned OpenAPI contract.

fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

include!(concat!(env!("OUT_DIR"), "/runtime_sdk.rs"));
