use super::*;
use std::path::{Path, PathBuf};

pub(super) enum AgentConfigJournal<'a> {
    Online(&'a mut OnlineAgentConfig),
    Legacy(&'a mut super::super::lifecycle::Maintenance),
}

impl AgentConfigJournal<'_> {
    pub(super) fn maintenance(&mut self) -> Option<&mut super::super::lifecycle::Maintenance> {
        match self {
            Self::Legacy(maintenance) => Some(maintenance),
            Self::Online(_) => None,
        }
    }

    pub(super) fn check(&self, record: &AgentConfigMaintenance) -> Result<()> {
        match self {
            Self::Legacy(maintenance) => maintenance.check_agent_config_record(record),
            Self::Online(_) => encode(record).map(|_| ()),
        }
    }

    pub(super) fn save(&mut self, record: &AgentConfigMaintenance) -> Result<()> {
        match self {
            Self::Legacy(maintenance) => maintenance.save_agent_config(record),
            Self::Online(journal) => journal.save(record),
        }
    }
}

const RECORD_LIMIT: usize = 16 * 1024;

fn encode(record: &AgentConfigMaintenance) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(record)?;
    anyhow::ensure!(bytes.len() <= RECORD_LIMIT, "agent config transaction exceeds its record bound");
    Ok(bytes)
}

pub(super) struct OnlineAgentConfig {
    path: PathBuf,
    digest: Option<String>,
    pub(super) record: Option<AgentConfigMaintenance>,
}

impl OnlineAgentConfig {
    pub(super) fn read(root: &Path) -> Result<Self> {
        let path = root.join("runtime/autostart/agent-config-transaction.json");
        respire_client_host::config::normalize_control_path(&path)?;
        let bytes = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink()
                    && metadata.len() <= RECORD_LIMIT as u64, "invalid agent config transaction file");
                Some(std::fs::read(&path)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let record = bytes.as_deref().map(serde_json::from_slice).transpose()?;
        let digest = bytes.as_deref().map(crate::autostart::hash);
        Ok(Self { path, digest, record })
    }

    fn save(&mut self, record: &AgentConfigMaintenance) -> Result<()> {
        let bytes = encode(record)?;
        crate::autostart::private_write(&self.path, &bytes, self.digest.as_deref())?;
        self.digest = Some(crate::autostart::hash(&bytes));
        Ok(())
    }

    pub(super) fn start(mut self, review: AgentConfigReview, owner: Option<&crate::net_rpc::Health>) -> Result<ResultEnvelope> {
        let mut record = AgentConfigMaintenance { review, was_serving: owner.is_some(),
            store_ready: owner.is_some_and(|health| health.store_ready), sent: false, receipt: None };
        if is_core(&record) {
            let result = control_core_setting(&mut record, &mut AgentConfigJournal::Online(&mut self), owner, false);
            return self.complete_core(&record, result);
        }
        let intent = &record.review.intent;
        let output = ResultEnvelope::new("agent-config", Status::Ok,
            serde_json::json!({"action":"updated", "key":intent.key, "value":intent.value()}),
            vec![respire_commands::output::Item::new(&intent.key, Status::Ok, &intent.raw_value)]);
        let receipt = AgentConfigReceipt { review: record.review.clone(), outcome: AgentConfigOutcome::Committed,
            result: serde_json::to_value(&output)? };
        let mut committed = record.clone();
        committed.sent = true;
        committed.receipt = Some(receipt.clone());
        encode(&committed)?;
        record.sent = true;
        self.save(&record)?;
        crate::client_config::publish_agent_config(&record.review, || self.save(&committed))?;
        crate::client_config::verify_agent_config_receipt(&receipt)?;
        self.finish()?;
        Ok(output)
    }

    pub(super) fn resume(mut self, intent: Option<&AgentConfigIntent>, requested: Option<&str>,
        query: Option<(&respire_commands::runtime_error::OperationContext, &Path)>,
        profile: &Path, identity: &respire_protocol::metadata_control::HostMetadataIdentity,
    ) -> Result<ResultEnvelope> {
        let mut record = self.record.take().context("agent config transaction missing")?;
        anyhow::ensure!(profile == record.review.profile && *identity == record.review.identity
            && requested.is_none_or(|id| id == record.review.operation_id)
            && intent.is_none_or(|intent| *intent == record.review.intent),
            "repeat the original agent-config intent and identity; another operation is pending");
        let receipt = record.receipt.as_ref();
        if is_core(&record) {
            let control: CoreConfigTransaction = serde_json::from_value(receipt.context(
                "original core configuration receipt unavailable")?.result["core_control"].clone())?;
            if let Some((context, source)) = query {
                anyhow::ensure!(source.canonicalize()? == profile && context.operation_id == context.idempotency_key
                    && control.scope.as_ref() == Some(&context.scope), "core configuration query identity mismatch");
            }
            if receipt.is_some_and(|receipt| receipt.outcome == AgentConfigOutcome::Unknown) {
                return Err(CoreConfigUnknown { details: core_transaction_details(&record)? }.into());
            }
            let owner = crate::rpc::probe_runtime()?;
            let query_only = query.is_some() || intent.is_none() || control.apply_sent;
            if !query_only {
                super::super::lifecycle::check_direct_access()?;
            }
            let result = control_core_setting(&mut record, &mut AgentConfigJournal::Online(&mut self), owner.as_ref(), query_only);
            return self.complete_core(&record, result);
        }
        anyhow::ensure!(query.is_none() && requested.is_none(), "original operation is not a core configuration write");
        let receipt = receipt.with_context(|| format!("agent_config_outcome_unknown operation_id={}; do not replay", record.review.operation_id))?;
        crate::client_config::verify_agent_config_receipt(receipt)?;
        let output = serde_json::from_value(receipt.result.clone())?;
        self.finish()?;
        Ok(output)
    }

    fn complete_core(self, record: &AgentConfigMaintenance, result: Result<ResultEnvelope>) -> Result<ResultEnvelope> {
        let completion = (|| {
            let mut output = result?;
            output.details = core_transaction_details(record)?;
            let path = core_result_path(&record.review.installation_root, &record.review.operation_id)?;
            let bytes = serde_json::to_vec(&output)?;
            match std::fs::read(&path) {
                Ok(previous) => anyhow::ensure!(previous == bytes, "core configuration terminal receipt changed"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    crate::autostart::private_write(&path, &bytes, None)?;
                }
                Err(error) => return Err(error.into()),
            }
            self.finish()?;
            Ok(output)
        })();
        if record.sent && completion.is_err() {
            let details = core_transaction_details(record)?;
            return completion.map_err(|error: anyhow::Error| error.context(CoreConfigUnknown { details }));
        }
        completion
    }

    fn finish(&self) -> Result<()> {
        anyhow::ensure!(Some(crate::autostart::hash(&std::fs::read(&self.path)?)) == self.digest,
            "agent config transaction changed before completion");
        std::fs::remove_file(&self.path)?;
        respire_spawn::durable::sync_directory(self.path.parent().context("agent config transaction parent missing")?)?;
        Ok(())
    }
}

fn is_core(record: &AgentConfigMaintenance) -> bool {
    matches!(record.review.intent.key.trim(), "read_heat" | "recall_fusion")
}
