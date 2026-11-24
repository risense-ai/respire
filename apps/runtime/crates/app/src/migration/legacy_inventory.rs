fn meaningful(path: &Path) -> Result<bool> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            bail!("migration root is a symbolic link; no external data was copied");
        }
    }
    if !path.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(path)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if !excluded(&name) && name != RECEIPT {
            return Ok(true);
        }
    }
    Ok(false)
}

fn excluded(name: &str) -> bool {
    matches!(
        name,
        "runtime" | "bin" | "bak" | ".rsrs-migrated.json" | "lock.db" | "lock.db-wal" | "lock.db-shm"
    ) || name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with(".pid")
        || name.starts_with("runtime.")
        || name.starts_with(".rsrs-metadata-")
        || name == "runtime.json"
        || name == "runtime-token"
}

fn identity(path: &Path) -> Result<String> {
    let canonical = path.canonicalize()?;
    let mut value = canonical.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.make_ascii_lowercase();
    }
    Ok(hex::encode(Sha256::digest(value.as_bytes())))
}

fn receipt_matches(destination: &Path, source_id: &str) -> Result<bool> {
    let path = destination.join(RECEIPT);
    if !path.is_file() {
        return Ok(false);
    }
    let receipt: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(receipt["source_identity"].as_str() == Some(source_id) && receipt["complete"] == true)
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
fn destination_for(base: PathBuf, id: &str, reserved: &mut BTreeSet<PathBuf>) -> Result<PathBuf> {
    if (!base.exists() || !meaningful(&base)? || receipt_matches(&base, id)?)
        && reserved.insert(base.clone())
    {
        return Ok(base);
    }
    let name = base
        .file_name()
        .ok_or_else(|| anyhow!("invalid migration destination"))?
        .to_string_lossy();
    let alternate = base.with_file_name(format!("{name}-{}", &id[..16]));
    if alternate.exists() && !receipt_matches(&alternate, id)? {
        bail!("legacy migration destination conflict; original libraries were preserved");
    }
    if !reserved.insert(alternate.clone()) {
        bail!("duplicate legacy migration destination");
    }
    Ok(alternate)
}

/// Also used by fixture-only tests; it does not replace HOME or access runtime services.
#[cfg(test)]
fn migrate_home(home: &Path) -> Result<()> {
    let target = home.join(".rsrs");
    if let Ok(metadata) = std::fs::symlink_metadata(target.join("accounts")) {
        if metadata.file_type().is_symlink() {
            bail!("new account root is a symbolic link; no external data was written");
        }
    }
    let sources = [
        ("respire", ["respire", "memocap", "1memory"]),
        ("onememory", ["1memory", "memocap", "respire"]),
    ];
    let mut roots = Vec::new();
    for (label, services) in sources {
        let path = home.join(format!(".{label}"));
        if meaningful(&path)? {
            roots.push((label, path, services));
        }
    }
    if roots.is_empty() {
        return Ok(());
    }
    let lock_root = home.join(".rsrs-migration-lock");
    private_directory(&lock_root)?;
    let _lock = crate::lock::LibraryLock::acquire(&lock_root, Duration::from_secs(30))?;
    let fresh = !meaningful(&target)?;
    let mut reserved = BTreeSet::new();
    let mut profiles = Vec::new();
    for (index, (label, source, services)) in roots.iter().enumerate() {
        let id = identity(source)?;
        let main = if fresh && index == 0 || receipt_matches(&target, &id)? {
            reserved.insert(target.clone());
            target.clone()
        } else {
            destination_for(
                target.join("accounts").join(format!("legacy-{label}")),
                &id,
                &mut reserved,
            )?
        };
        profiles.push(Profile {
            source: source.clone(),
            destination: main,
            identity: id,
            services: *services,
        });
        let accounts = source.join("accounts");
        if accounts.is_dir() {
            let mut entries = std::fs::read_dir(accounts)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if !meaningful(&entry.path())? {
                    continue;
                }
                let id = identity(&entry.path())?;
                let original = safe_name(&entry.file_name().to_string_lossy());
                let name = if index == 0 && (fresh || profiles[0].destination == target) {
                    original
                } else {
                    format!("legacy-{label}-{original}")
                };
                let destination =
                    destination_for(target.join("accounts").join(name), &id, &mut reserved)?;
                profiles.push(Profile {
                    source: entry.path(),
                    destination,
                    identity: id,
                    services: *services,
                });
            }
        }
    }
    let pending = profiles
        .iter()
        .filter_map(
            |profile| match receipt_matches(&profile.destination, &profile.identity) {
                Ok(true) => None,
                result => Some(result.map(|_| profile)),
            },
        )
        .collect::<Result<Vec<_>>>()?;
    if pending.is_empty() {
        return Ok(());
    }
    let staging_root = home.join(".rsrs-migration-staging");
    private_directory(&staging_root)?;
    let staging = staging_root.join(uuid::Uuid::new_v4().to_string());
    private_directory(&staging)?;
    let mut completed = Vec::new();
    // Provider settings belong to the user's selected global source, not to
    // every account. Existing new provider entries always retain precedence.
    crate::keystore::import_classification(&roots[0].2)?;
    for (index, profile) in pending.iter().enumerate() {
        let stage = staging.join(format!("profile-{index}"));
        snapshot_profile(profile, &stage, &profiles, false)?;
        completed.push((profile, stage));
    }
    // A new default home is published in one rename, including every account.
    if fresh {
        let (_, main_stage) = completed
            .iter()
            .find(|(profile, _)| profile.destination == target)
            .ok_or_else(|| anyhow!("missing main migration snapshot"))?;
        for (profile, stage) in &completed {
            if profile.destination == target {
                continue;
            }
            let relative = profile.destination.strip_prefix(&target)?;
            let destination = main_stage.join(relative);
            if let Some(parent) = destination.parent() {
                private_directory(parent)?;
            }
            std::fs::rename(stage, destination)?;
        }
        if target.exists() {
            // Only an empty directory may be replaced; never remove existing state.
            std::fs::remove_dir(&target)
                .context("new home is no longer empty; migration was not published")?;
        }
        std::fs::rename(main_stage, &target)?;
    } else {
        for (profile, stage) in completed {
            if profile.destination.exists() {
                std::fs::remove_dir(&profile.destination)
                    .context("migration destination changed; no state was overwritten")?;
            }
            if let Some(parent) = profile.destination.parent() {
                private_directory(&target)?;
                private_directory(parent)?;
            }
            std::fs::rename(stage, &profile.destination)?;
        }
    }
    eprintln!("legacy library copied safely to ~/.rsrs; original directories and credentials were retained");
    Ok(())
}

