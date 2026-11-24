fn zh() -> bool {
    i18n::lang() == Lang::Zh
}

fn t(zh_text: &str, en_text: &str) -> String {
    if zh() {
        zh_text.to_owned()
    } else {
        en_text.to_owned()
    }
}

fn rpc(args: &[&str]) -> Result<Value, String> {
    let owned: Vec<String> = args.iter().copied().map(str::to_owned).collect();
    crate::rpc::query_existing_json(owned).map_err(|err| format!("{err:#}"))
}

fn retrieval_action(args: &[&str]) -> Result<Value, String> {
    // Configuration reads and writes are host-owned, including fusion read-back.
    if matches!(args, ["agent-config"] | ["agent-config", "--set", _]) {
        return host_command(args);
    }
    checked_result(rpc(args)?)
}

fn checked_result(result: Value) -> Result<Value, String> {
    if matches!(result["status"].as_str(), Some("ok" | "skip")) {
        return Ok(result);
    }
    let errors = result["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    if errors.contains("model operation cancelled") {
        return Err(t(
            "\u{5df2}\u{53d6}\u{6d88}\u{ff1b}\u{5f53}\u{524d}\u{7d22}\u{5f15}\u{4fdd}\u{6301}\u{4e0d}\u{53d8}",
            "Cancelled; current index unchanged",
        ));
    }
    Err(if errors.is_empty() {
        format!("Command failed: {}", result["status"])
    } else {
        errors
    })
}

fn switch_account(name: &str) -> Result<String, String> {
    let switched = host_command(&["account", "use", name])?;
    let expected_dir = switched["summary"]["dir"]
        .as_str()
        .ok_or_else(|| "Profile switch did not return its directory".to_owned())?;
    let accounts = host_command(&["account", "list"])?;
    let actual = accounts["details"]["accounts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["current"] == true))
        .ok_or_else(|| "Host did not report its current account".to_owned())?;
    if actual["name"] != name || actual["dir"] != expected_dir {
        return Err("Host account does not match the requested profile".to_owned());
    }
    Ok(t(
        "\u{5df2}\u{5207}\u{6362}\u{8d26}\u{6237}",
        "Account switched",
    ))
}

fn host_command(args: &[&str]) -> Result<Value, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command.arg("--json").args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().map_err(|error| error.to_string())?;
    // Diagnostic failures are display rows, not a reason to hide the report.
    if args == ["doctor"] {
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if value["command"] == "doctor" {
                return Ok(value);
            }
        }
    }
    if !output.status.success() {
        if let Ok(value) = serde_json::from_slice(&output.stdout) {
            if let Err(error) = checked_result(value) {
                return Err(error);
            }
        }
        return Err(format!(
            "Host action failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let envelope = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    checked_result(envelope)
}

fn refresh_loop(stop: Arc<AtomicBool>, shared: Arc<Mutex<Live>>) {
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let mut next = Live::empty();
        if let Some((pid, url, version)) = crate::rpc::runtime_brief() {
            next.pid = pid;
            next.url = url;
            next.runtime_version = version;
        }
        match rpc(&["status"]) {
            Ok(envelope) if envelope["status"] == "ok" => {
                let summary = &envelope["summary"];
                next.index = summary["retrieval_index"].clone();
                next.model_progress = summary["model_operation"].clone();
                next.connected = true;
                next.user = summary["session"]["user"].as_str().unwrap_or("").to_owned();
                next.addr = summary["server_addr"].as_str().unwrap_or("").to_owned();
                next.autosync = summary["autosync"].as_bool().unwrap_or(true);
                next.workspace = summary["workspace"].as_str().unwrap_or("normal").to_owned();
                next.local_alive = summary["local_alive"].as_u64().unwrap_or(0);
                next.local_total = summary["local_total"].as_u64().unwrap_or(0);
                let live = &summary["sync_live"];
                next.phase = live["phase"].as_str().unwrap_or("idle").to_owned();
                next.pulled = live["pulled"].as_u64().unwrap_or(0);
                next.pushed = live["pushed"].as_u64().unwrap_or(0);
                next.remote_alive = live["remote_alive"].as_u64().unwrap_or(0);
                next.conflicts = live["conflicts"].as_i64().unwrap_or(0);
                next.pending = live["pending"].as_i64().unwrap_or(0);
                if next.phase == "err" {
                    next.error = live["error"].as_str().unwrap_or("").to_owned();
                }
            }
            Ok(envelope) => next.error = envelope["errors"].to_string(),
            Err(error) => {
                next.connected = false;
                next.error = error;
            }
        }
        if !next.connected {
            next.runtime_version.clear();
        }
        if let Ok(mut guard) = shared.lock() {
            next.inference = guard.inference.clone();
            next.notice = guard.notice.clone();
            next.ver_line = guard.ver_line.clone();
            next.update_spec = guard.update_spec.clone();
            next.inject = guard.inject.iter().map(InjectRow::clone_row).collect();
            next.accounts = guard.accounts.iter().map(AccountRow::clone_row).collect();
            next.doctor = guard.doctor.clone();
            next.engine = guard.engine.clone();
            next.model_settings = guard.model_settings.clone();
            next.fresh = guard.fresh;
            next.stale = guard.stale;
            next.missing = guard.missing;
            *guard = next;
        }
        std::thread::sleep(FAST.saturating_sub(started.elapsed()));
    }
}

fn refresh_inference_loop(stop: Arc<AtomicBool>, shared: Arc<Mutex<Live>>) {
    while !stop.load(Ordering::Relaxed) {
        let inference = match crate::net_rpc::health() {
            Ok(health) => health.inference,
            Err(error) => serde_json::json!({"phase":"unavailable","error":format!("{error:#}")}),
        };
        if let Ok(mut state) = shared.lock() {
            state.inference = inference;
        }
        std::thread::sleep(FAST);
    }
}

fn refresh_details_loop(stop: Arc<AtomicBool>, shared: Arc<Mutex<Live>>) {
    let mut slow_at = Instant::now() - SLOW;
    while !stop.load(Ordering::Relaxed) {
        let ready = shared.lock().map(|live| live.connected).unwrap_or(false);
        if !ready || slow_at.elapsed() < SLOW {
            std::thread::sleep(FAST);
            continue;
        }
        slow_at = Instant::now();
        let mut next = match shared.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        if let Ok(envelope) = host_command(&["account", "list"]) {
            if let Some(rows) = envelope["details"]["accounts"].as_array() {
                next.accounts.clear();
                for row in rows {
                    next.accounts.push(AccountRow {
                        name: row["name"].as_str().unwrap_or("").to_owned(),
                        user: row["user"].as_str().unwrap_or("").to_owned(),
                        current: row["current"].as_bool().unwrap_or(false),
                    });
                }
            }
        }
        let revision = INJECT_REVISION.load(Ordering::Acquire);
        if revision % 2 == 0 {
            if let Ok(rows) = read_inject_rows() {
                if let Ok(mut guard) = shared.lock() {
                    if INJECT_REVISION.load(Ordering::Acquire) == revision {
                        set_inject_rows(&mut guard, rows);
                    }
                }
            }
        }

        if let Ok(envelope) = host_command(&["doctor"]) {
            if let Ok(envelope) = serde_json::from_value::<crate::output::ResultEnvelope>(envelope)
            {
                next.doctor = envelope
                    .items
                    .iter()
                    .map(|item| {
                        (
                            item.name.clone(),
                            item.status.as_str().to_owned(),
                            envelope.human_item_value(item),
                        )
                    })
                    .collect();
            }
        }
        let model_revision = MODEL_REVISION.load(Ordering::Acquire);
        let model_read = retrieval_action(&["model", "engine"]).and_then(checked_result);
        if let Ok(envelope) = &model_read {
            next.engine = envelope["summary"]["engine"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            next.model_settings = envelope["summary"].clone();
        }
        if let Ok(mut guard) = shared.lock() {
            if model_read.is_ok()
                && model_revision % 2 == 0
                && MODEL_REVISION.load(Ordering::Acquire) == model_revision
            {
                guard.engine = next.engine;
                guard.model_settings = next.model_settings;
            }
            guard.accounts = next.accounts;
            guard.doctor = next.doctor;
        }
    }
}
