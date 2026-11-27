#![forbid(unsafe_code)]

//! Runtime application services and account transactions.

pub use respire_core_sdk::env;
pub use respire_core_sdk as core_sdk;

#[cfg(test)]
mod test_lock;

pub mod auth;
pub mod core_config;
pub mod credential_io;
pub mod diary;
pub mod embedding;
pub mod feedback;
pub mod input_error;
pub mod keystore;
pub mod lock;
pub mod login_transaction;
pub mod metadata_gate;
pub mod migration;
pub mod read_heat;
pub mod recall_fusion;
pub mod resident;
pub mod review;
pub mod review_approval;
pub mod service;
pub mod session_import;
pub mod share;
pub mod snapshot;
pub mod sync;
pub mod taxonomy;
pub mod transport;

pub mod memory {
    pub use respire_core_sdk::{bge, defrag, onnx, search};
    pub use respire_crypto::{crypto, engine};
    pub use respire_crypto::{hydrate_local, reembed_embedding, MemoryEngine, SessionKeys};
    pub use respire_protocol as model;
    pub use respire_protocol::{Kind, MemoryEntry, MemoryQuery, PayloadV2, StoredMemory, VERSION_TAG};
}

pub use memory::{hydrate_local, MemoryEngine, SessionKeys};
pub use memory::model::{Kind, MemoryEntry, MemoryQuery, StoredMemory, VERSION_TAG};
pub use transport::MemoryTransport;

/// Compatibility version exported for server health responses.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
