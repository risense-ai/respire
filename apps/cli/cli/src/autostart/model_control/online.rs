use super::{model_mirror_origin, Context, Duration, ModelInstallIntent, Result};
use crate::net_rpc::{openapi_health, openapi_optional};
use respire_runtime_sdk::{
    types::{DownloadModelRequest, OperationStatus, OperationStatusState},
    Client,
};

pub(super) async fn download_m3(
    client: &Client,
    instance: &str,
    intent: &ModelInstallIntent,
) -> Result<Option<OperationStatus>> {
    let advertised = openapi_health(client, Some(instance)).await?;
    if !advertised.is_some_and(|health| health.capabilities.iter().any(|flag| flag == "openapi.v1"))
    {
        return Ok(None);
    }
    let request = DownloadModelRequest {
        model_id: "m3".into(),
        variant: None,
        engine: intent.for_engine.clone(),
        mirror: intent
            .mirror
            .as_deref()
            .filter(|mirror| mirror.trim() != "auto")
            .map(model_mirror_origin)
            .transpose()?,
    };
    let Some(accepted) = openapi_optional(
        client.start_model_download(&request).await,
        "model download",
    )?
    else {
        return Ok(None);
    };
    let id = uuid::Uuid::parse_str(&accepted.operation_id)
        .context("invalid model download operation ID")?;
    let id = id.to_string();
    loop {
        let status = openapi_optional(client.get_operation(&id).await, "model download status")?
            .with_context(|| format!("Runtime lost model download operation {id}"))?;
        anyhow::ensure!(
            status.operation_id == id && status.kind == "model_download",
            "Runtime returned a different model download operation"
        );
        match status.state {
            OperationStatusState::Completed => return Ok(Some(status)),
            OperationStatusState::Pending | OperationStatusState::Running => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            OperationStatusState::Failed | OperationStatusState::Cancelled => {
                let reason = status
                    .error
                    .as_ref()
                    .and_then(Option::as_ref)
                    .map_or("no error code", |error| error.code.as_str());
                anyhow::bail!(
                    "Runtime model download {id} ended in {} ({reason})",
                    status.state
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Write;

    #[test]
    fn model_download_polls_and_only_falls_back_before_acceptance() -> Result<()> {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let health =
            json!({"status":"ready","instance_id":"fixture","capabilities":["openapi.v1"]});
        let accepted = json!({"operation_id":id,"status":"pending"});
        let snapshot = |state| json!({"operation_id":id,"kind":"model_download","state":state});
        let operation_path = format!("/v1/operations/{id}");
        let intent = ModelInstallIntent {
            mirror: Some("https://mirror.invalid".into()),
            for_engine: Some("cpu".into()),
            ..Default::default()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        for (responses, expected) in [
            (vec![("/v1/health", 404, json!({}))], "legacy"),
            (
                vec![
                    ("/v1/health", 200, health.clone()),
                    ("/v1/models/downloads", 404, json!({})),
                ],
                "legacy",
            ),
            (
                vec![
                    ("/v1/health", 200, health.clone()),
                    ("/v1/models/downloads", 409, json!({})),
                ],
                "error",
            ),
            (
                vec![
                    ("/v1/health", 200, health.clone()),
                    ("/v1/models/downloads", 202, accepted.clone()),
                    (&operation_path, 404, json!({})),
                ],
                "error",
            ),
            (
                vec![
                    ("/v1/health", 200, health.clone()),
                    ("/v1/models/downloads", 202, accepted.clone()),
                    (&operation_path, 200, snapshot("failed")),
                ],
                "error",
            ),
            (
                vec![
                    ("/v1/health", 200, health.clone()),
                    ("/v1/models/downloads", 202, accepted.clone()),
                    (&operation_path, 200, snapshot("running")),
                    (&operation_path, 200, snapshot("completed")),
                ],
                "completed",
            ),
        ] {
            let server = crate::loopback_http::Server::bind()?;
            let client =
                crate::net_rpc::openapi_client(&format!("http://{}", server.addr()), None)?;
            std::thread::scope(|scope| -> Result<()> {
                let worker = scope.spawn(|| -> Result<()> {
                    for (path, status, body) in &responses {
                        let request = server.recv_timeout(Duration::from_secs(3))?.context("missing download request")?;
                        assert_eq!(request.url(), *path);
                        if *path == "/v1/models/downloads" {
                            assert_eq!(serde_json::from_slice::<Value>(request.body())?,
                                json!({"model_id":"m3","engine":"cpu","mirror":"https://mirror.invalid"}));
                        }
                        let body = body.to_string();
                        write!(request.into_stream(), "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
                    }
                    Ok(())
                });
                let outcome = runtime.block_on(download_m3(&client, "fixture", &intent));
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("download fixture panicked"))??;
                match expected {
                    "legacy" => assert!(outcome?.is_none()),
                    "error" => assert!(outcome.is_err()),
                    _ => assert_eq!(
                        outcome?.context("missing completion")?.state,
                        OperationStatusState::Completed
                    ),
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}
