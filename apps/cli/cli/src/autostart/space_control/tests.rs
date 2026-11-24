use super::*;

const PID: u32 = 5252;
const DIAGNOSTIC: &str = "synthetic-private-space-diagnostic";

struct Fixture {
    _directory: tempfile::TempDir,
    original: Original,
    request: RpcRequest,
    response: serde_json::Value,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let operation_id = uuid::Uuid::new_v4().to_string();
        let original = Original { schema_version: HOST_SPACE_SCHEMA_V, root: root.clone(), profile: root.clone(),
            operation_id: operation_id.clone(), key: operation_id.clone(), source_identity: HostAuthIdentity::Absent,
            operation: HostSpaceOperation::Kick { session_id: None, all: true }, token: "fixture-maintenance".into(),
            coordinator: uuid::Uuid::new_v4().to_string(), was_serving: false, store_ready: false, credential_backend:None, legacy_factor:false, sent: true };
        let receipt = HostSpaceResult { operation_id: operation_id.clone(), fingerprint: original.fingerprint()?,
            profile: root.clone(), identity: original.source_identity.clone(), operation: original.operation.clone(),
            outcome: HostSpaceOutcome::Committed, invite_code: None, session_id: None, target_profile: None,
            target_account_id: None, target_session_sha256: None,
            kick: vec![HostSpaceKickItem { session_id: uuid::Uuid::new_v4().to_string(), state: HostSpaceKickState::Revoked }],
            remaining: Some(0) };
        let request: RpcRequest = serde_json::from_value(json!({"v":1,"id":uuid::Uuid::new_v4().to_string(),
            "method":HOST_SPACE_KICK_METHOD,"args":[],"operation_id":operation_id,"idempotency_key":operation_id}))?;
        let response = json!({"v":request.v,"id":request.id,"pid":PID,"bin":"rsrs-runtime",
            "operation_id":original.operation_id,"idempotency_key":original.key,"scope":null,"web_url":null,
            "data_dir":root,"ok":true,"exit":0,"code":null,"error":null,
            "envelope":ResultEnvelope::new("space-control",Status::Ok,serde_json::to_value(receipt)?,vec![
                Item::new("control_ready",Status::Ok,"true"),Item::new("store_ready",Status::Skip,"false")])});
        Ok(Self { _directory: directory, original, request, response })
    }

    fn validate(&self, response: serde_json::Value) -> Result<ResultEnvelope> {
        validate_response(serde_json::from_value(response)?, &self.request, &self.original.profile, PID, &self.original)
    }

    fn rejection(&self, code: &str) -> serde_json::Value {
        let mut response = self.response.clone();
        response["ok"] = json!(false);
        response["exit"] = json!(2);
        response["code"] = json!(code);
        response["error"] = json!(DIAGNOSTIC);
        let deadline = code == "control_outcome_unknown";
        response["envelope"] = json!({"command":"space-control","status":"fail",
            "summary":if deadline { json!({"outcome":"unknown"}) } else { json!({}) },
            "details":null,"actions":[],"related":[],"errors":if deadline { json!([]) } else { json!([DIAGNOSTIC]) },
            "items":[Item::new("control_ready",if deadline { Status::Skip } else { Status::Fail },
                if deadline { "unknown" } else { "false" }),Item::new("store_ready",Status::Skip,"false")]});
        response
    }

    fn reject_malformed(&self, response: serde_json::Value) -> Result<()> {
        let error = self.validate(response).err().context("malformed space response was accepted")?.to_string();
        assert!(!error.contains("Runtime rejected space control"), "malformed response was classified: {error}");
        assert!(!error.contains(DIAGNOSTIC));
        Ok(())
    }
}

#[test]
fn stable_rejections_preserve_codes_without_exposing_diagnostics() -> Result<()> {
    let fixture = Fixture::new()?;
    for code in ["unauthorized", "scope_mismatch", "auth_ownership_unavailable", "control_unavailable",
        "space_outcome_unknown", "control_outcome_unknown"] {
        let error = fixture.validate(fixture.rejection(code)).err().context("rejection was accepted")?.to_string();
        assert_eq!(error, format!("Runtime rejected space control ({code}); query the original operation without replaying"));
        assert!(!error.contains(DIAGNOSTIC));
    }
    Ok(())
}

#[test]
fn rejections_require_the_original_request_owner_and_scope() -> Result<()> {
    let fixture = Fixture::new()?;
    for (pointer, value) in [
        ("/v", json!(fixture.request.v + 1)), ("/id", json!("foreign-request")),
        ("/pid", json!(PID + 1)), ("/bin", json!("foreign-image")),
        ("/operation_id", json!(uuid::Uuid::new_v4().to_string())), ("/idempotency_key", json!("foreign-key")),
        ("/scope", json!({"account_id":"foreign-account","instance_id":"foreign-instance","generation":1})),
        ("/web_url", json!("https://fixture.invalid")), ("/data_dir", json!(fixture.original.profile.parent())),
    ] {
        let mut response = fixture.rejection("unauthorized");
        *response.pointer_mut(pointer).context("fixture binding field missing")? = value;
        fixture.reject_malformed(response).with_context(|| format!("binding field {pointer}"))?;
    }
    Ok(())
}

#[test]
fn rejections_reject_unknown_codes_and_malformed_envelopes() -> Result<()> {
    let fixture = Fixture::new()?;
    for code in ["unauthorized", "control_outcome_unknown"] {
        for (pointer, value) in [
            ("/code", json!("unrecognized-private-code")), ("/ok", json!(true)), ("/exit", json!(0)),
            ("/error", json!(null)), ("/error", json!("")), ("/envelope", json!(null)),
            ("/envelope/command", json!("foreign-command")), ("/envelope/status", json!("ok")),
            ("/envelope/summary", json!({"outcome":"committed"})), ("/envelope/details", json!({})),
            ("/envelope/actions", json!(["untrusted-action"])),
            ("/envelope/related", json!([respire_protocol::RelatedMemory { id: "foreign-memory".into(),
                title: "untrusted-related".into(), source_id: "foreign-source".into(),
                relation: respire_protocol::RelationKind::SeeAlso }])),
            ("/envelope/errors", json!(["different-diagnostic"])), ("/envelope/items", json!([])),
            ("/envelope/items/0/name", json!("other-ready")), ("/envelope/items/0/value", json!("true")),
            ("/envelope/items/0/status", json!("ok")), ("/envelope/items/1/value", json!("true")),
            ("/envelope/items/1/status", json!("ok")),
        ] {
            let mut response = fixture.rejection(code);
            *response.pointer_mut(pointer).context("fixture envelope field missing")? = value;
            if pointer == "/envelope/related" {
                let _: RpcResponse<ResultEnvelope> = serde_json::from_value(response.clone())?;
            }
            fixture.reject_malformed(response).with_context(|| format!("{code} field {pointer}"))?;
        }
        let mut response = fixture.rejection(code);
        let duplicate = response["envelope"]["items"][0].clone();
        response["envelope"]["items"].as_array_mut().context("fixture items missing")?.push(duplicate);
        fixture.reject_malformed(response)?;
    }
    Ok(())
}

#[test]
fn typed_success_and_receipt_binding_remain_unchanged() -> Result<()> {
    let mut fixture = Fixture::new()?;
    assert_eq!(credential_alias(&fixture.original)?,format!("login-{}",uuid::Uuid::parse_str(&fixture.original.operation_id)?.simple()));
    assert!(material(&fixture.original,None)?.is_none());
    assert!(checkpoint_invite(&fixture.original,None)?.is_none());
    assert!(respire_client_host::host_credentials::space::remove_legacy("foreign-owner","native","fixture-factor").is_err());
    assert!(respire_client_host::host_credentials::space::remove_legacy("login-fixture","kernel","fixture-factor").is_err());
    let accepted = fixture.validate(fixture.response.clone())?;
    assert_eq!(serde_json::to_value(accepted)?, fixture.response["envelope"]);
    for (pointer, value) in [
        ("/envelope/summary/operation_id", json!(uuid::Uuid::new_v4().to_string())),
        ("/envelope/summary/fingerprint", json!("foreign-fingerprint")),
        ("/envelope/summary/identity", json!({"kind":"bound","account_id":"foreign-account","session_sha256":"foreign-session"})),
        ("/envelope/summary/profile", json!(fixture.original.profile.parent())),
        ("/envelope/summary/operation", json!({"kind":"kick","session_id":null,"all":false})),
        ("/envelope/summary/kick", json!([])), ("/envelope/summary/remaining", json!(null)),
        ("/envelope/summary/invite_code", json!("unrelated-secret")),
    ] {
        let mut response = fixture.response.clone();
        *response.pointer_mut(pointer).context("fixture receipt field missing")? = value;
        fixture.reject_malformed(response).with_context(|| format!("receipt field {pointer}"))?;
    }
    let session = serde_json::to_vec(&json!({"user":"invite-owner","token":"fixture-session"}))?;
    std::fs::write(fixture.original.profile.join("session.json"),&session)?;
    fixture.original.source_identity = HostAuthIdentity::Bound { account_id:super::super::hash(b"invite-owner"),
        session_sha256:super::super::hash(&session) };
    fixture.original.operation = HostSpaceOperation::Invite { note:None,readonly:true };
    let sid = uuid::Uuid::new_v4().to_string();
    let code = format!("1mem-invite:{}",hex::encode(serde_json::to_vec(&json!({"space":"fixture",
        "addr":"https://fixture.invalid","user":"invite-owner","token":"fixture-token","super":"fixture-factor",
        "readonly":true,"session_id":sid,"control_operation_id":fixture.original.operation_id,
        "control_fingerprint":fixture.original.fingerprint()?}))?));
    let mut pending = fixture.response.clone();
    pending["envelope"]["status"] = json!("pending");
    pending["envelope"]["summary"] = json!({"operation_id":fixture.original.operation_id,
        "fingerprint":fixture.original.fingerprint()?,"profile":fixture.original.profile,
        "identity":fixture.original.source_identity,"operation":fixture.original.operation,"outcome":"pending",
        "invite_code":code,"session_id":sid,"target_profile":null,"target_account_id":null,
        "target_session_sha256":null,"kick":[],"remaining":null});
    fixture.validate(pending.clone())?;
    let mut unknown = pending.clone();
    unknown["ok"] = json!(false);
    unknown["exit"] = json!(2);
    unknown["code"] = json!("space_outcome_unknown");
    unknown["envelope"]["status"] = json!("fail");
    unknown["envelope"]["summary"]["outcome"] = json!("unknown");
    fixture.reject_malformed(unknown)?;
    pending["envelope"]["summary"]["session_id"] = json!(uuid::Uuid::new_v4().to_string());
    fixture.reject_malformed(pending)?;
    Ok(())
}
