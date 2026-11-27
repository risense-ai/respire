fn uninstall_m3_online(
    owner: &crate::net_rpc::Health,
    root: &Path,
    profile: &Path,
) -> Result<Option<ResultEnvelope>> {
    use crate::net_rpc::{
        openapi_client, openapi_health, openapi_optional, read_token, rpc_base_url,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = openapi_client(&rpc_base_url(), read_token()?)?;
    let advertised = runtime.block_on(openapi_health(&client, Some(&owner.instance_id)))?;
    if !advertised.is_some_and(|health| {
        health
            .capabilities
            .iter()
            .any(|flag| flag == "model.active_assets")
    }) {
        return Ok(None);
    }
    let directory = local_m3_directory()?;
    let removed = remove_m3(root, profile, &directory, |target| {
        let assets = openapi_optional(
            runtime.block_on(client.get_active_model_assets()),
            "active model assets",
        )?
        .context("Runtime advertised active model assets but the route is unavailable")?;
        check_inactive_m3(target, &assets)?;
        let current = crate::net_rpc::health()?;
        anyhow::ensure!(
            crate::net_rpc::request_scope(&current)? == crate::net_rpc::request_scope(owner)?
                && current.pid == owner.pid
                && current.process_start_id == owner.process_start_id,
            "Runtime owner changed while checking active model assets"
        );
        Ok(())
    })?;
    let mut output = uninstall_result(&directory, removed);
    output.details = json!({"verified":true,"runtime_restarted":false});
    Ok(Some(output))
}

fn check_inactive_m3(
    target: Option<&Path>,
    assets: &respire_runtime_sdk::types::ActiveModelAssetsResponse,
) -> Result<()> {
    let model = assets.active_model_id.trim();
    anyhow::ensure!(!model.is_empty(), "Runtime active model ID is missing");
    anyhow::ensure!(
        !model.eq_ignore_ascii_case("m3") && !model.eq_ignore_ascii_case("bge-m3"),
        "cannot uninstall m3 while it is active"
    );
    for active in &assets.active_paths {
        let active = Path::new(active);
        anyhow::ensure!(
            active.is_absolute(),
            "Runtime returned a relative active asset path"
        );
        let active = active
            .canonicalize()
            .context("cannot verify Runtime active asset path")?;
        if let Some(target) = target {
            #[cfg(windows)]
            let (active, target) = (
                PathBuf::from(
                    active
                        .to_str()
                        .context("active asset path must be Unicode")?
                        .to_lowercase(),
                ),
                PathBuf::from(
                    target
                        .to_str()
                        .context("uninstall path must be Unicode")?
                        .to_lowercase(),
                ),
            );
            anyhow::ensure!(
                !active.starts_with(&target) && !target.starts_with(&active),
                "an active Runtime asset overlaps the uninstall directory"
            );
        }
    }
    Ok(())
}

fn uninstall_result(directory: &Path, removed: bool) -> ResultEnvelope {
    let status = if removed { Status::Ok } else { Status::Skip };
    ResultEnvelope::new(
        "model",
        status,
        json!({"dir":directory,"removed":removed,"model":"bge-m3"}),
        vec![Item::new(
            "model",
            status,
            if removed {
                directory.display().to_string()
            } else {
                "not installed".into()
            },
        )],
    )
}
