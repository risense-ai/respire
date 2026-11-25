use super::*;

#[test]
fn schema2_accepts_unique_files_and_rejects_alias_duplicates() -> Result<()> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("RSRS_CONTRACT_FIXTURES"),
        "/fixtures/positive/release-metadata-v2.json"
    )))?;
    let mut metadata: BuildMetadata = serde_json::from_value(fixture["payload"].clone())?;
    metadata.validate_schema2()?;
    let duplicate = metadata.files.first().context("fixture has no binary")?;
    metadata.files.push(BuildFile {
        path: duplicate.path.to_ascii_uppercase(),
        size: duplicate.size,
        sha256: duplicate.sha256.clone(),
    });
    anyhow::ensure!(metadata.validate_schema2().is_err());
    Ok(())
}

#[test]
fn metadata_rejects_null_aliases_and_duplicate_capabilities() -> Result<()> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("RSRS_CONTRACT_FIXTURES"),
        "/fixtures/positive/release-metadata-v2.json"
    )))?;
    let mut payload = fixture["payload"].clone();
    payload["sha256"] = serde_json::Value::Null;
    anyhow::ensure!(serde_json::from_value::<BuildMetadata>(payload).is_err());
    let json = fixture["payload"].to_string().replace(
        "\"business_executor\":\"1\"",
        "\"business_executor\":\"1\",\"business_executor\":\"0\"",
    );
    anyhow::ensure!(serde_json::from_str::<BuildMetadata>(&json).is_err());
    Ok(())
}

#[test]
fn paths_and_mirrors_fail_closed() -> Result<()> {
    for name in [
        "../escape",
        "a/../b",
        "/absolute",
        "a//b",
        "a/./b",
        "NUL.txt",
        "dir/COM1",
        "trailing.",
        "space ",
        "C:stream",
        "a\\b",
    ] {
        assert!(!safe_metadata_path(name), "unsafe path accepted: {name}");
    }
    assert!(safe_metadata_path("core-notices/models/LICENSE.txt"));
    assert_eq!(
        download_origins(None)?,
        [RELEASE_BASE.to_owned(), format!("{MIRROR_PREFIX}/{RELEASE_BASE}")]
    );
    assert_eq!(download_origins(Some(RELEASE_BASE))?, [RELEASE_BASE]);
    assert!(download_origins(Some("https://unapproved.invalid")).is_err());
    let version = "2.0.0-dev.4";
    let name = "runtime-build-x86_64-unknown-linux-gnu.json";
    let source = RuntimeReleaseSource::Legacy;
    let official = source.browser_url(RELEASE_BASE, version, name);
    assert_eq!(official_asset_identity(&official), Some((source, version, name)));
    for url in [
        format!("{MIRROR_PREFIX}/{official}"),
        format!("{official}?redirect=1"),
        official.replace("github.com/", "github.com.evil.invalid/"),
        official.replace(name, "../escape"),
    ] {
        assert!(official_asset_identity(&url).is_none());
    }
    let asset = serde_json::json!({"id":1,"name":name,"size":32,"state":"uploaded", "digest":format!("sha256:{}", "a".repeat(64))});
    let release = serde_json::json!({"tag_name":format!("v{version}"),"draft":false,"assets":[asset.clone()]});
    assert_eq!(
        public_asset(serde_json::from_value(release.clone())?, source, version, name, 32)?.id,
        1
    );
    let mut invalid = Vec::new();
    for (field, value) in [
        ("tag_name", serde_json::json!("v2.0.0-dev.3")),
        ("draft", serde_json::json!(true)),
        ("assets", serde_json::json!([])),
        ("assets", serde_json::json!([asset.clone(), asset.clone()])),
    ] {
        let mut changed = release.clone();
        changed[field] = value;
        invalid.push(changed);
    }
    for (field, value) in [
        ("id", serde_json::json!(0)),
        ("name", serde_json::json!("wrong.json")),
        ("size", serde_json::json!(0)),
        ("size", serde_json::json!(33)),
        ("state", serde_json::json!("new")),
        ("digest", serde_json::json!("sha256:invalid")),
    ] {
        let mut changed = release.clone();
        changed["assets"][0][field] = value;
        invalid.push(changed);
    }
    for changed in invalid {
        assert!(public_asset(serde_json::from_value(changed)?, source, version, name, 32).is_err());
    }
    Ok(())
}
