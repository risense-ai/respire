//! Windows same-user credential storage, independent of launch authorization.

#[path = "windows_acl.rs"]
mod acl;

use self::acl::{denied, wide, CurrentUser, Descriptor};
use super::{directory_name, OwnerRecord, OWNER_RECORD_LEN, SECRET_FILE};
use std::{
    ffi::{c_void, OsStr},
    fs::File,
    io::{self, Error, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FileRenameInformationEx, NtCreateFile, NtSetInformationFile, FILE_CREATE,
            FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
            OBJ_CASE_INSENSITIVE, UNICODE_STRING,
        },
        Storage::FileSystem::{
            CreateFileW, FileDispositionInfo, GetDriveTypeW, GetFileInformationByHandle,
            GetFileType, GetVolumeInformationByHandleW, SetFileInformationByHandle,
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ADD_FILE, FILE_ATTRIBUTE_DEVICE,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
            FILE_TYPE_DISK, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

// Win32 file system flag: the volume preserves and enforces ACLs.
const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
const PENDING: &str = ".pending";
const META: u32 = FILE_READ_ATTRIBUTES | READ_CONTROL;
const DIR_FLAGS: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
// FILE_RENAME_FLAG_REPLACE_IF_EXISTS, FILE_RENAME_FLAG_POSIX_SEMANTICS.
const ATOMIC_REPLACE: u32 = 0x1 | 0x2;

pub struct OwnerCredential {
    published: Option<Removable>,
    directory: Removable,
}

impl OwnerCredential {
    pub fn publish(&mut self, record: &OwnerRecord) -> io::Result<()> {
        let user = CurrentUser::query()?;
        user.verify_handle(self.directory.handle(), true)?;
        let private = user.descriptor()?;
        let file = relative_open(
            self.directory.handle(),
            OsStr::new(PENDING),
            GENERIC_WRITE | META | DELETE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            Some(&private),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        // Construct the handle-based cleanup guard immediately after CREATE_NEW.
        let mut candidate = Removable(File::from(file));
        checked_file(candidate.handle(), None)?;
        user.verify_handle(candidate.handle(), true)?;
        candidate.0.write_all(record.as_bytes())?;
        candidate.0.sync_all()?;
        checked_file(candidate.handle(), Some(OWNER_RECORD_LEN as u64))?;
        rename_to_secret(&candidate, &self.directory, self.published.is_some())?;
        // POSIX replacement unlinks the previous file. Dropping its retained
        // HANDLE cannot delete the new object; distinct file identities.
        self.published = Some(candidate);
        Ok(())
    }
}

pub fn create_owner_credential(
    base: &Path,
    instance_id: &str,
    record: &OwnerRecord,
) -> io::Result<OwnerCredential> {
    let name = directory_name(instance_id)?;
    let name = name.to_str().map_err(Error::other)?;
    let base = pinned_base(base)?;
    let user = CurrentUser::query()?;
    user.verify_handle(base.as_raw_handle(), false)?;
    let descriptor = user.descriptor()?;
    // NtCreateFile links a new directory under the exact validated base
    // HANDLE; FILE_CREATE never opens or changes an existing path.
    let directory = relative_open(
        base.as_raw_handle(),
        OsStr::new(name),
        META | DELETE | FILE_ADD_FILE | FILE_TRAVERSE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        Some(&descriptor),
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
    )?;
    let directory = Removable(File::from(directory));
    checked_directory(directory.handle())?;
    user.verify_handle(directory.handle(), true)?;
    let mut state = OwnerCredential {
        published: None,
        directory,
    };
    state.publish(record)?;
    Ok(state)
}

pub fn read_owner_credential(base: &Path, instance_id: &str) -> io::Result<OwnerRecord> {
    let name = directory_name(instance_id)?;
    let name = name.to_str().map_err(Error::other)?;
    let base = pinned_base(base)?;
    let user = CurrentUser::query()?;
    user.verify_handle(base.as_raw_handle(), false)?;
    let directory = relative_open(
        base.as_raw_handle(),
        OsStr::new(name),
        META | FILE_TRAVERSE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
    )?;
    checked_directory(directory.as_raw_handle())?;
    user.verify_handle(directory.as_raw_handle(), true)?;
    let input = relative_open(
        directory.as_raw_handle(),
        OsStr::new(SECRET_FILE),
        GENERIC_READ | META,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )?;
    // The open writer shares READ and DELETE, never WRITE.
    checked_file(input.as_raw_handle(), Some(OWNER_RECORD_LEN as u64))?;
    user.verify_handle(input.as_raw_handle(), true)?;
    let mut input = File::from(input);
    let mut bytes = [0u8; OWNER_RECORD_LEN];
    let result = (|| {
        input.read_exact(&mut bytes)?;
        if input.read(&mut [0])? != 0 {
            return Err(denied("owner credential length changed"));
        }
        Ok(OwnerRecord::from_bytes(bytes))
    })();
    bytes.fill(0);
    result
}

fn pinned_base(base: &Path) -> io::Result<OwnedHandle> {
    pinned_base_with_sharing(
        base,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        false,
    )
}

fn pinned_base_with_sharing(
    base: &Path,
    last_share: u32,
    canonical_model_path: bool,
) -> io::Result<OwnedHandle> {
    let mut components = base.components().peekable();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(denied("owner credential requires an absolute Windows path"));
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) => drive,
        Prefix::VerbatimDisk(drive) if canonical_model_path => drive,
        _ => {
            return Err(denied(
                "owner credential requires a local Windows drive path",
            ))
        }
    };
    if components.next() != Some(Component::RootDir) {
        return Err(denied(
            "owner credential requires a local Windows drive path",
        ));
    }
    let path = PathBuf::from(format!("{}:\\", char::from(drive)));
    let name = wide(path.as_os_str())?;
    // DRIVE_FIXED; mapped, remote, removable and unknown volumes are denied.
    if unsafe { GetDriveTypeW(name.as_ptr()) } != 3 || components.peek().is_none() {
        return Err(denied(
            "owner credential requires a fixed local disk directory",
        ));
    }
    let first = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            DIR_FLAGS,
            null_mut(),
        )
    };
    let mut retained = owned(first)?;
    checked_directory(retained.as_raw_handle())?;
    while let Some(term) = components.next() {
        let Component::Normal(name) = term else {
            return Err(denied("owner credential path contains invalid components"));
        };
        let access = if components.peek().is_none() {
            META
        } else {
            FILE_READ_ATTRIBUTES
        };
        let next = relative_open(
            retained.as_raw_handle(),
            name,
            access | FILE_TRAVERSE,
            if components.peek().is_none() {
                last_share
            } else {
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            },
            None,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        )?;
        checked_directory(next.as_raw_handle())?;
        // Resolve the next name under the current retained kernel directory,
        // never by joining or re-opening any of the previously checked paths.
        retained = next;
    }
    verify_acl_volume(retained.as_raw_handle())?;
    Ok(retained)
}

fn verify_acl_volume(handle: HANDLE) -> io::Result<()> {
    let mut flags = 0;
    // SAFETY: Retained handle owns the validated base directory; the flags
    // are the only requested output. There is no pathname-based volume query.
    win(unsafe {
        GetVolumeInformationByHandleW(
            handle,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    })?;
    require_persistent_acls(flags)
}

fn require_persistent_acls(flags: u32) -> io::Result<()> {
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(denied(
            "owner credential volume does not enforce persistent ACLs",
        ));
    }
    Ok(())
}

fn relative_open(
    parent: HANDLE,
    name: &OsStr,
    access: u32,
    share: u32,
    descriptor: Option<&Descriptor>,
    disposition: u32,
    kind: u32,
) -> io::Result<OwnedHandle> {
    let wide = wide(name)?;
    let chars = &wide[..wide.len() - 1];
    let len = std::mem::size_of_val(chars);
    if !(2..=510).contains(&len)
        || matches!(chars, [46] | [46, 46])
        || chars.last().is_some_and(|ch| *ch == 32 || *ch == 46)
        || chars.iter().any(|ch| matches!(*ch, 47 | 58 | 92))
    {
        return Err(denied(
            "owner credential path contains an alias, stream or separator",
        ));
    }
    let native = UNICODE_STRING {
        Length: len as u16,
        MaximumLength: len as u16,
        Buffer: wide.as_ptr().cast_mut(),
    };
    let attrs = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent,
        ObjectName: &native,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor.map_or(null(), Descriptor::raw),
        SecurityQualityOfService: null(),
    };
    let mut info = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    // SAFETY: All arguments are live for the synchronous native call, and
    // ObjectName is one component relative to the actual opened parent.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attrs,
            &mut info,
            null(),
            0,
            share,
            disposition,
            kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    owned(handle)
}

fn metadata(h: HANDLE) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    if unsafe { GetFileType(h) } != FILE_TYPE_DISK {
        return Err(denied("owner credential must be a disk object"));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    win(unsafe { GetFileInformationByHandle(h, &mut info) })?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE) != 0 {
        return Err(denied("owner credential cannot use any reparse or device"));
    }
    Ok(info)
}

fn checked_directory(h: HANDLE) -> io::Result<()> {
    if metadata(h)?.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(denied("owner credential path is not a directory"));
    }
    Ok(())
}

fn checked_file(h: HANDLE, size: Option<u64>) -> io::Result<()> {
    let f = metadata(h)?;
    let actual = (u64::from(f.nFileSizeHigh) << 32) | u64::from(f.nFileSizeLow);
    if f.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
        || f.nNumberOfLinks != 1
        || size.is_some_and(|expected| actual != expected)
    {
        return Err(denied(
            "owner credential file type, links or length invalid",
        ));
    }
    Ok(())
}

fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

fn win(answer: i32) -> io::Result<()> {
    if answer == 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Removable(File);
impl Removable {
    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
}
impl Drop for Removable {
    fn drop(&mut self) {
        // Never delete a string path when cleaning up. Windows may leave an
        // empty directory if interrupted or if a reader is still closing.
        let _ = mark_delete(self.handle());
    }
}

fn mark_delete(handle: HANDLE) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    win(unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    })
}

#[path = "windows_model_removal.rs"]
pub(super) mod model_removal;
#[path = "windows_model_namespace.rs"]
pub(super) mod model_namespace;
#[path = "windows_model_stage.rs"]
pub(super) mod model_stage;

fn rename_to_secret(input: &Removable, directory: &Removable, replace: bool) -> io::Result<()> {
    rename_relative(
        input.handle(),
        directory.handle(),
        OsStr::new(SECRET_FILE),
        if replace { ATOMIC_REPLACE } else { 0 },
    )
}

fn rename_relative(input: HANDLE, directory: HANDLE, leaf: &OsStr, flags: u32) -> io::Result<()> {
    let name = wide(leaf)?;
    let name = &name[..name.len() - 1];
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let length = offset + std::mem::size_of_val(name);
    let bytes = length.max(std::mem::size_of::<FILE_RENAME_INFO>());
    let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    let raw = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: Aligned allocation contains the complete Win32 flexible
    // structure and name. RootDirectory is pinned and name is constant.
    unsafe {
        (*raw).Anonymous.Flags = flags;
        (*raw).RootDirectory = directory;
        (*raw).FileNameLength = std::mem::size_of_val(name) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            raw.cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        let mut status_block = IO_STATUS_BLOCK::default();
        let status = NtSetInformationFile(
            input,
            &mut status_block,
            raw.cast::<c_void>(),
            bytes as u32,
            FileRenameInformationEx,
        );
        if status < 0 {
            Err(Error::from_raw_os_error(
                RtlNtStatusToDosError(status) as i32
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
