use super::*;

struct Fixture {
    directory: Removable,
    path: PathBuf,
}
impl Fixture {
    fn new() -> io::Result<Self> {
        let temp = std::env::temp_dir();
        let parent = pinned_base(&temp)?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|_| Error::other("fixture randomness failed"))?;
        let name = format!(
            "respire-credential-test-{:016x}",
            u64::from_ne_bytes(random)
        );
        let user = CurrentUser::query()?;
        let descriptor = user.descriptor()?;
        let dir = relative_open(
            parent.as_raw_handle(),
            OsStr::new(&name),
            META | DELETE | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(&descriptor),
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        Ok(Self {
            directory: Removable(File::from(dir)),
            path: temp.join(name),
        })
    }
}

fn record(value: u8) -> OwnerRecord {
    let mut bytes = [value; OWNER_RECORD_LEN];
    bytes[0] = 0;
    OwnerRecord::from_bytes(bytes)
}

#[test]
fn windows_roundtrip_rotate_open_old_snapshot_and_clean() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let a = record(5);
    let b = record(6);
    assert!(read_owner_credential(&fixture.path, "absent").is_err());
    assert!(create_owner_credential(&fixture.path, "../escape", &a).is_err());
    let mut writer = create_owner_credential(&fixture.path, "one", &a)
        .map_err(|e| Error::other(format!("initial publication: {e:?}")))?;
    assert!(create_owner_credential(&fixture.path, "one", &b).is_err());
    assert!(read_owner_credential(&fixture.path, "one")?.as_bytes() == a.as_bytes());
    let old_file = relative_open(
        writer.directory.handle(),
        OsStr::new(SECRET_FILE),
        GENERIC_READ | META,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?;
    let mut old_file = File::from(old_file);
    writer
        .publish(&b)
        .map_err(|e| Error::other(format!("atomic replacement: {e:?}")))?;
    let mut old_bytes = [0u8; OWNER_RECORD_LEN];
    old_file.read_exact(&mut old_bytes)?;
    assert!(old_bytes == *a.as_bytes());
    assert!(read_owner_credential(&fixture.path, "one")?.as_bytes() == b.as_bytes());
    drop(old_file);
    drop(writer);
    assert!(read_owner_credential(&fixture.path, "one").is_err());
    let name = directory_name("one")?;
    assert!(!fixture
        .path
        .join(name.to_str().map_err(Error::other)?)
        .exists());
    Ok(())
}

#[test]
fn windows_denies_short_links_and_insecure_object() -> io::Result<()> {
    let fixture = Fixture::new()?;
    let user = CurrentUser::query()?;
    let descriptor = user.descriptor()?;
    let name = directory_name("invalid")?;
    let name = OsStr::new(name.to_str().map_err(Error::other)?);
    let dir = relative_open(
        fixture.directory.handle(),
        name,
        META | DELETE | FILE_ADD_FILE | FILE_TRAVERSE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        Some(&descriptor),
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
    )?;
    let dir = Removable(File::from(dir));
    let open = |desc| {
        relative_open(
            dir.handle(),
            OsStr::new(SECRET_FILE),
            GENERIC_WRITE | META | DELETE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            desc,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )
    };
    let short = Removable(File::from(open(Some(&descriptor))?));
    assert!(read_owner_credential(&fixture.path, "invalid").is_err());
    drop(short);
    // Omitting source protection deliberately creates an inherited DACL.
    // This is a synthetic test object, not the production create path.
    let mut inherited = Removable(File::from(open(None)?));
    inherited.0.write_all(record(1).as_bytes())?;
    inherited.0.sync_all()?;
    assert!(read_owner_credential(&fixture.path, "invalid").is_err());
    drop(inherited);

    let mut linked = Removable(File::from(open(Some(&descriptor))?));
    linked.0.write_all(record(2).as_bytes())?;
    linked.0.sync_all()?;
    assert!(read_owner_credential(&fixture.path, "invalid").is_ok());
    let link = fixture.path.join("extra-link");
    std::fs::hard_link(fixture.path.join(name).join(SECRET_FILE), &link)?;
    let linked_result = read_owner_credential(&fixture.path, "invalid");
    std::fs::remove_file(link)?;
    assert!(linked_result.is_err());
    Ok(())
}

#[test]
fn windows_refuses_an_actual_junction_in_the_base_path() -> io::Result<()> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let fixture = Fixture::new()?;
    let owner = CurrentUser::query()?.descriptor()?;
    let create = |name: &str| -> io::Result<Removable> {
        Ok(Removable(File::from(relative_open(
            fixture.directory.handle(),
            OsStr::new(name),
            GENERIC_WRITE | META | DELETE | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(&owner),
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?)))
    };
    let target = create("target")?;
    let junction = create("junction")?;
    let display = fixture.path.join("target");
    let mut substitute = std::ffi::OsString::from(r"\??\");
    substitute.push(display.as_os_str());
    let substitute = wide(&substitute)?;
    let display = wide(display.as_os_str())?;
    let bytes = |s: &[u16]| std::mem::size_of_val(s) as u16;
    let mut reparse = Vec::<u8>::new();
    reparse.extend_from_slice(&0xa0000003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    for n in [
        8 + bytes(&substitute) + bytes(&display),
        0,
        0,
        bytes(&substitute) - 2,
        bytes(&substitute),
        bytes(&display) - 2,
    ] {
        reparse.extend_from_slice(&n.to_le_bytes());
    }
    for unit in substitute.into_iter().chain(display) {
        reparse.extend_from_slice(&unit.to_le_bytes());
    }
    let mut done = 0;
    win(unsafe {
        DeviceIoControl(
            junction.handle(),
            0x0009_00a4,
            reparse.as_ptr().cast(),
            reparse.len() as u32,
            null_mut(),
            0,
            &mut done,
            null_mut(),
        )
    })?; // FSCTL_SET_REPARSE_POINT, only on this exact disposable directory.
    let linked_base = fixture.path.join("junction");
    assert!(pinned_base(&linked_base).is_err());
    assert!(create_owner_credential(&linked_base, "should-not-exist", &record(4)).is_err());
    assert!(!fixture
        .path
        .join("target/.owner-credentials-should-not-exist")
        .exists());
    drop(junction);
    drop(target);
    Ok(())
}

#[test]
fn windows_requires_persistent_acl_support_from_the_open_volume() -> io::Result<()> {
    let fixture = Fixture::new()?;
    verify_acl_volume(fixture.directory.handle())?;
    assert_eq!(
        require_persistent_acls(0x0000_0004).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    ); // Unicode support alone is not enough.
    assert!(require_persistent_acls(FILE_PERSISTENT_ACLS | 0x0000_0004).is_ok());
    Ok(())
}
