use std::io;

const PREFIX: &str = "job-";
pub(super) const SLOTS: [&str; 4] = [
    ".model-switch-slot-0",
    ".model-switch-slot-1",
    ".model-switch-slot-2",
    ".model-switch-slot-3",
];

pub(super) fn job_name() -> io::Result<String> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("model job entropy unavailable"))?;
    let mut name = PREFIX.to_owned();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in nonce {
        name.push(char::from(HEX[usize::from(byte >> 4)]));
        name.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(name)
}

pub(super) fn limit() -> io::Error {
    io::Error::other("too many retained private model jobs")
}

#[cfg(all(test, feature = "test-private-space-root"))]
mod tests {
    use super::*;
    use crate::owner_credential::ModelJobRoot;

    fn fixture() -> io::Result<(std::path::PathBuf, std::path::PathBuf)> {
        let parent = std::env::temp_dir().canonicalize()?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let parent = parent.join(format!(
            "respire-model-job-fixture-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&parent)?;
        #[cfg(windows)]
        let base =
            crate::owner_credential::LocalSpaceNamespace::make_fixture_installation(&parent)?;
        #[cfg(unix)]
        let base = {
            use std::os::unix::fs::DirBuilderExt;
            let base = parent.join("owner-only-installation");
            std::fs::DirBuilder::new().mode(0o700).create(&base)?;
            base
        };
        Ok((parent, base))
    }

    #[test]
    fn native_job_root_reserves_only_bounded_owner_private_children() -> io::Result<()> {
        let (parent, base) = fixture()?;
        let mut jobs = Vec::new();
        for _ in 0..SLOTS.len() {
            jobs.push(ModelJobRoot::create(&base)?);
        }
        assert!(ModelJobRoot::create(&base).is_err());
        let mut one = jobs.pop().ok_or_else(limit)?;
        let first = one.root()?.to_path_buf();
        assert!(first.starts_with(&base));
        {
            use std::io::{Read, Seek, Write};
            let mut spool = one.create_source_spool()?;
            spool.write_all(b"sealed fixture row")?;
            spool.rewind()?;
            let mut body = Vec::new();
            spool.read_to_end(&mut body)?;
            assert_eq!(body, b"sealed fixture row");
            assert!(one.create_source_spool().is_err());
            let mut prepared = one.create_prepared_spool()?;
            prepared.write_all(b"opaque derived fixture bytes")?;
            assert!(one.create_prepared_spool().is_err());
        }
        one.remove_empty()?;
        assert!(!first.exists());
        let new = ModelJobRoot::create(&base)?;
        let new_path = new.root()?.to_path_buf();
        assert_ne!(first, new_path);
        let index = new.index_root()?;
        let unknown = index.join("unowned-marker");
        std::fs::write(&unknown, b"must survive native job cleanup")?;
        assert!(new.remove_empty().is_err());
        assert!(unknown.exists());
        for job in jobs {
            job.remove_empty()?;
        }
        std::fs::remove_file(unknown)?;
        std::fs::remove_dir(index)?;
        let slot = new_path.parent().ok_or_else(limit)?.to_path_buf();
        std::fs::remove_dir(new_path)?;
        std::fs::remove_dir(slot)?;
        std::fs::remove_dir(base)?;
        std::fs::remove_dir(parent)?;
        Ok(())
    }
    #[test]
    fn model_job_rejects_non_directory_without_creating_a_stage() -> io::Result<()> {
        let (parent, base) = fixture()?;
        let marker = base.join("not-a-directory");
        std::fs::write(&marker, b"fixture")?;
        assert!(ModelJobRoot::create(&marker).is_err());
        assert_eq!(std::fs::read(&marker)?, b"fixture");
        #[cfg(unix)]
        {
            let alias = base.join("alias");
            std::os::unix::fs::symlink(&base, &alias)?;
            assert!(ModelJobRoot::create(&alias).is_err());
            std::fs::remove_file(alias)?;
        }
        assert_eq!(std::fs::read_dir(&base)?.count(), 1);
        std::fs::remove_file(marker)?;
        std::fs::remove_dir(base)?;
        std::fs::remove_dir(parent)?;
        Ok(())
    }
    #[test]
    fn concurrent_owners_cannot_reserve_more_than_four_native_slots() -> io::Result<()> {
        const CONTENDERS: usize = 12;
        let (parent, base) = fixture()?;
        for _ in 0..3 {
            let barrier = std::sync::Barrier::new(CONTENDERS);
            let mut held = std::thread::scope(|scope| {
                let tasks = (0..CONTENDERS)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            ModelJobRoot::create(&base)
                        })
                    })
                    .collect::<Vec<_>>();
                tasks
                    .into_iter()
                    .map(|task| {
                        task.join()
                            .map_err(|_| io::Error::other("contender panicked"))
                    })
                    .collect::<io::Result<Vec<_>>>()
            })?;
            assert_eq!(
                held.iter().filter(|result| result.is_ok()).count(),
                SLOTS.len()
            );
            assert_eq!(std::fs::read_dir(&base)?.count(), SLOTS.len());
            for job in held.drain(..).flatten() {
                job.remove_empty()?;
            }
            assert_eq!(std::fs::read_dir(&base)?.count(), 0);
        }
        std::fs::remove_dir(base)?;
        std::fs::remove_dir(parent)?;
        Ok(())
    }
    #[test]
    fn failed_setup_releases_only_its_new_native_slot() -> io::Result<()> {
        let (parent, base) = fixture()?;
        let confirmed = ModelJobRoot::create(&base)?;
        let keep = confirmed.root()?.to_owned();
        let failed =
            ModelJobRoot::create_with(&base, || Err(io::Error::other("synthetic setup abort")));
        assert!(failed.is_err());
        assert!(keep.exists());
        assert_eq!(std::fs::read_dir(&base)?.count(), 1);
        confirmed.remove_empty()?;
        std::fs::remove_dir(base)?;
        std::fs::remove_dir(parent)?;
        Ok(())
    }
}
