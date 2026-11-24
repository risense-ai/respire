use super::*;
#[test]
fn unsent_original_query_or_rollback_cannot_admit_authentication() {
    let identity = HostAuthIdentity::Absent;
    assert!(require_unsent_admission(&AccountControl::Result, &identity, &identity).is_err());
    assert!(require_unsent_admission(&AccountControl::Rollback, &identity, &identity).is_err());
    let login = AccountControl::LoginStep {
        addr: "http://127.0.0.1:1".into(),
        user: "synthetic".into(),
        authorization: HostAuthAuthorization::Password {
            password: "synthetic-only".into(),
            totp: None,
        },
        super_password: None,
        input: HostAuthLoginContinuation::default(),
    };
    assert!(require_unsent_admission(&login, &identity, &identity).is_ok());
    assert!(require_unsent_admission(
        &login,
        &HostAuthIdentity::Bound {
            account_id: "other".into(),
            session_sha256: "00".repeat(32)
        },
        &identity
    )
    .is_err());
}
