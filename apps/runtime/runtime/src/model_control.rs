//! Bounded native client for the serving model operation. The owner credential
//! stays in this one-shot Runtime process; the host receives only the receipt.
use anyhow::{ensure, Context, Result};
use respire_commands::{net_rpc, rpc_worker};
use respire_spawn::owner_credential::{
    model_proof::{self, ModelAction, ModelClaim},
    read_owner_credential,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod install_http;

const FRAME: u64 = 4096;
const IDENTITY_FRAME: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u8,
    pid: u32,
    process_start_id: String,
    account_id: String,
    instance_id: String,
    generation: u64,
    model_id: String,
}

enum Operation<'a> {
    Remove,
    Install(&'a mut dyn Read),
}

enum NativeReply {
    Remove(ureq::Response),
    Install(u16, Value),
}

pub(crate) fn run() -> Result<i32> {
    let request: Request = read_json(std::io::stdin().lock(), FRAME)?;
    run_scoped(request, Operation::Remove)
}

pub(crate) fn run_install() -> Result<i32> {
    let mut input = std::io::stdin().lock();
    let mut header = Vec::new();
    (&mut input)
        .take(FRAME + 1)
        .read_until(b'\n', &mut header)?;
    ensure!(
        header.len() as u64 <= FRAME && header.last() == Some(&b'\n'),
        "invalid private model install envelope"
    );
    let request = read_json(header.as_slice(), FRAME)?;
    run_scoped(request, Operation::Install(&mut input))
}

fn run_scoped(request: Request, operation: Operation<'_>) -> Result<i32> {
    #[cfg(windows)]
    super::normalize_configured_data_root()?;
    let action = match &operation {
        Operation::Remove => ModelAction::Remove,
        Operation::Install(_) => ModelAction::Publish,
    };
    let instance = uuid::Uuid::parse_str(&request.instance_id).context("invalid model scope")?;
    ensure!(
        request.schema_version == 1
            && request.pid > 0
            && !request.process_start_id.is_empty()
            && request.account_id.len() == 64
            && request.generation > 0
            && instance.to_string() == request.instance_id
            && model_proof::valid_model_id(&request.model_id),
        "invalid private model request"
    );
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(155))
        .build();
    let port = net_rpc::rpc_port();
    let base = format!("http://{}:{port}", net_rpc::DEFAULT_RPC_HOST);
    let endpoint = rpc_worker::endpoint_path();
    let published = super::startup_identity::read_private_file(&endpoint)?
        .context("Runtime endpoint receipt is unavailable")?;
    let receipt: Value = read_json(published.as_slice(), IDENTITY_FRAME)?;
    let status = agent
        .get(&format!("{base}/api/health"))
        .timeout(Duration::from_secs(2))
        .call()
        .map_err(|_| anyhow::anyhow!("Runtime health is unavailable"))?;
    let health: Value = read_json(status.into_reader(), IDENTITY_FRAME)?;
    ensure!(
        net_rpc::is_our_health(&health) && health["store_ready"] == true,
        "Runtime is not ready"
    );
    let observed: net_rpc::Health = serde_json::from_value(health.clone())?;
    for (field, expected) in [
        ("pid", json!(request.pid)),
        ("process_start_id", json!(request.process_start_id)),
        ("account_id", json!(request.account_id)),
        ("instance_id", json!(request.instance_id)),
        ("generation", json!(request.generation)),
        ("url", json!(base)),
    ] {
        ensure!(
            health[field] == expected && receipt[field] == expected,
            "Runtime model scope changed"
        );
    }
    ensure!(
        Path::new(&observed.installation_root).canonicalize()?
            == respire_app::service::main_data_dir().canonicalize()?
            && Path::new(&observed.data_dir).canonicalize()?
                == respire_app::service::data_dir().canonicalize()?
            && net_rpc::pid_listening_on(port) == Some(request.pid)
            && net_rpc::instance_is_running(&observed)?,
        "Runtime model listener or profile changed"
    );
    let record = read_owner_credential(&rpc_worker::runtime_dir(), &request.instance_id)
        .map_err(|_| anyhow::anyhow!("Runtime model owner credential is unavailable"))?;
    ensure!(
        super::startup_identity::read_private_file(&endpoint)?.as_deref()
            == Some(published.as_slice()),
        "Runtime model scope changed"
    );
    let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let proof = model_proof::issue(
        record.as_bytes(),
        ModelClaim {
            action,
            model_id: &request.model_id,
            instance: *instance.as_bytes(),
            generation: request.generation,
        },
        now,
    )
    .context("Runtime model proof unavailable")?;
    drop(record);
    const UNCONFIRMED: &str =
        "Runtime model operation outcome is unconfirmed; do not retry automatically";
    let reply = match operation {
        Operation::Remove => {
            let result = agent
                .post(&format!("{base}/api/runtime/private-model/remove"))
                .set("x-respire-model-proof", proof.as_str())
                .send_json(json!({"schema_version": 1, "model_id": request.model_id}));
            let response = match result {
                Ok(value) | Err(ureq::Error::Status(_, value)) => value,
                Err(_) => anyhow::bail!(UNCONFIRMED),
            };
            NativeReply::Remove(response)
        }
        Operation::Install(input) => {
            let mut catalog = respire_app::embedding::builtin_registry()?;
            let profile = catalog
                .models
                .remove(&request.model_id)
                .context("unknown managed model")?;
            let weights = profile.weights.context("managed weights unavailable")?.size;
            let tokenizer = profile
                .tokenizer
                .context("managed tokenizer unavailable")?
                .size;
            let length = weights
                .checked_add(tokenizer)
                .context("invalid model transfer length")?;
            let (status, body) =
                install_http::install(port, proof.as_str(), &request.model_id, length, input)
                    .map_err(|_| anyhow::anyhow!(UNCONFIRMED))?;
            NativeReply::Install(status, body)
        }
    };
    ensure!(
        super::startup_identity::read_private_file(&endpoint)?.as_deref()
            == Some(published.as_slice())
            && net_rpc::pid_listening_on(port) == Some(request.pid)
            && net_rpc::instance_is_running(&observed)?,
        UNCONFIRMED
    );
    let (status, body) = match reply {
        NativeReply::Remove(response) => {
            (response.status(), read_json(response.into_reader(), FRAME)?)
        }
        NativeReply::Install(status, body) => (status, body),
    };
    serde_json::to_writer(
        std::io::stdout().lock(),
        &json!({"http_status": status, "body": body}),
    )?;
    std::io::stdout().lock().flush()?;
    Ok(0)
}

fn read_json<T: serde::de::DeserializeOwned>(source: impl Read, limit: u64) -> Result<T> {
    let mut raw = Vec::new();
    source.take(limit + 1).read_to_end(&mut raw)?;
    ensure!(
        raw.len() as u64 <= limit,
        "private model frame exceeds its bound"
    );
    serde_json::from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid private model frame"))
}
