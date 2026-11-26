//! Host configuration transactions and the network policy used by model downloads.
use super::host_config_patch::patch;
use super::{
    registration::{bind, RouteSpec, Verb},
    rest_dispatch, transport, V1Result,
};
use anyhow::{ensure, Result};
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::Response,
    Router,
};
use respire_app::service;
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};

pub(super) const ORIGINS: &[&str] = &[
    "https://hf-mirror.com",
    "https://hf-mirror.net",
    "https://huggingface.co",
];

pub(super) fn e2e_loopback_origin() -> Result<Option<String>> {
    #[cfg(not(feature = "e2e-loopback-mirror"))]
    {
        Ok(None)
    }
    #[cfg(feature = "e2e-loopback-mirror")]
    {
        let Ok(raw) = respire_app::env::var("RSRS_E2E_LOOPBACK_MIRROR") else {
            return Ok(None);
        };
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let url =
            url::Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid e2e loopback mirror"))?;
        let port = url
            .port()
            .filter(|port| *port != 0)
            .ok_or_else(|| anyhow::anyhow!("invalid e2e loopback mirror"))?;
        ensure!(
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.username().is_empty()
                && url.password().is_none()
                && matches!(url.path(), "" | "/")
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid e2e loopback mirror"
        );
        Ok(Some(format!("http://127.0.0.1:{port}")))
    }
}

pub(super) struct ModelNetwork {
    mirror: String,
    proxy_url: Option<String>,
}

impl ModelNetwork {
    pub(super) fn read() -> Result<Self> {
        Self::parse(&service::read_client_config_for_write()?)
    }

    pub(super) fn parse(config: &Value) -> Result<Self> {
        let mirror = match config.get("model_mirror") {
            None => "auto",
            Some(value) => value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid model mirror"))?,
        };
        let loopback = e2e_loopback_origin()?;
        ensure!(
            mirror == "auto" || ORIGINS.contains(&mirror) || loopback.as_deref() == Some(mirror),
            "invalid model mirror"
        );
        let proxy_url = match config.get("proxy_url") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => {
                let url =
                    url::Url::parse(value).map_err(|_| anyhow::anyhow!("invalid model proxy"))?;
                ensure!(
                    matches!(url.scheme(), "http" | "socks4" | "socks4a" | "socks5")
                        && url.host_str().is_some()
                        && !matches!(url.host(), Some(url::Host::Ipv6(_)))
                        && url.username().is_empty()
                        && url.password().is_none()
                        && matches!(url.path(), "" | "/")
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "invalid model proxy"
                );
                ureq::Proxy::new(value).map_err(|_| anyhow::anyhow!("invalid model proxy"))?;
                Some(value.clone())
            }
            _ => anyhow::bail!("invalid model proxy"),
        };
        Ok(Self {
            mirror: mirror.into(),
            proxy_url,
        })
    }

    pub(super) fn origins(&self, requested: Option<&str>) -> Result<Vec<String>> {
        let loopback = e2e_loopback_origin()?;
        let chosen = requested.or_else(|| (self.mirror != "auto").then_some(self.mirror.as_str()));
        if let Some(origin) = loopback {
            return match chosen {
                None => Ok(vec![origin]),
                Some(selected) if selected == origin => Ok(vec![origin]),
                _ => anyhow::bail!(
                    "e2e loopback mirror mode permits only the configured loopback origin"
                ),
            };
        }
        match chosen {
            None => Ok(ORIGINS.iter().map(|origin| (*origin).into()).collect()),
            Some(origin) if ORIGINS.contains(&origin) => Ok(vec![origin.into()]),
            _ => anyhow::bail!("mirror must be an approved canonical origin"),
        }
    }

    pub(super) fn agent(&self) -> Result<ureq::Agent> {
        let loopback = e2e_loopback_origin()?;
        let mut builder = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(10));
        if loopback.is_some() {
            ensure!(
                self.proxy_url.is_none(),
                "proxy_url is not allowed in e2e loopback mirror mode"
            );
            builder = builder.try_proxy_from_env(false);
        } else if let Some(proxy) = &self.proxy_url {
            builder = builder.proxy(
                ureq::Proxy::new(proxy).map_err(|_| anyhow::anyhow!("invalid model proxy"))?,
            );
        }
        Ok(builder.build())
    }
}

pub(super) fn routes(bound: SocketAddr) -> Router {
    let router = bind(
        Router::new(),
        RouteSpec::new(Verb::Get, "/v1/host/config", "getHostConfig"),
        get,
    );
    bind(
        router,
        RouteSpec::new(Verb::Patch, "/v1/host/config", "patchHostConfig"),
        patch,
    )
    .method_not_allowed_fallback(super::method_not_allowed)
    .with_state(bound)
}

pub(super) fn view(client: &Value, agent: &Value) -> Result<Value> {
    let network = ModelNetwork::parse(client)?;
    let diary = match agent.get("diary_mode") {
        None => "concise",
        Some(value) => value
            .as_str()
            .filter(|mode| matches!(*mode, "concise" | "verbose"))
            .ok_or_else(|| anyhow::anyhow!("invalid diary mode"))?,
    };
    let language = match client.get("lang") {
        None => crate::i18n::Lang::En,
        Some(value) => value
            .as_str()
            .and_then(crate::i18n::parse_lang)
            .ok_or_else(|| anyhow::anyhow!("invalid language"))?,
    };
    let language = match language {
        crate::i18n::Lang::En => "en",
        crate::i18n::Lang::Zh => "zh",
    };
    let language = match respire_app::env::var("RSRS_LANG")
        .ok()
        .as_deref()
        .and_then(crate::i18n::parse_lang)
    {
        Some(crate::i18n::Lang::En) => "en",
        Some(crate::i18n::Lang::Zh) => "zh",
        None => language,
    };
    Ok(
        json!({"language":language, "model_mirror":network.mirror, "diary_mode":diary, "proxy_url":network.proxy_url}),
    )
}

async fn get(
    State(bound): State<SocketAddr>,
    ConnectInfo(crate::network::Peer(peer)): ConnectInfo<crate::network::Peer>,
    request: Request<Body>,
) -> V1Result<Response> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::GET {
        return Err(super::method_not_allowed().await);
    }
    let scope = transport::authorize(&parts, peer, bound).await?;
    if parts.uri.query().is_some() {
        return Err(transport::invalid());
    }
    transport::json_optional::<Value>(body, 0).await?;
    let config = rest_dispatch::submit(scope, rest_dispatch::Access::Read, |ctx| {
        let transaction = service::HostConfigTransaction::begin(ctx.ownership)
            .map_err(|_| transport::unavailable())?;
        view(&transaction.client, &transaction.agent).map_err(|_| transport::unavailable())
    })
    .await?;
    transport::json_response(StatusCode::OK, config)
}
