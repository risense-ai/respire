//! Join a grant to its serving child's final supported stop receipt.
use super::*;
use respire_protocol::{RuntimeStopOutcome, RuntimeStopReceipt};
use respire_spawn::owner_credential::backup_grant::VerifiedGrant;

pub(super) fn verify_child_exit(
    child: &mut std::process::Child,
    admitted: &crate::net_rpc::Health,
    stop_operation: uuid::Uuid,
    grant: &VerifiedGrant,
    read_final: impl FnOnce() -> Result<RuntimeStopReceipt>,
) -> Result<()> {
    let scope = grant.scope();
    grant.check_fresh()?;
    ensure!(
        child.id() == admitted.pid
            && uuid::Uuid::parse_str(&admitted.instance_id)?.as_bytes() == &scope.instance
            && admitted.generation == scope.generation
            && admitted.account_id == hex::encode(scope.account)
            && <[u8; 32]>::from(Sha256::digest(
                Path::new(&admitted.data_dir)
                    .canonicalize()?
                    .as_os_str()
                    .as_encoded_bytes()
            )) == scope.profile,
        "owned child differs from admitted backup instance"
    );
    verify_final_exit(child, admitted, stop_operation, read_final)?;
    grant.check_fresh()?;
    Ok(())
}

pub(super) fn verify_final_exit(
    child: &mut std::process::Child,
    admitted: &crate::net_rpc::Health,
    stop_operation: uuid::Uuid,
    read_final: impl FnOnce() -> Result<RuntimeStopReceipt>,
) -> Result<()> {
    ensure!(child.id() == admitted.pid, "different child");
    ensure!(
        child.try_wait()?.is_some_and(|status| status.success()),
        "admitted Runtime has not exited successfully"
    );
    let receipt = read_final()?;
    ensure!(
        receipt.operation_id == stop_operation.to_string()
            && receipt.schema_version == 1
            && receipt.protocol_version == admitted.v
            && receipt.pid == admitted.pid
            && receipt.generation == admitted.generation
            && receipt.state == RuntimeStopOutcome::Drained
            && receipt.unfinished.is_empty(),
        "final receipt does not prove a clean backup drain"
    );
    for (expected, returned) in [
        (&admitted.instance_id, &receipt.instance_id),
        (&admitted.installation_id, &receipt.installation_id),
        (&admitted.process_start_id, &receipt.process_start_id),
        (&admitted.image_path, &receipt.image_path),
        (&admitted.image_sha256, &receipt.image_sha256),
        (&admitted.runtime_version, &receipt.runtime_version),
        (&admitted.account_id, &receipt.account_id),
    ] {
        ensure!(
            !expected.is_empty() && expected == returned,
            "stop receipt identity changed"
        );
    }
    Ok(())
}
