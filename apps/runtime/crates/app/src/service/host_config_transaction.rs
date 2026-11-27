use super::*;
use crate::lock::LibraryLock;
use serde_json::Value;
use std::{fs, io::Write, time::Duration};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    profile: PathBuf,
    committed: bool,
    client_before: Option<Vec<u8>>,
    client_after: Option<Vec<u8>>,
    agent_before: Option<Vec<u8>>,
    agent_after: Option<Vec<u8>>,
}

fn journal_path() -> PathBuf {
    client_config_path().with_file_name("host-config-transaction.json")
}

pub(super) fn ensure_no_host_config_transaction() -> Result<()> {
    ensure!(
        read_bytes(&journal_path())?.is_none(),
        "host configuration recovery is pending; restart Runtime before changing configuration"
    );
    Ok(())
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "configuration must be a regular, unlinked file: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(fs::read(path)?))
}

fn object(bytes: Option<&[u8]>) -> Result<Value> {
    let value: Value = bytes
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    ensure!(value.is_object(), "configuration must be a JSON object");
    Ok(value)
}

fn replace(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    let parent = path.parent().context("configuration parent missing")?;
    let Some(bytes) = bytes else {
        if read_bytes(path)?.is_some() {
            fs::remove_file(path)?;
        }
        return Ok(respire_spawn::durable::sync_directory(parent)?);
    };
    let temporary = parent.join(format!(".host-config-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        Ok(respire_spawn::durable::replace_file(&temporary, path)?)
    })();
    if let Err(error) = result {
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {}
            Err(cleanup) => {
                return Err(
                    error.context(format!("configuration temporary cleanup failed: {cleanup}"))
                )
            }
        }
        return Err(error);
    }
    Ok(())
}

impl Journal {
    fn files(&self) -> [(PathBuf, &Option<Vec<u8>>, &Option<Vec<u8>>); 2] {
        [
            (
                client_config_path(),
                &self.client_before,
                &self.client_after,
            ),
            (
                self.profile.join("agent.json"),
                &self.agent_before,
                &self.agent_after,
            ),
        ]
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && self.profile.is_absolute()
                && self.profile.canonicalize()? == self.profile,
            "invalid host configuration journal binding"
        );
        for bytes in [&self.client_before, &self.client_after] {
            let config = object(bytes.as_deref())?;
            ensure!(
                data_dir_from_config(Some(&config)).canonicalize()? == self.profile,
                "host configuration recovery profile changed"
            );
        }
        object(self.agent_before.as_deref())?;
        object(self.agent_after.as_deref())?;
        Ok(())
    }

    fn save(&self) -> Result<()> {
        replace(&journal_path(), Some(&serde_json::to_vec(self)?))
    }

    fn finish(&self) -> Result<()> {
        self.validate()?;
        // Check every file before restoring any file; never overwrite a later writer.
        for (path, before, after) in self.files() {
            let current = read_bytes(&path)?;
            ensure!(
                if self.committed {
                    current == *after
                } else {
                    current == *before || current == *after
                },
                "host configuration recovery conflict: {}; retain the journal",
                path.display()
            );
        }
        if !self.committed {
            for (path, before, _) in self.files().into_iter().rev() {
                if read_bytes(&path)? != *before {
                    replace(&path, before.as_deref())?;
                }
            }
        }
        replace(&journal_path(), None)
    }
}

/// Holds the existing cross-process config lock inside the caller's profile fence.
pub struct HostConfigTransaction<'a> {
    _ownership: &'a LibraryLock,
    _root: Option<LibraryLock>,
    _config: fs::File,
    journal: Journal,
    pub client: Value,
    pub agent: Value,
}

impl<'a> HostConfigTransaction<'a> {
    pub fn begin(ownership: &'a LibraryLock) -> Result<Self> {
        let profile = data_dir().canonicalize()?;
        ensure!(
            ownership.protects(&profile)?,
            "host configuration profile ownership missing"
        );
        ensure_runtime_profile()?;
        let root = main_data_dir().canonicalize()?;
        let root_lock = if root == profile {
            None
        } else {
            Some(LibraryLock::acquire(&root, Duration::ZERO)?)
        };
        let config_lock = client_config_lock_unchecked(false)?;
        let client_before = read_bytes(&client_config_path())?;
        let client = read_client_config_for_write()?;
        ensure!(
            client == object(client_before.as_deref())?,
            "client configuration changed during validation"
        );
        let agent_before = read_bytes(&profile.join("agent.json"))?;
        let agent = object(agent_before.as_deref())?;
        let journal = Journal {
            schema_version: 1,
            profile,
            committed: false,
            client_after: client_before.clone(),
            agent_after: agent_before.clone(),
            client_before,
            agent_before,
        };
        journal.validate()?;
        Ok(Self {
            _ownership: ownership,
            _root: root_lock,
            _config: config_lock,
            journal,
            client,
            agent,
        })
    }

    pub fn commit(mut self) -> Result<()> {
        if self.client != object(self.journal.client_before.as_deref())? {
            self.journal.client_after = Some(serde_json::to_vec_pretty(&self.client)?);
        }
        if self.agent != object(self.journal.agent_before.as_deref())? {
            self.journal.agent_after = Some(serde_json::to_vec_pretty(&self.agent)?);
        }
        self.journal.validate()?;
        if self.journal.client_before == self.journal.client_after
            && self.journal.agent_before == self.journal.agent_after
        {
            return Ok(());
        }
        for (path, before, _) in self.journal.files() {
            ensure!(
                read_bytes(&path)? == *before,
                "host configuration changed before publication"
            );
        }
        self.journal.save()?;
        let publication = (|| -> Result<()> {
            for (path, before, after) in self.journal.files() {
                if before != after {
                    replace(&path, after.as_deref())?;
                }
            }
            Ok(())
        })();
        if let Err(error) = publication {
            return match self.journal.finish() {
                Ok(()) => Err(error.context("host configuration rolled back")),
                Err(recovery) => Err(error.context(format!(
                    "host configuration recovery required: {recovery:#}"
                ))),
            };
        }
        // A failed commit-marker sync has an unknown outcome; recovery decides, never replay.
        self.journal.committed = true;
        self.journal.save()?;
        self.journal.finish()
    }
}

/// Called under installation ownership before the listener publishes readiness.
pub fn recover_host_config() -> Result<()> {
    let Some(original) = read_bytes(&journal_path())? else {
        return Ok(());
    };
    let journal: Journal = serde_json::from_slice(&original)?;
    journal.validate()?;
    let _profile = LibraryLock::acquire(&journal.profile, Duration::ZERO)?;
    let root = main_data_dir().canonicalize()?;
    let _root = if root == journal.profile {
        None
    } else {
        Some(LibraryLock::acquire(&root, Duration::ZERO)?)
    };
    let _config = client_config_lock_unchecked(false)?;
    ensure!(
        read_bytes(&journal_path())?.as_deref() == Some(original.as_slice()),
        "host configuration recovery journal changed"
    );
    journal.finish()
}

#[cfg(test)]
#[path = "host_config_transaction_tests.rs"]
mod tests;
