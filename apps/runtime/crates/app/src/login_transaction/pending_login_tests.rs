use super::*;
use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct RuntimeChild(Child);
impl Drop for RuntimeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        ensure!(Instant::now() < deadline, "disposable Runtime did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
#[ignore = "requires the compiled Runtime binary"]
fn ordinary_runtime_persists_scoped_login_confirmation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let (password, _, salt, wrapped, nonce) = crate::auth::new_vault_v4()?;
    let session = serde_json::to_vec(&json!({
        "user":"source", "vault_version":4, "kdf_salt":salt, "wrapped_urk":wrapped,
        "urk_nonce":nonce, "addr":"http://127.0.0.1:1"
    }))?;
    private_write(&root.join("session.json"), &session)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let addr = listener.local_addr()?;
    drop(listener);
    let base = format!("http://{addr}");
    let token = uuid::Uuid::new_v4().to_string();
    let start = || -> Result<RuntimeChild> {
        let child = Command::new(std::env::var("CARD010_RUNTIME")?)
            .args(["serve", "--port", &addr.port().to_string(), "--host", "127.0.0.1"])
            .env("HOME", &root).env("RSRS_DATA_DIR", &root).env("RSRS_RPC_TOKEN", &token)
            .env("RSRS_NO_AUTOSYNC", "1").stdout(Stdio::null()).spawn()?;
        Ok(RuntimeChild(child))
    };
    let child = start()?;
    let endpoint_path = root.join("runtime/endpoint.json");
    wait_until(|| endpoint_path.is_file())?;
    let endpoint: Value = serde_json::from_slice(&std::fs::read(endpoint_path)?)?;
    let http = ureq::AgentBuilder::new().try_proxy_from_env(false).redirects(0)
        .timeout(Duration::from_secs(10)).build();
    let unlock = json!({"schema_version":2,"profile":root,"session_sha256":digest(&session),
        "instance_id":endpoint["instance_id"],"generation":endpoint["generation"],"super_password":password,
        "provider_update":{"kind":"replace","credentials":[]}});
    let receipt: Value = http.post(&format!("{base}/api/runtime/unlock"))
        .set("Authorization", &format!("Bearer {token}")).send_json(unlock)?.into_json()?;
    ensure!(receipt["store_ready"] == true);

    let post = |payload: Value, bearer: &str| -> Result<(u16, Value)> {
        let request = http.post(&format!("{base}/v1/host/transactions/login"))
            .set("Authorization", &format!("Bearer {bearer}"));
        let response = match request.send_json(payload) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(err) => return Err(err.into()),
        };
        let status = response.status();
        if status >= 400 {
            assert_eq!(response.header("Content-Type"), Some("application/problem+json"));
        }
        Ok((status, response.into_json()?))
    };
    let account = || -> Result<Value> {
        Ok(http.get(&format!("{base}/v1/host/account"))
            .set("Authorization", &format!("Bearer {token}")).call()?.into_json()?)
    };
    let path = root.join(FILE);
    assert_eq!(account()?["transaction_state"], "none");
    for payload in [json!({}), json!({"mode":"bogus"}), json!({"mode":"recovery","passphrase_present":null}),
        json!({"mode":"recovery","passphrase_present":"true"}), json!({"mode":"browser_oauth","passphrase":"unused"})] {
        assert_eq!(post(payload, &token)?.0, 400);
    }
    let browser = json!({"mode":"browser_oauth","passphrase_present":true});
    assert_eq!(post(browser.clone(), "wrong-owner")?.0, 401);
    assert_eq!(post(json!({"mode":"offline_keygen"}), &token)?.0, 409);
    assert!(!path.exists());

    let expected = (200, json!({"status":"pending_confirmation"}));
    assert_eq!(post(browser.clone(), &token)?, expected);
    let original = std::fs::read(&path)?;
    assert_eq!(account()?["transaction_state"], "login_pending");
    let other = tempfile::tempdir()?;
    std::fs::write(other.path().join(FILE), &original)?;
    assert_eq!(projection(other.path(), Some(&session), &Journal::default())?, "none");
    assert_eq!(projection(&root, Some(br#"{"user":"other"}"#), &Journal::default())?, "none");
    assert_eq!(post(browser, &token)?, expected);
    assert_eq!(post(json!({"mode":"recovery"}), &token)?.0, 409);
    assert_eq!(std::fs::read(&path)?, original);

    // Exercise expiry without a five-minute sleep.
    let mut expired: Pending = serde_json::from_slice(&original)?;
    expired.issued = now()? - LIFETIME_SECONDS;
    private_write(&path, &serde_json::to_vec(&expired)?)?;
    assert_eq!(account()?["transaction_state"], "none");
    assert_eq!(post(json!({"mode":"recovery"}), &token)?, expected);
    assert_eq!(account()?["transaction_state"], "recovery_pending");
    let persisted = std::fs::read(&path)?;
    drop(child);
    let _reopened = start()?;
    wait_until(|| account().is_ok_and(|view| view["transaction_state"] == "recovery_pending"))?;
    assert_eq!(std::fs::read(&path)?, persisted);
    assert_eq!(std::fs::read(root.join("session.json"))?, session);
    Ok(())
}
