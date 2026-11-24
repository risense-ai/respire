pub(crate) fn exchange_control(
    binary: &Path,
    grant: &HostControlLaunchGrant,
    request: &RpcRequest,
    deadline: Instant,
    validate: impl FnOnce(RpcResponse<ResultEnvelope>, u32) -> Result<ResultEnvelope>,
) -> Result<ResultEnvelope> {
    let mut frames = Vec::new();
    for bytes in [serde_json::to_vec(&grant)?, serde_json::to_vec(&request)?] {
        anyhow::ensure!(
            !bytes.is_empty() && bytes.len() <= HOST_CONTROL_MAX_FRAME_BYTES,
            "host control request frame exceeds the protocol limit"
        );
        frames.extend_from_slice(&u32::try_from(bytes.len())?.to_be_bytes());
        frames.extend_from_slice(&bytes);
    }
    anyhow::ensure!(Instant::now() < deadline, "host control deadline exceeded");
    let mut command = Command::new(&binary);
    command
        .arg("--host-control-internal")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("RSRS_RUNTIME_BOOT_TOKEN");
    let mut child = respire_spawn::spawn_control_helper(&mut command)
        .context("cannot start owned host control Runtime")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let (input_sender, input_receiver) = std::sync::mpsc::channel();
    let mut worker = None;
    let outcome = (|| -> Result<ResultEnvelope> {
        let mut input = child
            .stdin
            .take()
            .context("missing host control input pipe")?;
        let mut output = child
            .stdout
            .take()
            .context("missing host control output pipe")?;
        worker = Some(
            std::thread::Builder::new()
                .name("host-control-io".into())
                .spawn(move || {
                    let response = (|| -> Result<RpcResponse<ResultEnvelope>> {
                        let written = input.write_all(&frames).and_then(|()| input.flush());
                        written.context("host control request write failed")?;
                        let response = respire_client_host::host_credentials::read_control_response(
                            &mut input, &mut output,
                        )?;
                        input_sender.send(input).map_err(|_| {
                            anyhow::anyhow!("host control input owner was released")
                        })?;
                        let mut extra = [0u8; 1];
                        anyhow::ensure!(
                            output.read(&mut extra)? == 0,
                            "extra host control response bytes"
                        );
                        Ok(response)
                    })();
                    let _ = sender.send(response);
                })?,
        );
        let response = receiver
            .recv()
            .context("host control response channel failure")??;
        let expected_exit = response.exit;
        let envelope = validate(response, child.id())?;
        let status = child.wait().context("cannot wait for host control Runtime exit")?;
        anyhow::ensure!(
            status.code() == Some(expected_exit),
            "host control Runtime exit does not match its response"
        );
        Ok(envelope)
    })();
    let cleanup = if outcome.is_err() {
        stop_control_child(&mut child)
    } else {
        Ok(())
    };
    // Keep stdin until exit confirmation; on failure release it to revoke the channel.
    drop(input_receiver);
    drop(receiver);
    cleanup?;
    if let Some(worker) = worker {
        let until = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() {
            anyhow::ensure!(
                Instant::now() < until,
                "cleanup_failed owned_pid={}: host control I/O worker did not finish",
                child.id()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.join().map_err(|_| {
            anyhow::anyhow!(
                "cleanup_failed owned_pid={}: host control I/O worker panicked",
                child.id()
            )
        })?;
    }
    outcome
}

fn stop_control_child(child: &mut Child) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(2);
    let kill_error = child.kill().err();
    loop {
        if control_exit_confirmed(
            child.id(),
            child.try_wait().map(|status| status.is_some()),
            Instant::now() >= until,
            kill_error.as_ref(),
        )? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn control_exit_confirmed(
    pid: u32,
    observation: std::io::Result<bool>,
    expired: bool,
    kill_error: Option<&std::io::Error>,
) -> Result<bool> {
    let reason = match observation {
        // try_wait(Some) has reaped the owned child, even if kill lost an exit race.
        Ok(true) => return Ok(true),
        Ok(false) if !expired => return Ok(false),
        Ok(false) => "exit confirmation deadline exceeded".to_owned(),
        Err(error) => format!("exit confirmation failed: {error}"),
    };
    match kill_error {
        Some(error) => {
            anyhow::bail!("cleanup_failed owned_pid={pid}: {reason}; kill failed: {error}")
        }
        None => anyhow::bail!("cleanup_failed owned_pid={pid}: {reason}"),
    }
}

fn validate_control_response(
    response: RpcResponse<ResultEnvelope>,
    request: &RpcRequest,
    profile: &Path,
    pid: u32,
) -> Result<ResultEnvelope> {
    anyhow::ensure!(
        response.v == request.v
            && response.id == request.id
            && response.bin == "rsrs-runtime"
            && response.pid == Some(pid)
            && response.scope.is_none()
            && response.operation_id.is_none()
            && response.idempotency_key.is_none()
            && response.web_url.is_none(),
        "host control response identity mismatch"
    );
    if let Some(data_dir) = &response.data_dir {
        anyhow::ensure!(
            Path::new(data_dir).canonicalize()? == profile,
            "host control data directory mismatch"
        );
    }
    let envelope = response
        .envelope
        .context("missing host control result envelope")?;
    let mut ready = envelope
        .items
        .iter()
        .filter(|item| item.name == "control_ready");
    anyhow::ensure!(
        ready
            .next()
            .is_some_and(|item| matches!(item.status, Status::Ok) && item.value == "true")
            && ready.next().is_none(),
        "host control authorization was not confirmed"
    );
    // Only the authenticated handler's post-authorization DB failure carries diagnostics.
    if !response.ok
        && response.exit == 2
        && response.code.as_deref() == Some("revision_unavailable")
        && envelope.command == "memory-revision"
        && matches!(envelope.status, Status::Fail)
    {
        if let Some(diagnostic) = response
            .error
            .as_deref()
            .filter(|message| !message.is_empty() && message.len() <= HOST_CONTROL_MAX_FRAME_BYTES)
        {
            anyhow::bail!("Runtime memory revision unavailable: {diagnostic}");
        }
    }
    anyhow::ensure!(
        response.ok && response.exit == 0 && response.error.is_none() && response.code.is_none(),
        "host control memory revision failed"
    );
    anyhow::ensure!(
        envelope.command == "memory-revision"
            && matches!(envelope.status, Status::Ok)
            && envelope.errors.is_empty(),
        "invalid host control result envelope"
    );
    let actual_profile = envelope.summary["profile"]
        .as_str()
        .context("missing host control profile")?;
    anyhow::ensure!(
        Path::new(actual_profile).canonicalize()? == profile
            && envelope.summary["revision"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
        "host control revision profile or value is invalid"
    );
    Ok(envelope)
}

fn prepare_launch_roots() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    // Resolve strict configuration and reject parent traversal before creating roots.
    let (root, profile) = respire_client_host::config::control_roots()?;
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&profile)?;
    Ok((
        root.canonicalize()?,
        profile.canonicalize()?,
    ))
}

fn launch_port(arguments: &[OsString]) -> Result<u16> {
    let mut found = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--port" {
            anyhow::ensure!(found.is_none(), "duplicate launch port");
            let port = arguments
                .next()
                .and_then(|value| value.to_str())
                .context("launch port is missing")?
                .parse::<u16>()?;
            anyhow::ensure!(port != 0, "launch port cannot be zero");
            found = Some(port);
        }
    }
    Ok(found.unwrap_or_else(crate::net_rpc::rpc_port))
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn verify_registered_supervisor(current: &Path, registered: &Path) -> Result<()> {
    let current = current.canonicalize()?;
    let registered = registered.canonicalize()?;
    anyhow::ensure!(
        current == registered || hash_file(&current)? == hash_file(&registered)?,
        "managed supervisor differs from the registered CLI"
    );
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}
