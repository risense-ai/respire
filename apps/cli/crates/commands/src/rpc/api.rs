use super::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use respire_runtime_sdk::{types::HealthResponse, Client, Error};

/// Keep legacy ownership evidence: V1 health does not include executable or profile identity.
fn negotiated_health() -> Result<Health> {
    check_connection_to(std::net::SocketAddr::from((
        [127, 0, 0, 1],
        rpc_port(),
    )))?;
    let advertised = discover(&rpc_base_url(), read_token()?)?;
    let owner = health()?;
    if let Some(advertised) = advertised.filter(supports_v1) {
        anyhow::ensure!(
            !advertised.instance_id.trim().is_empty()
                && advertised.instance_id == owner.instance_id,
            "Runtime instance changed during capability negotiation"
        );
    }
    Ok(owner)
}

pub fn command_roundtrip(method: &str, args: &[String], cli_version: &str) -> Result<respire_protocol::RpcResponse<crate::output::ResultEnvelope>> {
    let (id, version, health) = match method {
        "runtime.status" => {
            let health = negotiated_health()?;
            ("health", health.bin.clone(), Some(health))
        }
        "runtime.stop" => {
            request_stop()?;
            ("stop", cli_version.to_owned(), None)
        }
        "cli.exec" | "cli.progress" | "model.control" | "model.prepare" | "index.prepare"
        | "model.catalog" | "model.configure" | "model.switch" | "model.resume"
        | "model.progress" => {
            let response = rpc_method(method, args.to_vec())?;
            return serde_json::from_value(response).context("runtime rpc response shape mismatch");
        }
        other => bail!("unknown runtime method {other}"),
    };
    Ok(respire_protocol::RpcResponse {
        v: health.as_ref().map_or(crate::rpc_client::PROTOCOL_V, |health| health.v),
        id: id.into(),
        operation_id: None,
        scope: None,
        idempotency_key: None,
        ok: true,
        exit: 0,
        bin: version,
        envelope: None,
        error: None,
        code: None,
        web_url: health.as_ref().map(|health| health.url.clone()),
        pid: health.as_ref().map(|health| health.pid),
        data_dir: health.map(|health| health.data_dir),
    })
}

fn supports_v1(health: &HealthResponse) -> bool {
    health.capabilities.iter().any(|flag| flag == "openapi.v1")
}

fn discover(base: &str, token: Option<String>) -> Result<Option<HealthResponse>> {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()?;
        let client = Client::new_with_client(base, http);
        match client.get_health().await {
            Ok(response) => Ok(Some(response.into_inner())),
            Err(Error::ErrorResponse(response)) if response.status().as_u16() == 404 => Ok(None),
            Err(Error::ErrorResponse(response)) if matches!(response.status().as_u16(), 401 | 403) => {
                Err(RuntimeError::Unauthorized.into())
            }
            Err(error) => Err(RuntimeError::Transport(format!(
                "Runtime capability discovery failed: {error}"
            ))
            .into()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn discovery_uses_capabilities_and_only_falls_back_on_404() -> Result<()> {
        for (status, body, expected) in [
            (
                200,
                r#"{"status":"ready","instance_id":"test","capabilities":["openapi.v1"]}"#,
                "v1",
            ),
            (
                200,
                r#"{"status":"ready","instance_id":"test","capabilities":[]}"#,
                "legacy",
            ),
            (404, "{}", "absent"),
            (401, "{}", "unauthorized"),
            (403, "{}", "unauthorized"),
            (503, "{}", "error"),
            (302, "{}", "error"),
            (200, "{}", "error"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let base = format!("http://{}", listener.local_addr()?);
            let worker = std::thread::spawn(move || -> Result<()> {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut line = String::new();
                reader.read_line(&mut line)?;
                anyhow::ensure!(
                    line.starts_with("GET /v1/health HTTP/1.1"),
                    "wrong discovery route"
                );
                let mut authorized = false;
                loop {
                    line.clear();
                    let read = reader.read_line(&mut line)?;
                    anyhow::ensure!(read != 0, "incomplete discovery request");
                    if line == "\r\n" {
                        break;
                    }
                    authorized |= line
                        .trim()
                        .eq_ignore_ascii_case("authorization: Bearer fixture-bearer");
                }
                anyhow::ensure!(authorized, "existing bearer was not preserved");
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
                Ok(())
            });
            let result = discover(&base, Some("fixture-bearer".into()));
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("discovery fixture panicked"))??;
            match expected {
                "v1" => assert!(supports_v1(&result?.context("V1 health missing")?)),
                "legacy" => assert!(!supports_v1(&result?.context("legacy health missing")?)),
                "absent" => assert!(result?.is_none()),
                "unauthorized" => assert!(matches!(
                    result.err().and_then(|error| error
                        .downcast::<crate::runtime_error::RuntimeError>()
                        .ok()),
                    Some(crate::runtime_error::RuntimeError::Unauthorized)
                )),
                _ => assert!(
                    result.is_err(),
                    "HTTP {status} must not silently use legacy transport"
                ),
            }
        }
        Ok(())
    }
}
