use super::*;

/// Incoming HTTP fields for `/mcp` and `/sse`.
pub struct HttpIn {
    pub method: String,
    pub path: String,
    pub origin: String,
    pub accept: String,
    pub body: String,
}

pub fn mcp_http_url(web_url: &str) -> String {
    format!("{}/mcp", web_url.trim_end_matches('/'))
}

fn invoke_runtime_cli(args: Vec<String>) -> Result<serde_json::Value, String> {
    rpc::query_json(args).map_err(|err| err.to_string())
}

fn handle_rpc_live(req: &serde_json::Value) -> Option<serde_json::Value> {
    match ToolPolicy::from_env().and_then(|policy| {
        ServiceAccess::current()
            .map(|access| (policy, access))
            .map_err(|error| format!("{error:#}"))
    }) {
        Ok((policy, access)) => handle_rpc_with(req, &invoke_runtime_cli, &policy, access),
        Err(message) => req.get("id").map(|id| {
            serde_json::json!({
                "jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":message}
            })
        }),
    }
}

fn encode_sse(event: &str, data: &str) -> Vec<u8> {
    format!("event: {event}\ndata: {data}\n\n").into_bytes()
}

/// HTTP adapter for `/mcp` (Streamable HTTP) and `/sse` (legacy SSE endpoint event).
pub fn http_response(req: HttpIn) -> (u16, &'static str, Vec<u8>) {
    http_response_with(req, &handle_rpc_live)
}

pub(super) fn http_response_with(
    req: HttpIn,
    dispatch: &dyn Fn(&serde_json::Value) -> Option<serde_json::Value>,
) -> (u16, &'static str, Vec<u8>) {
    let method = req.method.to_ascii_uppercase();
    let path = req.path.split('?').next().unwrap_or(req.path.as_str());
    let wants_sse = req
        .accept
        .to_ascii_lowercase()
        .contains("text/event-stream");
    if method == "GET" && (path == "/mcp" || path == "/sse") {
        let endpoint = mcp_http_url(&req.origin);
        return (
            200,
            "text/event-stream; charset=utf-8",
            encode_sse("endpoint", &endpoint),
        );
    }
    if method != "POST" || (path != "/mcp" && path != "/sse") {
        return (
            405,
            "text/plain; charset=utf-8",
            b"method not allowed".to_vec(),
        );
    }
    let parsed: serde_json::Value = match serde_json::from_str(req.body.trim()) {
        Ok(value) => value,
        Err(err) => {
            return (
                400,
                "application/json; charset=utf-8",
                serde_json::json!({"error": format!("request body is not JSON: {err}")})
                    .to_string()
                    .into_bytes(),
            );
        }
    };
    let payload = if parsed.is_array() {
        let items: Vec<serde_json::Value> = parsed
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(dispatch)
            .collect();
        serde_json::Value::Array(items)
    } else {
        dispatch(&parsed).unwrap_or(serde_json::json!({"jsonrpc":"2.0","result":{}}))
    };
    let body = match serde_json::to_vec(&payload) {
        Ok(bytes) => bytes,
        Err(_) => b"{\"error\":\"cannot encode response\"}".to_vec(),
    };
    if wants_sse {
        let data = String::from_utf8_lossy(&body).into_owned();
        return (
            200,
            "text/event-stream; charset=utf-8",
            encode_sse("message", &data),
        );
    }
    (200, "application/json; charset=utf-8", body)
}
