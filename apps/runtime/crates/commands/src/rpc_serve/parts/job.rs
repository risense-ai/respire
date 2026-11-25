pub struct Job {
    pub scope: RuntimeRequestScope,
    pub args: Vec<String>,
    pub stop: bool,
    pub reply: Reply,
    pub sync_context: Option<(usize, i64)>,
    pub deadline: Arc<crate::admission::QueueDeadline>,
}

type CommandResult = std::result::Result<crate::output_state::Captured, String>;
type ReplyAction = Box<dyn FnOnce(CommandResult) + Send>;

/// A queued transport owns its reply without keeping a connection thread alive.
#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<ReplyAction>>>);
impl Reply {
    pub fn new(action: impl FnOnce(CommandResult) + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some(Box::new(action)))))
    }
    pub fn send(&self, result: CommandResult) -> std::result::Result<(), String> {
        let action = self
            .0
            .lock()
            .map_err(|_| "reply lock poisoned".to_owned())?
            .take();
        let action = action.ok_or_else(|| "reply already delivered".to_owned())?;
        action(result);
        Ok(())
    }
}

struct QueueWatch {
    deadline: std::sync::Weak<crate::admission::QueueDeadline>,
    reply: std::sync::Weak<Mutex<Option<ReplyAction>>>,
}

fn watch_queue(deadline: &Arc<crate::admission::QueueDeadline>, reply: &Reply) -> Result<()> {
    static WATCH: OnceLock<Mutex<Vec<QueueWatch>>> = OnceLock::new();
    let entries = WATCH.get_or_init(|| Mutex::new(Vec::new()));
    static STARTED: OnceLock<()> = OnceLock::new();
    let network = crate::network::runtime()?;
    STARTED.get_or_init(|| {
        network.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            loop {
                tick.tick().await;
                let mut expired = Vec::new();
                {
                    let mut entries = entries.lock().unwrap_or_else(|error| error.into_inner());
                    entries.retain(|entry| {
                        let Some(deadline) = entry.deadline.upgrade() else {
                            return false;
                        };
                        if deadline.expired() {
                            if let Some(reply) = entry.reply.upgrade() {
                                expired.push(Reply(reply));
                            }
                            false
                        } else {
                            !deadline.running()
                        }
                    });
                }
                // Reply actions only hand off in-memory results or spawn async I/O.
                for reply in expired {
                    let _ = reply.send(Err(QUEUE_EXPIRED.into()));
                }
            }
        });
    });
    entries
        .lock()
        .map_err(|_| anyhow::anyhow!("queue watch lock poisoned"))?
        .push(QueueWatch {
            deadline: Arc::downgrade(deadline),
            reply: Arc::downgrade(&reply.0),
        });
    Ok(())
}

pub fn serve(flags: RuntimeFlags) -> Result<()> {
    validate_process_identity()?;
    crate::runtime_policy::require_host("runtime startup")?;
    // Ignore SIGHUP off this thread. signal_hook's handler install must not sit
    // in front of bind: under llvm-cov that call can stall the thread that has
    // to become the listener, and the coverage run then never sees the runtime.
    #[cfg(unix)]
    {
        let _ = std::thread::Builder::new()
            .name("respire-sighup".into())
            .spawn(|| {
                let _ = signal_hook::flag::register(
                    signal_hook::consts::SIGHUP,
                    Arc::new(AtomicBool::new(false)),
                );
            });
    }
    // Children only bind. The host caller owns stop/copy/upgrade decisions.
    if probe_runtime()?.is_some() {
        return Ok(());
    }
    // Keep installation ownership for the listener's entire lifetime. A port
    // override must not start a second host for the same installation or replace
    // its endpoint record, which the former local-socket name also prevented.
    let _owner =
        match respire_app::lock::LibraryLock::acquire(&runtime_dir(), Duration::from_secs(15)) {
            Ok(owner) => owner,
            Err(error) => {
                // Another host may have completed startup while this caller waited.
                if probe_runtime()?.is_some() {
                    return Ok(());
                }
                return Err(error).context("another runtime owns this installation");
            }
        };
    if probe_runtime()?.is_some() {
        return Ok(());
    }
    respire_app::service::recover_host_config()?;
    respire_app::service::validate_client_config()?;
    // Acquire the existing library ownership before publishing readiness. This
    // also guards an older runtime using another port, and direct-mode clients.
    let library = respire_app::lock::LibraryLock::acquire(
        &respire_app::service::data_dir(),
        Duration::from_secs(120),
    )
    .context("runtime failed to lock the library")?;
    crate::host_hooks::verify_managed_boot()?;
    start_host_plugin_reader()?;
    // Probe and bind only the existing loopback HTTP/TCP transport; no legacy
    // pipe/socket fallback is necessary.
    let bound = crate::runtime_http::bind_runtime(flags.port, &flags.host)?;
    {
        let mut url = WEB_URL.lock().unwrap_or_else(|err| err.into_inner());
        *url = bound.url.clone();
    }
    let publication = crate::rpc_worker::publish_endpoint(&bound.url)?;
    let owner = Arc::new(Mutex::new(_owner));
    let (tx, rx) = mpsc::channel();
    let _ = JOB_TX.set(Mutex::new(tx.clone()));
    // Sandboxed clients cannot restart an idle-exited host runtime.
    let idle = None;
    let limit = worker_limit();
    WORKERS.store(limit, Ordering::Release);
    RUNNING.store(0, Ordering::Release);
    let startup = StartupEndpoint { owner: owner.clone(), publication: publication.clone() };
    if let Err(error) = std::thread::Builder::new()
        .name("respire-dispatch".into())
        // Services do not inherit a terminal's RUST_MIN_STACK. Context-query
        // parsing needs the same explicit stack as the command workers.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || dispatch_loop_inner(rx, limit, idle, library, Some(startup)))
    {
        StartupEndpoint { owner: owner.clone(), publication }.revoke();
        return Err(error).context("failed to start the runtime dispatcher");
    }
    // Keep the host entry alive on its actual listener. Drain/worker shutdown
    // retains its existing ownership and process-exit path in dispatch_loop.
    crate::runtime_http::serve_loop(bound.server)
}

fn locked_response(request: &RpcRequest) -> Option<RpcResponse> {
    if !STORE_READY.load(Ordering::Acquire)
        && !core_config_method(&request.method)
        && !(request.method == "cli.exec" && command_name(&request.args) == Some("status"))
        && !matches!(request.method.as_str(), "runtime.status" | "runtime.stop" | "runtime.result"
            | "cli.progress" | "model.progress")
    {
        return Some(error_response(request, "runtime_locked", "supply private key material from the CLI or Client2 before accessing the library"));
    }
    None
}

/// Protocol admission is shared by blocking callers and the HTTP callback
/// path. Scope is still checked under the admission and execution fences.
fn validate_rpc_entry(request: &RpcRequest) -> Option<RpcResponse> {
    if request.v != PROTOCOL_V {
        return Some(error_response(request, "protocol_version_mismatch", "protocol version mismatch"));
    }
    if !valid_identifier(&request.id) {
        return Some(error_response(request, "invalid_request_id",
            "request ID must contain 1 to 256 characters and no whitespace"));
    }
    if let Err((code, error)) = validate_request_scope(request) {
        return Some(error_response(request, &code, &error));
    }
    if let Some(response) = locked_response(request) {
        return Some(response);
    }
    if matches!(request.method.as_str(), "model.prepare" | "index.prepare")
        && crate::host_hooks::background_paused()
    {
        return Some(error_response(request, "runtime_maintenance",
            "runtime maintenance pauses model/index preparation"));
    }
    None
}

pub fn dispatch(request: RpcRequest, tx: &Sender<Job>) -> RpcResponse {
    if let Some(response) = validate_rpc_entry(&request) {
        return response;
    }
    dispatch_validated(request, tx)
}

