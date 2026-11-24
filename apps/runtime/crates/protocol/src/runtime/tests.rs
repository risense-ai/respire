    use super::*;

    fn identity() -> RuntimeIdentity {
        RuntimeIdentity {
            runtime_version: "1.0.21-dev.1".to_owned(),
        }
    }

    #[test]
    fn health_extension_roundtrip() -> anyhow::Result<()> {
        let json = identity().health_extension_json();
        assert_eq!(json["runtime_version"], "1.0.21-dev.1");
        assert!(json.get("runtime_compat").is_none());
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

    fn legacy_release_metadata() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "runtime_version": "1.2.3",
            "tag": "v1.2.3",
            "triple": "x86_64-pc-windows-msvc",
            "sha256": "a".repeat(64),
            "binary_file": "rsrs-runtime.exe",
            "binary_sha256": "b".repeat(64)
        })
    }

    #[test]
    fn release_metadata_accepts_schema_one_alias_mapping() -> anyhow::Result<()> {
        let meta = RuntimeBuildMeta::from_json(&legacy_release_metadata())
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        assert_eq!(meta.schema_version, 1);
        assert!(meta.target_is("x86_64-pc-windows-msvc"));
        assert_eq!(meta.archive_sha256, "a".repeat(64));
        assert_eq!(meta.binary_sha256, "b".repeat(64));
        assert!(meta.source_commit.is_none());
        Ok(())
    }

    #[test]
    fn release_metadata_rejects_alias_conflicts_and_unknown_schema() {
        let mut wire = legacy_release_metadata();
        wire["target_triple"] = serde_json::json!("aarch64-pc-windows-msvc");
        assert_eq!(
            RuntimeBuildMeta::from_json(&wire),
            Err(RuntimeBuildMetaError::AliasConflict("target_triple"))
        );
        wire["target_triple"] = wire["triple"].clone();
        wire["archive_sha256"] = serde_json::json!("c".repeat(64));
        assert_eq!(
            RuntimeBuildMeta::from_json(&wire),
            Err(RuntimeBuildMetaError::AliasConflict("archive_sha256"))
        );

        let mut missing = legacy_release_metadata();
        missing["binary_sha256"] = serde_json::Value::Null;
        assert_eq!(
            RuntimeBuildMeta::from_json(&missing),
            Err(RuntimeBuildMetaError::MissingField("binary_sha256"))
        );
        assert_eq!(
            RuntimeBuildMeta::from_json(&serde_json::json!({"schema_version": 3})),
            Err(RuntimeBuildMetaError::UnsupportedSchema(3))
        );
    }
