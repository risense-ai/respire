use super::{
    super::{rest_dispatch, transport, transport::RestScope, Problem, V1Result},
    model_switch,
    state::{self, Job, Phase},
};
use crate::{model_progress, rpc_serve::ModelSwitchReservation};
use anyhow::{Context, Result};
use axum::{http::StatusCode, response::Response};
use respire_app::{
    core_sdk::{
        onnx::Engine,
        profiles::{self, Resource},
    },
    embedding::{self, Choice},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    model_id: String,
    #[serde(default)]
    engine: Engine,
    mirror: Option<String>,
}

#[cfg(test)]
#[path = "model_download_tests.rs"]
pub(super) mod tests;


pub(super) async fn enqueue(scope: RestScope, input: Request) -> V1Result<Response> {
    rest_dispatch::submit(scope, rest_dispatch::Access::Write, move |ctx| {
        model_switch::check_mode().map_err(|_| transport::unavailable())?;
        let network = super::super::host_config::ModelNetwork::read()
            .map_err(|_| transport::unavailable())?;
        let origins = network.origins(input.mirror.as_deref()).map_err(|_| transport::invalid())?;
        let agent = network.agent().map_err(|_| transport::unavailable())?;
        // Local registry overrides are inputs to inference, never download authority.
        let registry = embedding::builtin_registry().map_err(|_| transport::unavailable())?;
        let target = embedding::select_from_registry(
            Choice {
                model: input.model_id,
                backend: "onnx".into(),
                engine: input.engine,
                variant: None,
            },
            &registry,
        )
        .ok()
        .filter(|s| s.engine == Engine::Cpu)
        .ok_or_else(|| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                "invalid_model",
                "Invalid model or engine",
            )
        })?;
        let resources = [target.profile.tokenizer, target.profile.weights]
            .into_iter()
            .map(|r| r.context("catalog resource missing"))
            .collect::<Result<Vec<_>>>()
            .map_err(|_| transport::unavailable())?;
        for resource in &resources {
            validate_resource(resource).map_err(|_| transport::unavailable())?;
        }
        launch(ctx.scope.clone(), resources, origins, move |url, output, resource, progress| {
            transfer(&agent, url, output, resource, progress)
        })
    })
    .await
}

type Fetch = dyn Fn(&str, &mut std::fs::File, &Resource, &dyn Fn(u64) -> Result<()>) -> Result<()> + Send;

fn launch(
    scope: RestScope,
    resources: Vec<Resource>,
    origins: Vec<String>,
    fetch: impl Fn(&str, &mut std::fs::File, &Resource, &dyn Fn(u64) -> Result<()>) -> Result<()> + Send + 'static,
) -> V1Result<Response> {
    let job = state::registry().begin(scope, "model_download")?;
    let reservation = ModelSwitchReservation::reserve(job.id()?)
        .map_err(|_| transport::unavailable())?
        .ok_or_else(|| {
            Problem::new(
                StatusCode::CONFLICT,
                "model_busy",
                "Model operation in progress",
            )
        })?;
    let receipt = transport::json_response(StatusCode::ACCEPTED, job.accepted()?)?;
    std::thread::Builder::new()
        .name("respire-model-download".into())
        .spawn(move || {
            reservation.enter();
            let result = run(&job, &resources, &origins, &fetch);
            let phase = match result {
                Ok(()) => Phase::Completed,
                Err(ref error)
                    if matches!(
                        error.downcast_ref::<model_progress::OperationStopped>(),
                        Some(model_progress::OperationStopped::Cancelled)
                    ) =>
                {
                    Phase::Cancelled
                }
                Err(_) => Phase::Failed,
            };
            if let Err(error) = &result {
                eprintln!("command=model_download status=fail error={error:#}");
            }
            if result
                .as_ref()
                .err()
                .is_some_and(|e| e.is::<SourcesExhausted>())
            {
                job.fail("model_mirror_fallback_exhausted");
            } else {
                job.finish(phase);
            }
            drop(reservation);
        })
        .map_err(|_| transport::unavailable())?;
    Ok(receipt)
}

#[cfg(test)]
fn origins(mirror: Option<&str>) -> Result<Vec<String>> {
    super::super::host_config::ModelNetwork::parse(&serde_json::json!({}))?.origins(mirror)
}

fn validate_resource(resource: &Resource) -> Result<()> {
    check_path(&resource.path)?;
    anyhow::ensure!(
        resource.size > 0
            && resource.sha256.len() == 64
            && resource.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid artifact pin"
    );
    let source = resource
        .download
        .as_deref()
        .context("catalog source missing")?;
    let relative = source
        .strip_prefix("https://huggingface.co/")
        .context("catalog source must use its official HTTPS origin")?;
    let parts: Vec<_> = relative.split('/').collect();
    anyhow::ensure!(
        parts.len() >= 5
            && parts[2] == "resolve"
            && parts[3].len() == 40
            && parts[3].bytes().all(|b| b.is_ascii_hexdigit())
            && parts.iter().all(|p| !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            && resource.path.file_name().and_then(|p| p.to_str()) == parts.last().copied(),
        "catalog source must have an immutable revision and matching filename"
    );
    Ok(())
}

// Match the host installer's no-link checks, including missing path components.
fn check_path(path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute() && path.to_str().is_some(),
        "model path must be absolute Unicode"
    );
    let mut prefix = PathBuf::new();
    for component in path.components() {
        anyhow::ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "model path contains dot components"
        );
        if let Component::Normal(name) = component {
            let name = name.to_str().context("model path is not Unicode")?;
            anyhow::ensure!(
                !name.contains([':', '\\']) && !name.ends_with(['.', ' ']),
                "ambiguous model path"
            );
        }
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) => {
                anyhow::ensure!(
                    !metadata.file_type().is_symlink(),
                    "model path follows a symlink"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    anyhow::ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "model path follows a reparse point"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn checkpoint(job: &Job) -> Result<()> {
    model_progress::check()?;
    anyhow::ensure!(
        !job.entry()
            .cancelled()
            .map_err(|_| anyhow::anyhow!("operation state unavailable"))?,
        model_progress::OperationStopped::Cancelled
    );
    anyhow::ensure!(
        job.scope().bearer_is_current(),
        "local bearer changed during model download"
    );
    model_switch::with_authority(job.scope(), |_| Ok(()))
}

fn valid(resource: &Resource, check: &impl Fn() -> Result<()>) -> Result<bool> {
    check_path(&resource.path)?;
    let mut file = match std::fs::File::open(&resource.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "model asset is not a regular file"
    );
    if file.metadata()?.len() != resource.size {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        model_progress::update("verify", "model asset", 0, Some(resource.size))?;
    }
    Ok(format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&resource.sha256))
}

fn run(job: &Job, resources: &[Resource], origins: &[String], fetch: &Fetch) -> Result<()> {
    checkpoint(job)?;
    if !job
        .start()
        .map_err(|_| anyhow::anyhow!("operation state unavailable"))?
    {
        return Err(model_progress::OperationStopped::Cancelled.into());
    }
    let check = || checkpoint(job);
    let mut staged = Vec::new();
    for (index, resource) in resources.iter().enumerate() {
        validate_resource(resource)?;
        if valid(resource, &check)? {
            continue;
        }
        // Never replace an existing artifact that a loaded worker could still use.
        anyhow::ensure!(
            !resource.path.try_exists()?,
            "existing model asset fails integrity; refusing replacement"
        );
        let parent = resource.path.parent().context("model parent missing")?;
        std::fs::create_dir_all(parent)?;
        check_path(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let source = resource
            .download
            .as_deref()
            .context("catalog source missing")?;
        let relative = source
            .strip_prefix("https://huggingface.co")
            .context("catalog source origin")?;
        let mut transferred = false;
        for origin in origins {
            check()?;
            temp.as_file_mut().set_len(0)?;
            use std::io::{Seek, SeekFrom};
            temp.as_file_mut().seek(SeekFrom::Start(0))?;
            let progress = |done: u64| -> Result<()> {
                check()?;
                model_progress::update(
                    "download",
                    resource
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .context("model file name missing")?,
                    done,
                    Some(resource.size),
                )?;
                job.progress(
                    ((index as u64 * 90 + done * 90 / resource.size) / resources.len() as u64)
                        as u8,
                )
                .map_err(|_| anyhow::anyhow!("operation state unavailable"))
            };
            match fetch(
                &format!("{origin}{relative}"),
                temp.as_file_mut(),
                resource,
                &progress,
            ) {
                Ok(()) => {
                    transferred = true;
                    break;
                }
                Err(error) if error.is::<SourceFailure>() => {
                    check()?;
                    eprintln!("command=model_download status=source_failed origin={origin} error={error:#}");
                }
                Err(error) => return Err(error),
            }
        }
        anyhow::ensure!(transferred, SourcesExhausted);
        temp.as_file().sync_all()?;
        staged.push((resource, temp));
    }
    // Wait without holding Core or vault locks; status and cancellation remain live.
    let mut staged = Some(staged);
    loop {
        checkpoint(job)?;
        if let Some(result) = crate::rpc_serve::with_owner_store_fence(|| {
            let staged = staged.take().context("model staging already consumed")?;
            profiles::exclusive(|| {
                checkpoint(job)?;
                for resource in resources {
                    let path = staged
                        .iter()
                        .find(|(r, _)| r.path == resource.path)
                        .map(|(_, temp)| temp.path())
                        .unwrap_or(&resource.path);
                    let candidate = Resource {
                        path: path.to_owned(),
                        ..resource.clone()
                    };
                    anyhow::ensure!(
                        valid(&candidate, &check)?,
                        "model artifact changed before publication"
                    );
                }
                model_switch::with_authority(job.scope(), |_| {
                    job.model_commit(true, || {
                        model_progress::commit_final(|| {
                            for (resource, temp) in staged {
                                check_path(&resource.path)?;
                                check_path(temp.path())?;
                                temp.persist_noclobber(&resource.path)
                                    .map_err(|e| e.error)?;
                                #[cfg(unix)]
                                std::fs::File::open(
                                    resource.path.parent().context("model parent missing")?,
                                )?
                                .sync_all()?;
                            }
                            Ok(())
                        })
                    })
                })
            })
        }) {
            return result;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug)]
struct SourceFailure;
impl std::fmt::Display for SourceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("model source transfer or integrity failed")
    }
}
impl std::error::Error for SourceFailure {}
#[derive(Debug)]
struct SourcesExhausted;
impl std::fmt::Display for SourcesExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("approved model sources exhausted")
    }
}
impl std::error::Error for SourcesExhausted {}

fn e2e_loopback_allowed(url: &url::Url) -> bool {
    #[cfg(not(feature = "e2e-loopback-mirror"))]
    {
        let _ = url;
        false
    }
    #[cfg(feature = "e2e-loopback-mirror")]
    {
        url.username().is_empty()
            && url.password().is_none()
            && super::super::host_config::e2e_loopback_origin()
                .ok()
                .flatten()
                .as_deref()
                == Some(url.origin().ascii_serialization().as_str())
    }
}

fn response(agent: &ureq::Agent, url: &str, check: &impl Fn() -> Result<()>) -> Result<ureq::Response> {
    let mut next = url::Url::parse(url)?;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        check()?;
        let host = next.host_str().context("model source host missing")?;
        anyhow::ensure!(
            e2e_loopback_allowed(&next)
                || (next.scheme() == "https"
                    && next.username().is_empty()
                    && next.password().is_none()
                    && next.port().is_none()
                    && matches!(
                        host,
                        "hf-mirror.com"
                            | "hf-mirror.net"
                            | "huggingface.co"
                            | "cdn-lfs.huggingface.co"
                            | "cdn-lfs.hf.co"
                            | "cdn-lfs-us-1.hf.co"
                            | "cas-bridge.xethub.hf.co"
                    )),
            SourceFailure
        );
        anyhow::ensure!(seen.insert(next.to_string()), SourceFailure);
        let reply = agent
            .get(next.as_str())
            .set("Accept-Encoding", "identity")
            .call()
            .map_err(|error| match error {
                ureq::Error::Status(status, _) => anyhow::Error::new(SourceFailure)
                    .context(format!("model source returned HTTP {status}")),
                ureq::Error::Transport(error) => anyhow::Error::new(SourceFailure).context(
                    format!("model source transport failed ({:?})", error.kind()),
                ),
            })?;
        if matches!(reply.status(), 301 | 302 | 303 | 307 | 308) {
            next = next
                .join(reply.header("Location").context(SourceFailure)?)
                .map_err(|_| SourceFailure)?;
        } else {
            anyhow::ensure!(reply.status() == 200, SourceFailure);
            return Ok(reply);
        }
    }
}

fn transfer(
    agent: &ureq::Agent,
    url: &str,
    output: &mut std::fs::File,
    resource: &Resource,
    progress: &dyn Fn(u64) -> Result<()>,
) -> Result<()> {
    progress(0)?;
    let reply = response(agent, url, &|| progress(0))?;
    if let Some(size) = reply.header("Content-Length") {
        anyhow::ensure!(
            size.parse::<u64>().ok() == Some(resource.size),
            SourceFailure
        );
    }
    copy_verified(reply.into_reader(), output, resource, progress)
}

fn copy_verified(
    mut input: impl Read,
    output: &mut impl Write,
    resource: &Resource,
    progress: &dyn Fn(u64) -> Result<()>,
) -> Result<()> {
    let mut buffer = [0; 64 * 1024];
    let mut done = 0;
    let mut hash = Sha256::new();
    loop {
        progress(done)?;
        let count = input.read(&mut buffer).map_err(|error| {
            anyhow::Error::new(SourceFailure)
                .context(format!("model source read failed ({:?})", error.kind()))
        })?;
        if count == 0 {
            break;
        }
        anyhow::ensure!(count as u64 <= resource.size - done, SourceFailure);
        output.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        done += count as u64;
    }
    anyhow::ensure!(
        done == resource.size
            && format!("{:x}", hash.finalize()).eq_ignore_ascii_case(&resource.sha256),
        SourceFailure
    );
    progress(done)?;
    Ok(())
}
