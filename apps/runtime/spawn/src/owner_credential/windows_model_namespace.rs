//! Provision only the fixed, Runtime-owned model namespace on Windows.
use super::*;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Create only missing model-namespace directories under an already canonical,
/// selected installation. Verify existing objects without changing their ACLs.
pub fn provision_model_namespace(installation: &Path, profile_key: &str) -> io::Result<()> {
    if profile_key.len() != 64
        || !profile_key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(denied("invalid managed model profile key"));
    }
    let user = CurrentUser::query_managed()?;
    let descriptor = user.managed_directory_descriptor()?;
    let mut retained = pinned_base_with_sharing(installation, SHARE_ALL, true)?;
    for name in ["runtime", "model-assets-v1", profile_key] {
        retained = open_or_create(
            retained.as_raw_handle(), name, &user, &descriptor, name == "runtime",
        )?;
    }
    Ok(())
}

fn open_or_create(
    parent: HANDLE,
    name: &str,
    user: &CurrentUser,
    descriptor: &Descriptor,
    allow_legacy_base: bool,
) -> io::Result<OwnedHandle> {
    let name = OsStr::new(name);
    let open = || {
        relative_open(
            parent,
            name,
            META | FILE_TRAVERSE,
            SHARE_ALL,
            None,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        )
    };
    let (directory, created) = match open() {
        Ok(existing) => (existing, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match relative_open(
                parent,
                name,
                META | FILE_TRAVERSE,
                SHARE_ALL,
                Some(descriptor),
                FILE_CREATE,
                FILE_DIRECTORY_FILE,
            ) {
                Ok(created) => (created, true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (open()?, false),
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    checked_directory(directory.as_raw_handle())?;
    if created {
        user.verify_new_managed_directory(directory.as_raw_handle())?;
    } else if allow_legacy_base {
        user.verify_legacy_profile_owner(directory.as_raw_handle())?;
    } else {
        user.verify_managed_base(directory.as_raw_handle())?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct OwnedFixture(PathBuf);
    impl Drop for OwnedFixture {
        fn drop(&mut self) {
            let runtime = self.0.join("runtime");
            let parent = runtime.join("model-assets-v1");
            let namespace = parent.join(KEY);
            for path in [
                namespace.join("synthetic-child"),
                namespace,
                parent,
                runtime,
                self.0.clone(),
            ] {
                let _ = std::fs::remove_dir(path);
            }
        }
    }

    #[test]
    fn provisions_exact_owner_only_namespace_and_keeps_existing_directories() -> io::Result<()> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| io::Error::other("fixture randomness failed"))?;
        let base = std::env::temp_dir().join(format!(
            "managed model fixture {:032x}",
            u128::from_ne_bytes(random)
        ));
        std::fs::create_dir(&base)?;
        let temp = OwnedFixture(base);
        let base = temp.0.canonicalize()?;
        assert!(provision_model_namespace(&base, "not-a-profile-key").is_err());
        assert!(!base.join("runtime").exists());
        provision_model_namespace(&base, KEY)?;
        let user = CurrentUser::query()?;
        let runtime = base.join("runtime");
        let parent = runtime.join("model-assets-v1");
        let namespace = parent.join(KEY);
        let check = |path: &Path| -> io::Result<_> {
            let handle = pinned_base_with_sharing(path, SHARE_ALL, true)?;
            user.verify_new_managed_directory(handle.as_raw_handle())?;
            super::super::model_stage::identity(handle.as_raw_handle())
        };
        let before = [check(&runtime)?, check(&parent)?, check(&namespace)?];
        provision_model_namespace(&base, KEY)?;
        assert_eq!(
            before,
            [check(&runtime)?, check(&parent)?, check(&namespace)?]
        );
        let child = namespace.join("synthetic-child");
        std::fs::create_dir(&child)?;
        let child = pinned_base_with_sharing(&child, SHARE_ALL, true)?;
        // ACL inheritance is independent of the object's default owner.
        user.verify_inherited_model_ace(child.as_raw_handle())?;
        CurrentUser::query_managed()?.verify_managed_base(child.as_raw_handle())
    }
}
