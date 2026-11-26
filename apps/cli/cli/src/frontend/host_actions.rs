use super::*;

pub(super) fn run_inject(
    targets_only: bool,
    id: Option<&str>,
    remove: bool,
    all: bool,
    preview: bool,
    expected: Option<&str>,
) -> Result<()> {
    let _ = all; // --all: explicit full inject (skip TUI; no-args in a real TTY opens TUI, otherwise same)
    if preview || expected.is_some() {
        if id != Some("codex") {
            anyhow::bail!("preview and revision check currently support --id codex only");
        }
        if preview {
            emit_details(
                "inject",
                OutputStatus::Ok,
                serde_json::json!({"preview":true,"target":"codex"}),
                serde_json::to_value(respire::inject::preview_codex(remove)?)?,
            )?;
        } else if let Some(revision) = expected {
            let changed = respire::inject::apply_codex(remove, revision)?;
            emit_result(ResultEnvelope::new(
                "inject",
                OutputStatus::Ok,
                serde_json::json!({"changed":changed,"target":"codex"}),
                vec![OutputItem::new(
                    "codex",
                    OutputStatus::Ok,
                    if changed { "updated" } else { "unchanged" },
                )],
            ))?;
        }
        return Ok(());
    }
    if targets_only {
        let ts = respire::inject::targets()?;
        let items = ts
            .iter()
            .map(|t| {
                OutputItem::new(
                    t.id,
                    match t.state {
                        "fresh" => OutputStatus::Ok,
                        "stale" => OutputStatus::Warn,
                        _ => OutputStatus::Skip,
                    },
                    format!("{} {}", t.state, t.path),
                )
            })
            .collect();
        let mut result = ResultEnvelope::new(
            "inject",
            OutputStatus::Ok,
            serde_json::json!({"count":ts.len()}),
            items,
        );
        result.details = serde_json::to_value(&ts)?;
        result.actions.push("inject --all".into());
        emit_result(result)?;
        return Ok(());
    }
    match id {
        Some(id) => {
            let changed = if remove {
                respire::inject::remove_one(id)?
            } else {
                respire::inject::inject_one(id)?
            };
            emit_result(ResultEnvelope::new(
                "inject",
                OutputStatus::Ok,
                serde_json::json!({"changed":changed,"id":id}),
                vec![OutputItem::new(
                    id,
                    OutputStatus::Ok,
                    if changed { "updated" } else { "unchanged" },
                )],
            ))?;
            Ok(())
        }
        None => {
            let mut any = false;
            let mut failures = Vec::new();
            let mut rows: Vec<serde_json::Value> = Vec::new();
            let mut items: Vec<OutputItem> = Vec::new();
            for t in respire::inject::targets()? {
                if !t.likely_installed && t.id != "generic" {
                    continue;
                }
                let changed = match respire::inject::inject_one(t.id) {
                    Ok(changed) => changed,
                    Err(error) => {
                        let message = format!("{}: {error:#}", t.id);
                        rows.push(
                            serde_json::json!({ "id": t.id, "error": message, "path": t.path }),
                        );
                        items.push(OutputItem::new(t.id, OutputStatus::Fail, "failed"));
                        failures.push(message);
                        continue;
                    }
                };
                any |= changed;
                rows.push(serde_json::json!({ "id": t.id, "changed": changed, "path": t.path }));
                items.push(OutputItem::new(
                    t.id,
                    OutputStatus::Ok,
                    if changed { "updated" } else { "unchanged" },
                ));
            }
            let status = if failures.is_empty() {
                OutputStatus::Ok
            } else {
                OutputStatus::Warn
            };
            let mut result = ResultEnvelope::new(
                "inject",
                status,
                serde_json::json!({"any":any,"count":rows.len()}),
                items,
            );
            result.details = serde_json::Value::Array(rows);
            if any {
                result
                    .actions
                    .push("restart the matching AI app or open a new session".into());
                result.actions.push("inject --targets".into());
            }
            result.errors.extend(failures);
            emit_result(result)?;
            Ok(())
        }
    }
}

pub(super) fn run_keygen(pass: Option<&str>, force: bool) -> Result<()> {
    emit_result(autostart::control_keygen(pass, force)?)
}

/// Version check (`update-check`): query the npm registry for a newer CLI.
/// 24h throttle (`--force` skips it); `--clear` drops the cache. Any network failure reports "not found", not an error.
pub(super) fn run_update_check(force: bool, clear: bool) -> Result<()> {
    use respire::update_check as uc;
    if clear {
        uc::clear_cache()?;
        if !force {
            return emit_result(ResultEnvelope::new(
                "update-check",
                OutputStatus::Ok,
                serde_json::json!({"cache":"cleared","checked":false}),
                vec![OutputItem::new("cache", OutputStatus::Ok, "cleared")]
                    .into_iter()
                    .map(|item| item.action("run update-check --force"))
                    .collect(),
            ));
        }
    }
    if !uc::enabled() {
        return emit_result(ResultEnvelope::new(
            "update-check",
            OutputStatus::Skip,
            serde_json::json!({"enabled":false}),
            vec![
                OutputItem::new("version check", OutputStatus::Skip, "disabled")
                    .action("unset RSRS_UPDATE_CHECK"),
            ],
        ));
    }
    let current = respire::VERSION;
    match uc::check(force) {
        Some(s) => {
            let status = if s.outdated {
                OutputStatus::Warn
            } else {
                OutputStatus::Ok
            };
            let mut result = ResultEnvelope::new(
                "update-check",
                status,
                serde_json::json!({"current":s.current.clone(),"latest":s.latest.clone(),"outdated":s.outdated,"ahead":uc::is_newer(&s.current,&s.latest),"cached":s.cached}),
                vec![OutputItem::new("version", status, s.comparison())],
            );
            if s.outdated {
                result.actions.push(s.install_command());
            }
            emit_result(result)
        }
        None => {
            let mut result = ResultEnvelope::new(
                "update-check",
                OutputStatus::Warn,
                serde_json::json!({"current":current,"latest":null,"outdated":null,"cached":false}),
                vec![OutputItem::new("version", OutputStatus::Warn, current)],
            );
            result
                .errors
                .push("registry unavailable or request timed out".into());
            emit_result(result)
        }
    }
}

/// Self-check (OpenViking doctor absorbed): model/store/lock/remote/inject/scope in one pass.
/// --remote also probes server /health. Per-item ok/fail; exit code stays 0 (diagnosis is not failure).
/// Multiple accounts on one machine: list profiles / use switches / remove deletes.
pub(super) fn run_account(action: &str, name: Option<&str>, yes: bool) -> Result<()> {
    match action {
        "list" => {
            let v = profile_discovery::account_list()?;
            let accounts = v["accounts"].as_array().cloned().unwrap_or_default();
            let items = accounts
                .iter()
                .map(|a| {
                    OutputItem::new(
                        a["name"].as_str().unwrap_or(""),
                        OutputStatus::Ok,
                        format!(
                            "{} {}",
                            a["dir"].as_str().unwrap_or(""),
                            a["user"].as_str().unwrap_or("")
                        ),
                    )
                })
                .collect();
            let mut result = ResultEnvelope::new(
                "account",
                OutputStatus::Ok,
                serde_json::json!({"count":accounts.len()}),
                items,
            );
            result.details = v;
            emit_result(result)
        }
        "use" => {
            let name = name.filter(|n| !n.trim().is_empty()).ok_or_else(|| {
                anyhow::anyhow!(
                    "use needs a profile name: rsrs account use <name> (main = primary)"
                )
            })?;
            emit_result(profile_control::account_use(name.trim())?)
        }
        "remove" => {
            let name = name.filter(|n| !n.trim().is_empty()).ok_or_else(|| {
                anyhow::anyhow!("remove needs a profile name: rsrs account remove <name> --yes")
            })?;
            if !yes {
                anyhow::bail!("deleting a profile wipes the local store and keys and is unrecoverable - add --yes to confirm");
            }
            emit_result(autostart::control_account_remove(name.trim())?)
        }
        // A profile name as the action = switch: rsrs account fslong ≡ account use fslong
        other => emit_result(profile_control::account_use(other)?),
    }
}

/// Space (virtual-account) command: an owner creates several virtual accounts, each a space; invite codes join people; kick drops membership.
pub(super) fn run_space(
    action: &str,
    name: Option<&str>,
    note: Option<&str>,
    readonly: bool,
    code: Option<&str>,
    session: Option<&str>,
    all: bool,
    yes: bool,
) -> Result<()> {
    use respire::space;
    match action {
        "list" => {
            let v = profile_discovery::space_list()?;
            let spaces = v["spaces"].as_array().cloned().unwrap_or_default();
            let items = spaces.iter().map(|s| {
                let name = s["name"].as_str().unwrap_or("");
                let dir = s["dir"].as_str().unwrap_or("");
                let user = s["user"].as_str().unwrap_or("");
                let members = s["members"].as_u64().unwrap_or(0);
                let current = if s["current"].as_bool() == Some(true) { " current" } else { "" };
                OutputItem::new(name, OutputStatus::Ok, format!("dir={dir} user={user} members={members}{current}"))
            }).collect();
            let mut result = ResultEnvelope::new("space", OutputStatus::Ok, serde_json::json!({"action":"list","count":spaces.len()}), items);
            result.details = v;
            emit_result(result)
        }
        "create" => {
            let n = name.filter(|n| !n.trim().is_empty())
                .ok_or_else(|| respire_client_host::input_error::InputError("create needs a space name: rsrs space create <name>".into()))?;
            emit_result(profile_control::space_create(n)?)
        }
        "use" => {
            let n = name.filter(|n| !n.trim().is_empty())
                .ok_or_else(|| respire_client_host::input_error::InputError("use needs a space name: rsrs space use <name>".into()))?;
            emit_result(profile_control::space_use(n)?)
        }
        "invite" => {
            emit_result(autostart::control_space_invite(note, readonly)?)
        }
        "join" => {
            let c = code.or(name).filter(|c| !c.trim().is_empty())
                .ok_or_else(|| respire_client_host::input_error::InputError("join needs an invite code: rsrs space join <code>".into()))?;
            emit_result(autostart::control_space_join(c)?)
        }
        "members" => {
            let v = space::space_members(name)?;
            let ms = v["members"].as_array().cloned().unwrap_or_default();
            let items = ms.iter().map(|m| OutputItem::new(m["session_id"].as_str().unwrap_or(""), OutputStatus::Ok, format!("device={} issued={}", m["device_name"].as_str().unwrap_or(""), m["issued_at"].as_str().unwrap_or("")))).collect();
            let status = if ms.is_empty() { OutputStatus::Skip } else { OutputStatus::Ok };
            let mut result = ResultEnvelope::new("space", status, serde_json::json!({"action":"members","space":v["space"],"count":ms.len()}), items);
            if ms.is_empty() { result.actions.push("space invite".into()); }
            result.details = v;
            emit_result(result)
        }
        "kick" => {
            if !session.map(|s| !s.trim().is_empty()).unwrap_or(false) && !all {
                return Err(respire_client_host::input_error::InputError("kick needs --session <id> or --all".into()).into());
            }
            emit_result(autostart::control_space_kick(session, all)?)
        }
        "remove" => {
            let n = name.filter(|n| !n.trim().is_empty())
                .ok_or_else(|| respire_client_host::input_error::InputError("remove needs a space name: rsrs space remove <name> --yes".into()))?;
            if !yes {
                return Err(respire_client_host::input_error::InputError("deleting a space profile wipes the local store and keys and is unrecoverable - add --yes to confirm".into()).into());
            }
            emit_result(autostart::control_space_remove(n)?)
        }
        other => anyhow::bail!(
            "unknown space action \"{other}\" - use: list | create <name> | use <name> | invite | join <code> | members | kick | remove"
        ),
    }
}

/// Local config: --data-dir/--addr/--autosync/--cure-auto set (all empty = read only).
pub(super) fn run_config(
    data_dir: Option<&str>,
    addr: Option<&str>,
    autosync: Option<bool>,
    cure_auto: Option<bool>,
    rpc_parallelism: Option<u32>,
) -> Result<()> {
    emit_result(autostart::run_config_control(
        data_dir,
        addr,
        autosync,
        cure_auto,
        rpc_parallelism,
    )?)
}

/// Under v3, Account Secret is no longer a decrypt key - export the v3 three-piece; old v1 material is listed as a side note.
pub(super) fn run_keys_export(out: Option<&str>) -> Result<()> {
    emit_result(autostart::control_keys_export(out)?)
}

/// Secrets are read by the authorized Runtime; rendering remains in the CLI.
pub(super) fn run_secret(reveal: bool) -> Result<()> {
    emit_result(autostart::control_secret(reveal)?)
}

pub(super) fn run_fivekeys(
    addr: &str,
    user: &str,
    pass: &str,
    secret: &str,
    kdf_salt: &str,
    wrapped_urk: &str,
    urk_nonce: &str,
    super_pass: &str,
) -> Result<()> {
    use respire_protocol::key_control::{
        HostKeyInput, HostKeyOperation, HostKeyOutcome, HostKeyResult,
    };
    let operation = HostKeyOperation::Fivekeys {
        addr: addr.trim().trim_end_matches('/').to_owned(),
        user: user.to_owned(),
        kdf_salt: kdf_salt.to_owned(),
        wrapped_urk: wrapped_urk.to_owned(),
        urk_nonce: urk_nonce.to_owned(),
        use_super: !super_pass.is_empty(),
    };
    let mut input = HostKeyInput {
        password: Some(pass.to_owned()),
        super_password: if super_pass.is_empty() {
            None
        } else {
            Some(super_pass.to_owned())
        },
        account_secret: Some(secret.to_owned()),
        totp: None,
        material: None,
    };
    let mut envelope = autostart::control_fivekeys(
        operation.clone(),
        HostKeyInput {
            password: input.password.clone(),
            super_password: input.super_password.clone(),
            account_secret: input.account_secret.clone(),
            totp: None,
            material: None,
        },
        None,
    )?;
    loop {
        if !matches!(envelope.status, OutputStatus::Pending) {
            return emit_result(envelope);
        }
        let receipt: HostKeyResult = serde_json::from_value(envelope.summary.clone())
            .map_err(|_| anyhow!("invalid fivekeys continuation receipt"))?;
        envelope.actions.push(format!(
            "runtime key-result --operation-id {}",
            receipt.operation_id
        ));
        if json_mode()
            || !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !std::io::IsTerminal::is_terminal(&std::io::stdout())
            || receipt.outcome != HostKeyOutcome::AwaitingInput
            || receipt.required_inputs.as_slice() != ["totp"]
            || input.totp.is_some()
        {
            return emit_result(envelope);
        }
        input.totp = Some(respire::prompt::ask_secret("TOTP: ").map_err(|error| {
            error.context(autostart::KeyRecoveryRequired {
                operation_id: receipt.operation_id.clone(),
                source_profile: receipt.source_profile.clone(),
            })
        })?);
        envelope = autostart::control_fivekeys(
            operation.clone(),
            HostKeyInput {
                password: input.password.clone(),
                super_password: input.super_password.clone(),
                account_secret: input.account_secret.clone(),
                totp: input.totp.clone(),
                material: None,
            },
            Some(&receipt),
        )?;
    }
}

pub(super) fn run_register(
    addr: Option<&str>,
    user: Option<&str>,
    pass: Option<&str>,
    super_pass: Option<&str>,
) -> Result<()> {
    let addr: &str = addr
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::client_config::DEFAULT_SERVER_ADDR);
    // Missing args: interactive prompt (TTY and not --json); args the prompt cannot fill still error.
    let interactive = respire::prompt::interactive();
    let user: String = match user.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(u) => u.to_owned(),
        None if interactive => {
            eprintln!("register server={addr}");
            respire::prompt::ask("username: ")?
        }
        None => {
            return Err(anyhow::anyhow!(
            "missing --user <username> (bare `rsrs register` in a TTY opens an interactive prompt)"
        ))
        }
    };
    let pass: String = match pass.filter(|s| !s.is_empty()) {
        Some(p) => p.to_owned(),
        None if interactive => {
            let p = respire::prompt::ask_secret("login password (for the server; can be reset): ")?;
            if p.len() < 8 {
                return Err(anyhow::anyhow!(
                    "login password must be at least 8 characters"
                ));
            }
            p
        }
        None => return Err(anyhow::anyhow!("missing --pass <login-password>")),
    };
    let super_pass: String = match super_pass.filter(|s| !s.is_empty()) {
        Some(s) => s.to_owned(),
        None => String::new(),
    };
    let response = autostart::control_register(
        addr,
        &user,
        &pass,
        (!super_pass.is_empty()).then_some(super_pass.as_str()),
    )?;
    let receipt: respire_protocol::host_control::HostAuthAccountResult =
        serde_json::from_value(response.summary.clone())?;
    anyhow::ensure!(
        receipt.outcome == respire_protocol::host_control::HostAuthAccountOutcome::Committed,
        "registration was not committed; retain and query the original account operation"
    );
    let super_key = receipt
        .super_password
        .filter(|value| !value.is_empty())
        .context("committed registration is missing the recovery code")?;
    let mut result = ResultEnvelope::new(
        "register",
        OutputStatus::Ok,
        serde_json::json!({"ok":true,"user":user,"addr":addr,"super":super_key}),
        vec![OutputItem::new("account", OutputStatus::Ok, user.clone())],
    );
    result.actions.push("secret --reveal".into());
    emit_result(result)
}
