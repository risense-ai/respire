use super::*;
use std::path::Path;

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        runtime_version: "1.0.21-dev.1".to_owned(),
        runtime_compat: RuntimeCompat {
            min_cli: "1.0.20".to_owned(),
            max_cli: "*".to_owned(),
        },
    }
}

#[test]
fn health_extension_roundtrip() -> anyhow::Result<()> {
    let json = identity().health_extension_json();
    assert_eq!(json["runtime_version"], "1.0.21-dev.1");
    assert_eq!(json["runtime_compat"]["min_cli"], "1.0.20");
    let back: RuntimeIdentity = serde_json::from_value(serde_json::json!({
        "runtime_version": "1.0.21-dev.1",
        "runtime_compat": {"min_cli": "1.0.20", "max_cli": "*"}
    }))?;
    assert_eq!(back, identity());
    Ok(())
}

#[test]
fn assets_status_defaults_incomplete() -> anyhow::Result<()> {
    let raw = r#"{"complete":false,"missing":[{"name":"bge-m3","suggested_source":"mirror"}]}"#;
    let status: AssetsStatus = serde_json::from_str(raw)?;
    assert!(!status.complete);
    assert!(status.backend_load.is_none());
    let file_only: AssetsStatus = serde_json::from_str(r#"{"complete":true,"missing":[]}"#)?;
    assert!(file_only.complete);
    assert!(file_only.backend_load.is_none());
    assert!(serde_json::to_value(file_only)?.get("backend_load").is_none());
    assert_eq!(status.missing.len(), 1);
    assert_eq!(status.missing[0].name, "bge-m3");
    assert!(status.missing[0].version.is_none());
    Ok(())
}

#[test]
fn asset_state_values_roundtrip() -> anyhow::Result<()> {
    let raw = r#"["missing","incomplete","corrupt","incompatible","complete"]"#;
    let states: Vec<AssetState> = serde_json::from_str(raw)?;
    assert_eq!(
        states,
        [
            AssetState::Missing,
            AssetState::Incomplete,
            AssetState::Corrupt,
            AssetState::Incompatible,
            AssetState::Complete,
        ]
    );
    assert_eq!(serde_json::to_string(&states)?, raw);
    Ok(())
}

#[test]
fn build_meta_roundtrip() -> anyhow::Result<()> {
    let meta = RuntimeBuildMeta {
        schema_version: 2,
        runtime_version: "1.0.21-dev.1".to_owned(),
        tag: "v1.0.21-dev.1".to_owned(),
        target_triple: "x86_64-pc-windows-msvc".to_owned(),
        archive_file: Some("rsrs-runtime-x86_64-pc-windows-msvc.tar.gz".to_owned()),
        archive_sha256: "1111111111111111111111111111111111111111111111111111111111111111"
            .to_owned(),
        binary_file: "rsrs-runtime-x86_64-pc-windows-msvc.exe".to_owned(),
        binary_sha256: "2222222222222222222222222222222222222222222222222222222222222222"
            .to_owned(),
        source_commit: Some("dd45330c9848ebb73f2d98a363e6af0de3fc5a91".to_owned()),
        compat: RuntimeCompat {
            min_cli: "1.0.20".to_owned(),
            max_cli: "*".to_owned(),
        },
        protocol_range: Some(ProtocolRange {
            minimum: 1,
            maximum: 1,
        }),
        capabilities: BTreeMap::from([("business_executor".to_owned(), "1".to_owned())]),
        files: vec![RuntimeBuildFile {
            path: "rsrs-runtime-x86_64-pc-windows-msvc.exe".to_owned(),
            size: 4096,
            sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                .to_owned(),
        }],
    };
    let json = serde_json::to_string(&meta)?;
    let back: RuntimeBuildMeta = serde_json::from_str(&json)?;
    assert_eq!(back, meta);
    assert_eq!(back.tag.trim_start_matches('v'), back.runtime_version);
    Ok(())
}

#[test]
fn release_metadata_accepts_schema_one_alias_mapping() -> anyhow::Result<()> {
    let root = Path::new(env!("RSRS_CONTRACT_FIXTURES")).join("fixtures/positive");
    let v1: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("release-metadata-v1.json"),
    )?)?;
    let meta = RuntimeBuildMeta::from_json(&v1["payload"])
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(meta.schema_version, 1);
    assert_eq!(meta.target_triple, "x86_64-pc-windows-msvc");
    assert_ne!(meta.archive_sha256, meta.binary_sha256);
    assert!(meta.source_commit.is_none());

    let v2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("release-metadata-v2.json"),
    )?)?;
    let canonical = RuntimeBuildMeta::from_json(&v2["payload"])
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    assert_eq!(canonical.schema_version, 2);
    assert!(canonical.target_is("x86_64-pc-windows-msvc"));
    assert_ne!(canonical.archive_sha256, canonical.binary_sha256);
    Ok(())
}

#[test]
fn release_metadata_rejects_alias_conflicts_and_unknown_schema() -> anyhow::Result<()> {
    let root =
        Path::new(env!("RSRS_CONTRACT_FIXTURES")).join("fixtures");
    let conflict: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("negative/release-metadata-conflicting-hash.json"),
    )?)?;
    assert_eq!(
        RuntimeBuildMeta::from_json(&conflict["payload"]),
        Err(RuntimeBuildMetaError::AliasConflict("target_triple"))
    );
    let mut archive_conflict = conflict.clone();
    archive_conflict["payload"]["triple"] =
        archive_conflict["payload"]["target_triple"].clone();
    assert_eq!(
        RuntimeBuildMeta::from_json(&archive_conflict["payload"]),
        Err(RuntimeBuildMetaError::AliasConflict("archive_sha256"))
    );
    let missing: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("negative/release-metadata-missing-binary-hash.json"),
    )?)?;
    assert_eq!(
        RuntimeBuildMeta::from_json(&missing["payload"]),
        Err(RuntimeBuildMetaError::MissingField("binary_sha256"))
    );

    let mut unknown = serde_json::json!({"schema_version": 3});
    assert_eq!(
        RuntimeBuildMeta::from_json(&unknown),
        Err(RuntimeBuildMetaError::UnsupportedSchema(3))
    );
    unknown["schema_version"] = serde_json::json!(2);
    assert_eq!(
        RuntimeBuildMeta::from_json(&unknown),
        Err(RuntimeBuildMetaError::MissingField("runtime_version"))
    );
    Ok(())
}
