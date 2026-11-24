//! Retired Client2 launcher entry points.

use anyhow::{Context, Result};
use serde_json::json;
use std::io::Write;

pub(crate) fn startup_expectation(arguments: impl Iterator<Item = std::ffi::OsString>)
    -> Result<respire_host_control::lifecycle_io::AccountRuntimeExpectation> {
    use respire_host_control::lifecycle_io::AccountRuntimeExpectation;
    let arguments: Vec<_> = arguments.collect();
    match arguments.as_slice() {
        [] => Ok(AccountRuntimeExpectation::Startup),
        [flag] if flag == "--require-unlocked" => Ok(AccountRuntimeExpectation::Unlock),
        [flag] if flag == "--locked-after-full-logout" => Ok(AccountRuntimeExpectation::LockedAfterFullLogout),
        _ => anyhow::bail!("unsupported compatibility startup credential expectation"),
    }
}

pub(crate) fn reject_startup(arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<()> {
    let message = match startup_expectation(arguments) {
        Ok(_) => "Client2 launcher must connect to the independently running Runtime",
        Err(_) => "unsupported Client2 compatibility startup credential expectation",
    };
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &json!({"startup_complete":false, "error":message}))?;
    output.write_all(b"\n").context("cannot report retired compatibility startup")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod legacy_tests;
