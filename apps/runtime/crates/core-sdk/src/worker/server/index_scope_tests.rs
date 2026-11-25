use super::*;

#[test]
fn reused_worker_thread_scopes_artifact_lookup_to_its_current_frame() -> Result<()> {
    let alpha = tempfile::tempdir()?;
    let beta = tempfile::tempdir()?;
    let fixture = "c3ludGhldGljIGFydGlmYWN0";

    let locator = with_frame_index_root(json!(alpha.path()), || {
        let mut prepared = json!({"artifact": fixture});
        crate::host::persist_prepared(&mut prepared)?;
        let locator = prepared["artifact"].as_str().context("prepared index locator")?;
        assert_eq!(crate::host::resolve(locator)?, fixture);
        Ok(locator.to_owned())
    })?;
    assert!(crate::business::index_root().is_err());

    with_frame_index_root(Value::Null, || {
        assert!(crate::business::index_root().is_err());
        assert!(crate::host::resolve(&locator).is_err());
        Ok(())
    })?;
    with_frame_index_root(json!(beta.path()), || {
        assert_eq!(crate::business::index_root()?, beta.path().canonicalize()?);
        assert!(crate::host::resolve(&locator).is_err());
        Ok(())
    })?;
    assert!(!beta.path().join("core-index").exists());
    with_frame_index_root(json!(alpha.path()), || {
        assert_eq!(crate::host::resolve(&locator)?, fixture);
        Ok(())
    })?;
    assert!(crate::business::index_root().is_err());
    Ok(())
}

#[test]
fn index_root_selection_is_restored_after_nested_error_and_unwind() -> Result<()> {
    let outer = tempfile::tempdir()?;
    let inner = tempfile::tempdir()?;
    with_frame_index_root(json!(outer.path()), || {
        let expected = outer.path().canonicalize()?;
        let result = with_frame_index_root(json!(inner.path()), || -> Result<()> {
            assert_eq!(crate::business::index_root()?, inner.path().canonicalize()?);
            bail!("synthetic worker error")
        });
        assert!(result.is_err());
        assert_eq!(crate::business::index_root()?, expected);

        let unwind = std::panic::catch_unwind(|| {
            let _: Result<()> = with_frame_index_root(Value::Null, || {
                assert!(crate::business::index_root().is_err());
                panic!("synthetic worker unwind");
            });
        });
        assert!(unwind.is_err());
        assert_eq!(crate::business::index_root()?, expected);
        Ok(())
    })?;
    assert!(crate::business::index_root().is_err());
    Ok(())
}

#[test]
fn invalid_index_roots_do_not_dispatch_or_create_paths() -> Result<()> {
    let root = tempfile::tempdir()?;
    let missing = root.path().join("missing");
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, b"synthetic")?;
    for invalid in [
        json!(12),
        json!("relative"),
        json!(missing),
        json!(file),
    ] {
        assert!(with_frame_index_root(invalid, || -> Result<()> { panic!("invalid frame was dispatched") }).is_err());
    }
    assert!(!missing.exists());
    assert!(crate::business::index_root().is_err());
    Ok(())
}
