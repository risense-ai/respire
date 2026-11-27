use super::*;

#[test]
fn port_is_open_sees_local_listener() -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    if !port_is_open_on(port) {
        return Err(format!("listener on {port} should be open"));
    }
    Ok(())
}

#[test]
fn default_port_is_15169() -> Result<(), String> {
    if DEFAULT_RPC_PORT != 15169 {
        return Err(format!("port {}", DEFAULT_RPC_PORT));
    }
    Ok(())
}

#[test]
fn rpc_port_reads_env() -> Result<(), String> {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = respire_client_host::env::var("RSRS_RPC_PORT").ok();
    std::env::set_var("RSRS_RPC_PORT", "26111");
    let got = rpc_port();
    match prev {
        Some(v) => std::env::set_var("RSRS_RPC_PORT", v),
        None => std::env::remove_var("RSRS_RPC_PORT"),
    }
    if got != 26111 {
        return Err(format!("got {got}"));
    }
    Ok(())
}

#[test]
fn origin_empty_is_cli() -> Result<(), String> {
    if !origin_ok("", "http://127.0.0.1:15169") {
        return Err("empty origin should pass".into());
    }
    if origin_ok("https://evil.example", "http://127.0.0.1:15169") {
        return Err("evil origin should fail".into());
    }
    if !origin_ok("http://127.0.0.1:15169", "http://127.0.0.1:15169") {
        return Err("same origin should pass".into());
    }
    if !origin_ok("http://localhost:29123", "http://127.0.0.1:29123") {
        return Err("localhost must use the actual bound port".into());
    }
    if origin_ok("http://localhost:15169", "http://127.0.0.1:29123") {
        return Err("default-port origin must not match a different listener".into());
    }
    if !origin_ok("http://127.0.0.1", "http://127.0.0.1:80")
        || !origin_ok("http://localhost", "http://127.0.0.1:80")
        || !origin_ok("http://[::1]", "http://[::1]:80")
    {
        return Err("HTTP default-port origins must use their canonical form".into());
    }
    if origin_ok("http://localhost", "http://127.0.0.1:29123")
        || origin_ok("http://evil.example", "http://127.0.0.1:80")
    {
        return Err("canonical origins must not exempt other ports or hosts".into());
    }
    Ok(())
}

#[test]
fn health_shape() -> Result<(), String> {
    let ours = json!({"server":"respire","bin":"1.0.6-dev.2","pid":1,"v":1,"exe":""});
    if !is_our_health(&ours) {
        return Err("ours".into());
    }
    if is_our_health(&json!({"server":"nginx"})) {
        return Err("nginx".into());
    }
    let health: Health = serde_json::from_value(ours.clone()).map_err(|error| error.to_string())?;
    assert!(health.rpc.is_null());
    let mut reported = ours;
    reported["rpc"] = json!({"effective_parallelism":3});
    let health: Health = serde_json::from_value(reported).map_err(|error| error.to_string())?;
    assert_eq!(health.rpc["effective_parallelism"], 3);
    Ok(())
}

#[test]
fn version_newer_dev() -> Result<(), String> {
    if version_cmp("1.0.6-dev.2", "1.0.6-dev.1") != std::cmp::Ordering::Greater {
        return Err("dev.2 should be newer".into());
    }
    if version_cmp("1.0.6-dev.1", "1.0.6-dev.1") != std::cmp::Ordering::Equal {
        return Err("equal".into());
    }
    Ok(())
}

#[test]
fn token_env_beats_file() -> Result<(), String> {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let prev_data = respire_client_host::env::var("RSRS_DATA_DIR").ok();
    let prev_tok = respire_client_host::env::var("RSRS_RPC_TOKEN").ok();
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    std::env::set_var("RSRS_RPC_TOKEN", "from-env");
    let got = load_token();
    match prev_data {
        Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    match prev_tok {
        Some(v) => std::env::set_var("RSRS_RPC_TOKEN", v),
        None => std::env::remove_var("RSRS_RPC_TOKEN"),
    }
    if got.as_deref() != Some("from-env") {
        return Err(format!("{got:?}"));
    }
    Ok(())
}

#[test]
fn persist_and_load_token_file() -> Result<(), String> {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;

    let fixture = dir.path().join("token-reader");
    assert!(read_token_file(&fixture).map_err(|e| e.to_string())?.is_none());
    for invalid in [b"".as_slice(), b" \t\r\n".as_slice(), &[0xff]] {
        std::fs::write(&fixture, invalid).map_err(|e| e.to_string())?;
        let error = read_token_file(&fixture).err().ok_or("invalid token accepted")?;
        assert!(matches!(
            error.downcast_ref::<RuntimeError>(), Some(RuntimeError::TokenUnreadable(_))
        ));
        assert_eq!(std::fs::read(&fixture).map_err(|e| e.to_string())?, invalid);
    }
    std::fs::write(&fixture, "  file-token\r\n").map_err(|e| e.to_string())?;
    assert_eq!(
        read_token_file(&fixture).map_err(|e| e.to_string())?.as_deref(),
        Some("file-token")
    );
    std::fs::remove_file(&fixture).map_err(|e| e.to_string())?;
    std::fs::create_dir(&fixture).map_err(|e| e.to_string())?;
    assert!(read_token_file(&fixture).is_err());
    std::fs::remove_dir(&fixture).map_err(|e| e.to_string())?;
    let prev_data = respire_client_host::env::var("RSRS_DATA_DIR").ok();
    let prev_tok = respire_client_host::env::var("RSRS_RPC_TOKEN").ok();
    std::env::remove_var("RSRS_RPC_TOKEN");
    std::env::set_var("RSRS_DATA_DIR", dir.path());
    persist_token("file-token").map_err(|e| e.to_string())?;
    let got = load_token();
    match prev_data {
        Some(v) => std::env::set_var("RSRS_DATA_DIR", v),
        None => std::env::remove_var("RSRS_DATA_DIR"),
    }
    match prev_tok {
        Some(v) => std::env::set_var("RSRS_RPC_TOKEN", v),
        None => std::env::remove_var("RSRS_RPC_TOKEN"),
    }
    if got.as_deref() != Some("file-token") {
        return Err(format!("{got:?}"));
    }
    Ok(())
}
