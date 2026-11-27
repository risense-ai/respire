#![forbid(unsafe_code)]

//! Runtime host identity and low-level helpers without app or Core dependencies.
//! The application service owns account and installation root resolution.

pub mod config;
pub mod credential;
pub mod identity;
pub mod input_error;
#[cfg(feature = "locks")]
pub mod lock;
mod version;

pub use version::version_cmp;
