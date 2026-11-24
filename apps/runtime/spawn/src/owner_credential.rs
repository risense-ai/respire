//! A fresh, instance-scoped credential transported only to the same OS owner.
//! This secret is not an HTTP token. The caller supplies fresh CSPRNG bytes.

use std::{ffi::CString, io};

pub const OWNER_RECORD_LEN: usize = 48;

/// Raw record: 32-byte key followed by a 16-byte opaque authority epoch.
/// No Debug/Display implementation; this is not an HTTP token.
pub struct OwnerRecord([u8; OWNER_RECORD_LEN]);

impl OwnerRecord {
    pub fn from_bytes(value: [u8; OWNER_RECORD_LEN]) -> Self {
        Self(value)
    }
    pub fn as_bytes(&self) -> &[u8; OWNER_RECORD_LEN] {
        &self.0
    }
}
const SECRET_FILE: &str = "secret";

fn directory_name(instance_id: &str) -> io::Result<CString> {
    if !(1..=80).contains(&instance_id.len())
        || !instance_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid owner credential instance",
        ));
    }
    CString::new(format!(".owner-credentials-{instance_id}"))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{
    create_owner_credential, open_model_file, read_owner_credential, same_model_file, ModelStage,
    OwnerCredential,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::model_stage::{open_model_file, same_model_file, ModelStage};
#[cfg(windows)]
pub use windows::{create_owner_credential, read_owner_credential, OwnerCredential};

pub mod model_proof;
pub mod proof;
