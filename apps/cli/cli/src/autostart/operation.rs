use anyhow::{bail, ensure, Context, Result};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use respire_runtime_sdk::{
    types::{HealthResponse, OperationEvent, OperationStatus, OperationStatusState as State},
    Client,
};
use std::time::Duration;

pub(super) fn inspect(id: &str, follow: bool) -> Result<()> {
    let result = operation_result(id, follow)?;
    let failed = matches!(result.status, crate::OutputStatus::Fail);
    crate::emit_result(result)?;
    if failed {
        crate::set_exit_code(1);
    }
    Ok(())
}

fn operation_result(id: &str, follow: bool) -> Result<crate::output::ResultEnvelope> {
    let id = uuid::Uuid::parse_str(id).context("invalid operation ID")?;
    let mut headers = HeaderMap::new();
    let token = crate::net_rpc::read_token()?.context("Runtime bearer required")?;
    let mut bearer =
        HeaderValue::from_str(&format!("Bearer {token}")).context("invalid Runtime bearer")?;
    bearer.set_sensitive(true);
    headers.insert(AUTHORIZATION, bearer);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let snapshot = runtime.block_on(async {
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        tokio::time::timeout(
            Duration::from_secs(30),
            observe(http, &id.to_string(), follow),
        )
        .await
        .context("Runtime operation observation deadline exceeded")?
    })?;
    let failed = matches!(snapshot.state, State::Cancelled | State::Failed);
    let status = if failed {
        crate::OutputStatus::Fail
    } else {
        crate::OutputStatus::Ok
    };
    Ok(crate::output::ResultEnvelope::new(
        "runtime.operation",
        status,
        serde_json::to_value(snapshot)?,
        Vec::new(),
    ))
}

const JSON_LIMIT: usize = 64 * 1024;

// The generated JSON methods buffer the entire body. Decode their generated
// response types only after enforcing the wire limit, including chunked bodies.
async fn bounded_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    health: bool,
) -> Result<T> {
    let mut response = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    if health && response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("legacy Runtime has no frozen operations endpoint");
    }
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "Runtime returned HTTP {}",
        response.status()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|len| len <= JSON_LIMIT as u64),
        "Runtime JSON exceeds 64 KiB"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= JSON_LIMIT - bytes.len(),
            "Runtime JSON exceeds 64 KiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn instance(http: &reqwest::Client, base: &str, follow: bool) -> Result<String> {
    let health: HealthResponse = bounded_json(http, &format!("{base}/v1/health"), true).await?;
    let has_sse = health
        .capabilities
        .iter()
        .any(|cap| cap == "sse.operations.v1");
    if follow {
        ensure!(has_sse, "Runtime does not advertise sse.operations.v1");
    } else {
        ensure!(
            has_sse || health.capabilities.iter().any(|cap| cap == "openapi.v1"),
            "Runtime does not advertise OpenAPI operation snapshots"
        );
    }
    ensure!(
        !health.instance_id.trim().is_empty(),
        "Runtime instance ID missing"
    );
    Ok(health.instance_id)
}

async fn observe(http: reqwest::Client, id: &str, follow: bool) -> Result<OperationStatus> {
    let base = crate::net_rpc::rpc_base_url();
    let sdk = Client::new_with_client(&base, http.clone());
    let original = instance(&http, &base, follow).await?;
    let url = format!("{base}/v1/operations/{id}");
    for attempt in 0..=1 {
        if attempt > 0 {
            ensure!(
                instance(&http, &base, follow).await? == original,
                "Runtime instance changed"
            );
        }
        let snapshot: OperationStatus = bounded_json(&http, &url, false).await?;
        ensure!(
            snapshot.operation_id == id,
            "Runtime returned a different operation"
        );
        ensure!(
            instance(&http, &base, follow).await? == original,
            "Runtime instance changed"
        );
        if !follow || terminal(snapshot.state) {
            return Ok(snapshot);
        }
        if let Some(end) = follow_events(&sdk, id).await? {
            ensure!(
                instance(&http, &base, follow).await? == original,
                "Runtime instance changed"
            );
            return Ok(end);
        }
    }
    bail!("Runtime event stream disconnected repeatedly")
}

fn terminal(state: State) -> bool {
    matches!(state, State::Completed | State::Failed | State::Cancelled)
}

async fn follow_events(client: &Client, id: &str) -> Result<Option<OperationStatus>> {
    let response = client.stream_operation_events(id).await?;
    ensure!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            == Some("text/event-stream"),
        "Runtime returned an unexpected event stream type"
    );
    let mut stream = response.into_inner();
    let mut line = Vec::new();
    let mut name = String::new();
    let mut data = String::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { return Ok(None) };
        // Let the absolute observation timer run even for a continuously ready stream.
        tokio::task::yield_now().await;
        for byte in chunk {
            if byte != b'\n' {
                ensure!(line.len() < 65536, "Runtime event line exceeds 64 KiB");
                line.push(byte);
                continue;
            }
            let field = std::str::from_utf8(&line)?.trim_end_matches('\r');
            if field.is_empty() {
                if !data.is_empty() {
                    let event: OperationEvent = serde_json::from_str(&data)?;
                    let state = event.data.state;
                    let expected = if terminal(state) {
                        state.to_string()
                    } else {
                        "progress".into()
                    };
                    ensure!(
                        event.data.operation_id == id
                            && name == expected
                            && event.event.to_string() == expected,
                        "Runtime event identity or state mismatch"
                    );
                    if terminal(state) {
                        return Ok(Some(event.data));
                    }
                }
                name.clear();
                data.clear();
            } else if let Some((key, value)) = field.split_once(':') {
                let value = value.strip_prefix(' ').unwrap_or(value);
                match key {
                    "event" => name = value.to_owned(),
                    "data" => {
                        ensure!(
                            data.len() + value.len() < 65536,
                            "Runtime event exceeds 64 KiB"
                        );
                        data.push_str(value);
                        data.push('\n');
                    }
                    _ => {}
                }
            }
            line.clear();
        }
    }
    Ok(None)
}
