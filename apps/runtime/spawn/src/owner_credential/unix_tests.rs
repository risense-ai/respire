use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) struct Scratch(PathBuf);

impl Scratch {
    fn create() -> Self {
        Self::try_create_in(&std::env::temp_dir()).expect("fresh owner test directory")
    }

    pub(super) fn model_fixture() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        if let Ok(root) = Self::try_create_in(Path::new("/dev/shm")) {
            return Ok(root);
        }
        Self::try_create_in(&std::env::temp_dir())
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    fn try_create_in(parent: &Path) -> io::Result<Self> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let parent = parent.canonicalize()?;
        for _ in 0..32 {
            let id = COUNT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                "respire-owner-file-test-{}-{id}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    fn instance(&self, id: &str) -> PathBuf {
        let name = directory_name(id).unwrap();
        self.0.join(name.to_str().unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only this test's fresh exclusive scratch root, never live Runtime data.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn record_round_trip_rotation_and_cleanup() {
    let base = Scratch::create();
    let first = OwnerRecord::from_bytes([0x11; OWNER_RECORD_LEN]);
    let second = OwnerRecord::from_bytes([0x22; OWNER_RECORD_LEN]);
    let mut guard = create_owner_credential(&base.0, "instance-one", &first).unwrap();
    let path = base.instance("instance-one");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(
        fs::metadata(path.join(SECRET_FILE)).unwrap().mode() & 0o7777,
        0o600
    );
    assert_eq!(
        read_owner_credential(&base.0, "instance-one")
            .unwrap()
            .as_bytes(),
        first.as_bytes()
    );
    assert!(matches!(
        create_owner_credential(&base.0, "instance-one", &second),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists
    ));
    let mut old = File::open(path.join(SECRET_FILE)).unwrap();
    guard.publish(&second).unwrap();
    let mut old_bytes = Vec::new();
    old.read_to_end(&mut old_bytes).unwrap();
    assert_eq!(old_bytes, first.as_bytes());
    assert_eq!(
        read_owner_credential(&base.0, "instance-one")
            .unwrap()
            .as_bytes(),
        second.as_bytes()
    );
    drop(guard);
    assert!(!path.exists());
}

#[test]
fn refuses_link_and_overbroad_permissions_without_touching_unrelated_file() {
    let base = Scratch::create();
    let record = OwnerRecord::from_bytes([0x33; OWNER_RECORD_LEN]);
    let external = base.0.join("unrelated");
    fs::write(&external, [0x44; OWNER_RECORD_LEN]).unwrap();
    let instance = base.instance("instance-two");
    let guard = create_owner_credential(&base.0, "instance-two", &record).unwrap();
    let file = instance.join(SECRET_FILE);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_owner_credential(&base.0, "instance-two").is_err());
    fs::remove_file(&file).unwrap();
    symlink(&external, &file).unwrap();
    assert!(read_owner_credential(&base.0, "instance-two").is_err());
    drop(guard);
    assert!(fs::symlink_metadata(&file).unwrap().is_symlink());
    assert_eq!(fs::read(external).unwrap(), [0x44; OWNER_RECORD_LEN]);
}

#[test]
fn refuses_replaced_directory_and_uses_instance_specific_cleanup() {
    let base = Scratch::create();
    let record = OwnerRecord::from_bytes([0x55; OWNER_RECORD_LEN]);
    let mut guard = create_owner_credential(&base.0, "instance-three", &record).unwrap();
    let active = base.instance("instance-three");
    let renamed = base.0.join("previous-path");
    fs::rename(&active, &renamed).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&active).unwrap();
    assert!(guard.publish(&record).is_err());
    drop(guard);
    assert!(active.is_dir());
    assert!(!renamed.join(SECRET_FILE).exists());
    assert!(read_owner_credential(&base.0, "../instance-three").is_err());
}

#[test]
fn rejects_filesystems_without_verified_local_ownership() {
    #[cfg(target_os = "linux")]
    {
        for local in [0xEF53, 0x58465342, 0x9123683E, 0x01021994] {
            assert!(owner_filesystem(local));
        }
        for unverified in [0x65735546, 0x6969, 0xFF534D42, 0x794C7630, 0] {
            assert!(!owner_filesystem(unverified));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let local = libc::MNT_LOCAL as u32;
        assert!(owner_filesystem(local, b"apfs"));
        assert!(owner_filesystem(local, b"hfs"));
        assert!(!owner_filesystem(0, b"apfs"));
        assert!(!owner_filesystem(local, b"macfuse"));
        assert!(!owner_filesystem(local, b"smbfs"));
        assert!(!owner_filesystem(
            local | libc::MNT_IGNORE_OWNERSHIP as u32,
            b"apfs",
        ));
    }
}
