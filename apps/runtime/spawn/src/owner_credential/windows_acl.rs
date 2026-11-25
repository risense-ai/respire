//! Win32 descriptors for the same-OS-user owner credential.
use std::{
    ffi::{c_void, OsStr},
    io::{self, Error, ErrorKind},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::{LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        CheckTokenMembership, CreateWellKnownSid, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, IsValidAcl, IsValidSecurityDescriptor, IsValidSid, TokenElevation,
        TokenOwner, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE,
        ACE_HEADER, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED,
        TOKEN_ELEVATION, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, WELL_KNOWN_SID_TYPE,
    },
    Storage::FileSystem::{FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const ALLOW_ACE: u8 = 0;
const DENY_ACE: u8 = 1;
const MAX_SID_WIDE: usize = 185;
const PRIVATE_RIGHTS: u32 = FILE_ALL_ACCESS;
const BASE_READ_ONLY: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | 0xa000_0000;

pub(super) fn denied(message: &'static str) -> Error {
    Error::new(ErrorKind::PermissionDenied, message)
}
fn check(value: bool, message: &'static str) -> io::Result<()> {
    if value {
        Ok(())
    } else {
        Err(denied(message))
    }
}
fn win(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Windows path contains NUL",
        ));
    }
    Ok(value.into_iter().chain([0]).collect())
}

struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Only holds a unique allocation from a Win32 LocalAlloc API.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
pub(super) struct CurrentUser {
    // TokenUser embeds a pointer into its own checked, aligned OS buffer.
    _storage: Vec<usize>,
    sid: PSID,
    managed_admin_default: bool,
}
impl CurrentUser {
    pub(super) fn query() -> io::Result<Self> {
        Self::query_with_managed_base(false)
    }

    pub(super) fn query_managed() -> io::Result<Self> {
        Self::query_with_managed_base(true)
    }

    fn query_with_managed_base(managed: bool) -> io::Result<Self> {
        let mut raw = null_mut();
        // SAFETY: Current process pseudo handle, and an output HANDLE.
        win(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) })?;
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut len = 0;
        let first = unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut len)
        };
        check(
            first == 0
                && Error::last_os_error().raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32)
                && (std::mem::size_of::<TOKEN_USER>()..=4096).contains(&(len as usize)),
            "cannot query Windows token owner",
        )?;
        let mut storage = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        let ptr = storage.as_mut_ptr();
        win(unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, ptr.cast(), len, &mut len)
        })?;
        let sid = unsafe { (*ptr.cast::<TOKEN_USER>()).User.Sid };
        check(
            !sid.is_null() && unsafe { IsValidSid(sid) } != 0,
            "invalid Windows token owner",
        )?;
        Ok(Self {
            _storage: storage,
            sid,
            // No alias when any native evidence is unavailable.
            managed_admin_default: managed && elevated_admin_owner(&token).unwrap_or(false),
        })
    }

    pub(super) fn descriptor(&self) -> io::Result<Descriptor> {
        self.owner_descriptor("")
    }

    pub(super) fn managed_directory_descriptor(&self) -> io::Result<Descriptor> {
        self.owner_descriptor("OICI")
    }

    fn owner_descriptor(&self, inheritance: &str) -> io::Result<Descriptor> {
        let mut raw = null_mut();
        // SAFETY: Valid SID retained by CurrentUser; conversion allocates a bounded SID string.
        win(unsafe { ConvertSidToStringSidW(self.sid, &mut raw) })?;
        let sid = Local(raw.cast());
        let mut len = 0;
        while len < MAX_SID_WIDE && unsafe { *sid.0.cast::<u16>().add(len) } != 0 {
            len += 1;
        }
        check(len < MAX_SID_WIDE, "Windows owner SID was too long")?;
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid.0.cast(), len) })
            .map_err(|_| denied("invalid Windows owner SID"))?;
        // O sets the actual owner, D:P disables inherited access. Only the
        // current native SID receives an ACE; children get descriptors afresh.
        let sddl = wide(OsStr::new(&format!(
            "O:{sid}D:P(A;{inheritance};FA;;;{sid})"
        )))?;
        Descriptor::from_wide(&sddl)
    }

    pub(super) fn verify_handle(&self, handle: HANDLE, private: bool) -> io::Result<()> {
        self.verify_handle_as(handle, private.then_some(0), false)
    }

    pub(super) fn verify_managed_base(&self, handle: HANDLE) -> io::Result<()> {
        self.verify_handle_as(handle, None, self.managed_admin_default)
    }

    /// Restoration creates its own protected child; it does not broaden or
    /// require rewriting the existing profile's discretionary ACL.
    pub(super) fn verify_legacy_profile_owner(&self, handle: HANDLE) -> io::Result<()> {
        use windows_sys::Win32::Security::GetSecurityDescriptorOwner;
        let descriptor = descriptor_from_handle(handle)?;
        let mut owner = null_mut();
        let mut ignored = 0;
        win(unsafe { GetSecurityDescriptorOwner(descriptor.0 .0, &mut owner, &mut ignored) })?;
        let admins = KnownSid::new(WinBuiltinAdministratorsSid)?;
        check(
            owner_matches(same(self.sid, owner), false, self.managed_admin_default, same(admins.ptr(), owner)),
            "Windows restore profile does not belong to the operating system user",
        )
    }
    pub(super) fn verify_new_managed_directory(&self, handle: HANDLE) -> io::Result<()> {
        // CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE; no inherited ACE.
        self.verify_handle_as(handle, Some(3), false)
    }

    fn verify_handle_as(
        &self,
        handle: HANDLE,
        owner_only_flags: Option<u8>,
        managed_admin_default: bool,
    ) -> io::Result<()> {
        let descriptor = descriptor_from_handle(handle)?;
        self.verify_descriptor(&descriptor, owner_only_flags, managed_admin_default)
    }

    pub(super) fn verify_inherited_model_ace(&self, handle: HANDLE) -> io::Result<()> {
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, INHERITED_ACE};
        let descriptor = descriptor_from_handle(handle)?;
        let mut found = 0;
        let mut acl = null_mut();
        let mut ignored = 0;
        win(unsafe {
            GetSecurityDescriptorDacl(descriptor.0 .0, &mut found, &mut acl, &mut ignored)
        })?;
        check(
            found != 0
                && !acl.is_null()
                && unsafe { IsValidAcl(acl) } != 0
                && unsafe { (*acl).AceCount } == 1,
            "expected one inherited model ACE",
        )?;
        let mut entry = null_mut();
        win(unsafe { GetAce(acl, 0, &mut entry) })?;
        let header = unsafe { &*entry.cast::<ACE_HEADER>() };
        check(
            header.AceType == ALLOW_ACE
                && u32::from(header.AceFlags) & INHERITED_ACE != 0
                && usize::from(header.AceSize) >= std::mem::size_of::<ACCESS_ALLOWED_ACE>(),
            "model ACE did not inherit an allow rule",
        )?;
        let allow = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
        let account = std::ptr::from_ref(&allow.SidStart).cast_mut().cast();
        check(
            allow.Mask == PRIVATE_RIGHTS && same(self.sid, account),
            "inherited model ACE is not full access for TokenUser",
        )
    }

    fn verify_descriptor(
        &self,
        descriptor: &Descriptor,
        owner_only_flags: Option<u8>,
        managed_admin_default: bool,
    ) -> io::Result<()> {
        let private = owner_only_flags.is_some();
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};
        let sd = descriptor.0 .0;
        check(
            !sd.is_null() && unsafe { IsValidSecurityDescriptor(sd) } != 0,
            "invalid Windows credential descriptor",
        )?;
        let mut owner = null_mut();
        let mut ignored = 0;
        win(unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut ignored) })?;
        let admins = KnownSid::new(WinBuiltinAdministratorsSid)?;
        check(
            owner_matches(
                same(self.sid, owner),
                private,
                managed_admin_default,
                same(admins.ptr(), owner),
            ),
            "Windows credential or directory has a different owner",
        )?;
        let mut control = 0;
        let mut revision = 0;
        win(unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) })?;
        if private {
            check(
                control & SE_DACL_PROTECTED != 0,
                "Windows credential ACL inherits permissions",
            )?;
        }
        let mut found = 0;
        let mut acl = null_mut();
        win(unsafe { GetSecurityDescriptorDacl(sd, &mut found, &mut acl, &mut ignored) })?;
        check(
            found != 0 && !acl.is_null() && unsafe { IsValidAcl(acl) } != 0,
            "Windows credential has no valid discretionary ACL",
        )?;
        let system = KnownSid::new(WinLocalSystemSid)?;
        let count = unsafe { (*acl).AceCount };
        if private {
            check(count == 1, "Windows credential ACL is not owner-only")?;
        }
        for i in 0..u32::from(count) {
            let mut entry = null_mut();
            win(unsafe { GetAce(acl, i, &mut entry) })?;
            let header = unsafe { &*entry.cast::<ACE_HEADER>() };
            if !private && header.AceType == DENY_ACE {
                continue;
            }
            check(
                header.AceType == ALLOW_ACE
                    && usize::from(header.AceSize) >= std::mem::size_of::<ACCESS_ALLOWED_ACE>(),
                "Windows credential ACL has an unsupported ACE",
            )?;
            let allow = unsafe { &*entry.cast::<ACCESS_ALLOWED_ACE>() };
            let sid_start = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            let sid_bytes = usize::from(header.AceSize) - sid_start;
            check(sid_bytes >= 8, "Windows credential ACL has a truncated SID")?;
            let sid = unsafe { entry.cast::<u8>().add(sid_start) };
            let subauthorities = usize::from(unsafe { *sid.add(1) });
            check(
                8 + 4 * subauthorities <= sid_bytes,
                "Windows credential ACL SID exceeds its ACE",
            )?;
            let account = sid.cast();
            if let Some(expected_flags) = owner_only_flags {
                check(
                    header.AceFlags == expected_flags
                        && same(self.sid, account)
                        && allow.Mask == PRIVATE_RIGHTS,
                    "Windows credential ACL is not protected owner-only access",
                )?;
            } else {
                // Base ACL may allow SYSTEM/Administrators to maintain the
                // filesystem and other accounts to read/traverse, never mutate.
                check(
                    same(self.sid, account)
                        || same(system.ptr(), account)
                        || same(admins.ptr(), account)
                        || allow.Mask & !BASE_READ_ONLY == 0,
                    "another Windows user can replace credential directory entries",
                )?;
            }
        }
        Ok(())
    }
}

fn descriptor_from_handle(handle: HANDLE) -> io::Result<Descriptor> {
    let mut result = null_mut();
    // SAFETY: The handle is owned by the caller. The returned descriptor owns its ACL and SID.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut result,
        )
    };
    let descriptor = Descriptor(Local(result));
    if status != 0 {
        return Err(Error::from_raw_os_error(status as i32));
    }
    Ok(descriptor)
}

fn owner_matches(
    current_user: bool,
    strict: bool,
    elevated_default_admin: bool,
    admin: bool,
) -> bool {
    current_user || (!strict && elevated_default_admin && admin)
}

fn elevated_admin_owner(token: &OwnedHandle) -> io::Result<bool> {
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0;
    // SAFETY: Fixed native result in an aligned, initialized value.
    win(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            std::ptr::from_mut(&mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    })?;
    if len as usize != std::mem::size_of::<TOKEN_ELEVATION>() || elevation.TokenIsElevated == 0 {
        return Ok(false);
    }
    let first =
        unsafe { GetTokenInformation(token.as_raw_handle(), TokenOwner, null_mut(), 0, &mut len) };
    let head = std::mem::size_of::<TOKEN_OWNER>();
    check(
        first == 0
            && Error::last_os_error().raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32)
            && (head + 8..=4096).contains(&(len as usize)),
        "cannot query Windows default token owner",
    )?;
    let mut storage = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
    let ptr = storage.as_mut_ptr();
    let capacity = storage.len() * std::mem::size_of::<usize>();
    win(unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenOwner, ptr.cast(), len, &mut len)
    })?;
    check(
        (head + 8..=capacity).contains(&(len as usize)),
        "invalid Windows default token owner length",
    )?;
    let owner = unsafe { (*ptr.cast::<TOKEN_OWNER>()).Owner };
    // Check the whole SID is inside the native token buffer before using it.
    let Some(offset) = (owner as usize)
        .checked_sub(ptr as usize)
        .filter(|offset| *offset >= head && *offset <= len as usize - 8)
    else {
        return Ok(false);
    };
    let count = unsafe { *ptr.cast::<u8>().add(offset + 1) } as usize;
    if 8 + 4 * count > len as usize - offset {
        return Ok(false);
    }
    let admins = KnownSid::new(WinBuiltinAdministratorsSid)?;
    if !same(admins.ptr(), owner) {
        return Ok(false);
    }
    let mut enabled = 0;
    // NULL checks effective membership, not a group present only as deny-only.
    win(unsafe { CheckTokenMembership(null_mut(), admins.ptr(), &mut enabled) })?;
    Ok(enabled != 0)
}

fn same(a: PSID, b: PSID) -> bool {
    !a.is_null() && !b.is_null() && unsafe { IsValidSid(b) != 0 && EqualSid(a, b) != 0 }
}

struct KnownSid([u32; 32]);
impl KnownSid {
    fn new(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Self> {
        let mut sid = Self([0; 32]);
        let mut len = std::mem::size_of_val(&sid) as u32;
        win(unsafe { CreateWellKnownSid(kind, null_mut(), sid.0.as_mut_ptr().cast(), &mut len) })?;
        Ok(sid)
    }
    fn ptr(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }
}

pub(super) struct Descriptor(Local);
impl Descriptor {
    fn from_wide(sddl: &[u16]) -> io::Result<Self> {
        let mut raw = null_mut();
        win(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut raw,
                null_mut(),
            )
        })?;
        Ok(Self(Local(raw)))
    }
    pub(super) fn raw(&self) -> *const windows_sys::Win32::Security::SECURITY_DESCRIPTOR {
        self.0 .0.cast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_denies_everyone_wrong_owner_and_inheritance() -> io::Result<()> {
        let user = CurrentUser::query()?;
        user.verify_descriptor(&user.descriptor()?, Some(0), false)?;
        for s in [
            "O:SYD:P(A;;FA;;;SY)",
            "O:SYD:P(A;;FA;;;WD)",
            "O:SYD:(A;;FA;;;SY)",
        ] {
            let descriptor = Descriptor::from_wide(&wide(OsStr::new(s))?)?;
            assert!(user.verify_descriptor(&descriptor, Some(0), false).is_err());
        }
        Ok(())
    }
}

#[cfg(test)]
mod managed_owner_policy_tests {
    use super::*;

    #[test]
    fn modeled_legacy_owner_keeps_all_existing_acl_rules() -> io::Result<()> {
        let user = CurrentUser::query()?;
        let legacy = Descriptor::from_wide(&wide(OsStr::new("O:BAD:P(A;;FA;;;BA)"))?)?;
        let other_writes =
            Descriptor::from_wide(&wide(OsStr::new("O:BAD:P(A;;FA;;;BA)(A;;FA;;;WD)"))?)?;
        assert!(user.verify_descriptor(&legacy, None, false).is_err());
        user.verify_descriptor(&legacy, None, true)?;
        assert!(user.verify_descriptor(&legacy, Some(0), true).is_err());
        assert!(user.verify_descriptor(&other_writes, None, true).is_err());
        Ok(())
    }

    #[test]
    fn only_actual_elevated_admin_owner_is_eligible_for_a_managed_base() {
        assert!(owner_matches(true, false, false, false));
        assert!(owner_matches(true, true, false, false));
        assert!(owner_matches(false, false, true, true));
        assert!(!owner_matches(false, false, false, true));
        assert!(!owner_matches(false, false, true, false));
        assert!(!owner_matches(false, true, true, true));
    }
}
