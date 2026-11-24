use super::*;

type CallbackMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;

/// Hidden binary entry. No App, profile migration, store, or LibraryLock is opened.
pub fn run() -> Result<()> {
    crate::bge::m3_model_dir()?;
    let output = Arc::new(Mutex::new(crate::ffi::worker_stdout()?));
    let callbacks: CallbackMap = Arc::new(Mutex::new(HashMap::new()));
    let (ordinary_tx, ordinary_rx) = mpsc::channel::<Value>();
    let ordinary_rx = Arc::new(Mutex::new(ordinary_rx));
    let (background_tx, background_rx) = mpsc::channel::<Value>();
    let background_rx = Arc::new(Mutex::new(background_rx));
    let count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    for index in 0..=count {
        let receiver = if index == count {
            Arc::clone(&background_rx)
        } else {
            Arc::clone(&ordinary_rx)
        };
        let output = Arc::clone(&output);
        let callbacks = Arc::clone(&callbacks);
        std::thread::Builder::new()
            .name(format!("respire-core-{index}"))
            .stack_size(16 * 1024 * 1024)
            .spawn(move || loop {
                let frame = match receiver.lock() {
                    Ok(receiver) => receiver.recv(),
                    Err(_) => break,
                };
                let Ok(mut frame) = frame else {
                    break;
                };
                let id = frame["id"].as_u64().unwrap_or(u64::MAX);
                let result = (|| -> Result<Value> {
                    if let Some(root) = frame["index_root"].as_str() {
                        crate::business::set_index_root(std::path::Path::new(root))?;
                    }
                    let operation_value = frame["operation"].take();
                    let operation = operation_value
                        .as_str()
                        .context("Core worker operation missing")?;
                    #[cfg(feature = "native-fault-tests")]
                    if operation == "test_worker_hold" {
                        loop {
                            std::thread::park();
                        }
                    }
                    let payload = frame["payload"].take();
                    let mut callback = |request: &Value| -> Result<Value> {
                        let (sender, receiver) = mpsc::channel();
                        callbacks
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Core worker callbacks poisoned"))?
                            .insert(id, sender);
                        write_frame(
                            &mut *output
                                .lock()
                                .map_err(|_| anyhow::anyhow!("Core worker output poisoned"))?,
                            &ResponseFrame {
                                id,
                                body: ResponseBody::Callback { callback: request },
                            },
                        )?;
                        let mut response = receiver
                            .recv()
                            .context("host transport callback unavailable")?;
                        if let Some(error) = response["callback_error"].as_str() {
                            bail!("{error}");
                        }
                        response
                            .get_mut("callback_result")
                            .map(Value::take)
                            .context("host transport callback result missing")
                    };
                    #[cfg(feature = "native-fault-tests")]
                    if operation == "test_worker_callback" {
                        return callback(&json!({"synthetic_fixture":true}));
                    }
                    let selection = frame
                        .get("selection")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .map(serde_json::from_value::<crate::profiles::Selection>)
                        .transpose()?;
                    let execute = || -> Result<Value> {
                        if frame["transport"].as_bool() == Some(true) {
                            crate::business::execute_with_transport::<Value>(
                                operation,
                                payload,
                                &mut callback,
                            )
                        } else {
                            crate::business::execute::<Value>(operation, payload)
                        }
                    };
                    match selection {
                        Some(selection) => crate::profiles::scoped(selection, execute),
                        None => execute(),
                    }
                })();
                let body = match &result {
                    Ok(value) => ResponseBody::Result { result: value },
                    Err(error) => ResponseBody::Error {
                        error: format!("{error:#}"),
                    },
                };
                let response = ResponseFrame { id, body };
                if let Ok(mut output) = output.lock() {
                    if write_frame(&mut *output, &response).is_err() {
                        break;
                    }
                }
            })?;
    }
    let heartbeat_output = Arc::clone(&output);
    std::thread::Builder::new()
        .name("respire-core-heartbeat".into())
        // Core calls can initialize native EPs with deep call stacks.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let result = crate::Core::new();
            let Ok(mut core) = result else {
                return;
            };
            loop {
                let status = core
                    .call("engine_control", json!({"action":"inference_status"}))
                    .and_then(|mut status| {
                        let diagnostics = crate::host::provider_diagnostics()?;
                        if !diagnostics.is_empty() {
                            status["provider_registration_errors"] = json!(diagnostics);
                        }
                        Ok(status)
                    });
                match (status, heartbeat_output.lock()) {
                    (Ok(status), Ok(mut output)) => {
                        if write_frame(&mut *output, &HeartbeatFrame { heartbeat: &status })
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => break,
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })?;
    let mut input = std::io::stdin();
    loop {
        let frame = read_frame(&mut input)?;
        let id = frame["id"]
            .as_u64()
            .context("Core worker request ID missing")?;
        if frame.get("callback_result").is_some() || frame.get("callback_error").is_some() {
            if let Some(sender) = callbacks
                .lock()
                .map_err(|_| anyhow::anyhow!("Core worker callbacks poisoned"))?
                .remove(&id)
            {
                let _ = sender.send(frame);
            }
        } else if frame["operation"] == "prepare" {
            background_tx.send(frame)?;
        } else {
            ordinary_tx.send(frame)?;
        }
    }
}
