#![forbid(unsafe_code)]
pub mod crypto;
#[cfg(test)]
mod golden_tests;
pub mod engine;
pub use engine::{hydrate_local, reembed_embedding, MemoryEngine, SessionKeys};
pub use respire_protocol as model;
