#![forbid(unsafe_code)]

//! Private Respire algorithms, model inference and retrieval.
//!
//! Out of scope: auth, sync, remote HTTP, Tauri. Wire types come from `respire_protocol`.

mod i18n;
mod candidate;
pub mod bge;
pub mod embedding;
#[cfg(all(feature = "mlx", target_os = "macos", target_arch = "aarch64"))]
pub mod mlx;
pub mod chunks;
pub mod defrag;
pub mod engine;
pub mod onnx;
mod inference_queue;
pub mod rank;
pub mod query;
pub mod search;
pub mod metadata;
pub mod representation;
pub mod model {
    pub use crate::candidate::StoredMemory;
    pub use respire_protocol::*;
}

pub use candidate::StoredMemory;
pub use respire_protocol::{Kind, MemoryEntry, MemoryQuery, VERSION_TAG};
