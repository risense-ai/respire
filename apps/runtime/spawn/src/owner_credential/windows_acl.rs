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
        CreateWellKnownSid, EqualSid, GetAce, GetSecurityDescriptorControl, GetTokenInformation,
        IsValidAcl, IsValidSecurityDescriptor, IsValidSid, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
        WELL_KNOWN_SID_TYPE,
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
}
impl CurrentUser {
    pub(super) fn query() -> io::Result<Self> {
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
        })
    }

    pub(super) fn descriptor(&self) -> io::Result<Descriptor> {
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
        let sddl = wide(OsStr::new(&format!("O:{sid}D:P(A;;FA;;;{sid})")))?;
        Descriptor::from_wide(&sddl)
    }

    pub(super) fn verify_handle(&self, handle: HANDLE, private: bool) -> io::Result<()> {
        let mut result = null_mut();
        // SAFETY: Handle is still owned by caller. Returned descriptor owns ACL and SID.
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
        self.verify_descriptor(&descriptor, private)
    }

    fn verify_descriptor(&self, descriptor: &Descriptor, private: bool) -> io::Result<()> {
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};
        let sd = descriptor.0 .0;
        check(
            !sd.is_null() && unsafe { IsValidSecurityDescriptor(sd) } != 0,
            "invalid Windows credential descriptor",
        )?;
        let mut owner = null_mut();
        let mut ignored = 0;
        win(unsafe { GetSecurityDescriptorOwner(sd, &mut owner, &mut ignored) })?;
        check(
            same(self.sid, owner),
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
        let admins = KnownSid::new(WinBuiltinAdministratorsSid)?;
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
            if private {
                check(
                    header.AceFlags == 0 && same(self.sid, account) && allow.Mask == PRIVATE_RIGHTS,
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
        user.verify_descriptor(&user.descriptor()?, true)?;
        for s in [
            "O:SYD:P(A;;FA;;;SY)",
            "O:SYD:P(A;;FA;;;WD)",
            "O:SYD:(A;;FA;;;SY)",
        ] {
            let descriptor = Descriptor::from_wide(&wide(OsStr::new(s))?)?;
            assert!(user.verify_descriptor(&descriptor, true).is_err());
        }
        Ok(())
    }
}
