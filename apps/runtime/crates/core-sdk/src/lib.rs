#![deny(unsafe_code)]

//! Safe ownership wrapper. No private Rust engine crate is a dependency.

pub mod env;
mod feedback_contract;
mod host;
mod backend_probe;
pub use backend_probe::backend_load_probe;
pub use host::{feedback_enabled, legacy_effective_model_config, query_settings_from_env};
mod cpu_model;
#[cfg(windows)]
#[allow(unsafe_code)]
mod host_winml;
pub mod worker;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{marker::PhantomData, rc::Rc};

mod business;
pub use business::*;
mod inference_policy;
mod model_selection;
pub mod profiles;
mod resident;
pub use resident::{
    defer_indexing, indexing_deferred, resident_scope_active, BackgroundIndexScope, ResidentLease,
    ResidentScope,
};

pub const ABI_VERSION: u32 = 0x0001_0002;
/// Capability read from the pinned, verified SDK manifest.
pub use respire_core_ffi::{CORE_VERSION, FEEDBACK_CONTRACT};
pub const CORE_SOURCE_MANIFEST_SHA256: &str = env!("RESPIRE_CORE_SOURCE_MANIFEST_SHA256");
pub const CORE_BUILD_TARGET: &str = env!("RESPIRE_CORE_BUILD_TARGET");

/// Runtime libraries from the checksum-verified, pinned SDK manifest.
pub const RUNTIME_FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/runtime_files.rs"));

#[derive(Serialize)]
struct CoreRequest<'a> {
    schema_version: u32,
    request_id: &'a str,
    operation: &'a str,
    payload: &'a Value,
}

fn response_result(response: &mut Value) -> Result<Value> {
    response
        .as_object_mut()
        .and_then(|fields| fields.remove("result"))
        .context("Core response missing result")
}

fn validate_response_identity(response: &Value, request_id: &str) -> Result<()> {
    anyhow::ensure!(
        response["schema_version"].as_u64() == Some(1)
            && response["request_id"].as_str() == Some(request_id),
        "Core response schema or request ID mismatch"
    );
    Ok(())
}

/// Confined to its owner thread; calls cannot race destruction or one another.
pub struct Core {
    handle: ffi::Handle,
    next_request: u64,
    poisoned: bool,
    native_feedback_contract: Option<u64>,
    native_read_heat_contract: Option<u64>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Core {
    pub fn new() -> Result<Self> {
        let version = ffi::abi_version();
        if version != ABI_VERSION {
            bail!("Core ABI mismatch: {version:#x}");
        }
        let mut core = Self {
            handle: ffi::Handle::create(&serde_json::to_vec(&json!({"test_mode":
                env::var("RSRS_CORE_TEST_MODE").as_deref() == Ok("1")}))?)?,
            next_request: 0,
            poisoned: false,
            native_feedback_contract: None,
            native_read_heat_contract: None,
            _thread_bound: PhantomData,
        };
        // No request payload is live while probing the linked library. Cache
        // only this handle's immutable native capability, never feedback counts.
        let capabilities = core.capabilities()?;
        core.native_feedback_contract = capabilities
            .get("feedback_contract")
            .and_then(Value::as_u64);
        core.native_read_heat_contract = capabilities
            .get("read_heat_contract")
            .and_then(Value::as_u64);
        Ok(core)
    }

    pub fn call(&mut self, operation: &str, payload: Value) -> Result<Value> {
        self.call_inner(operation, payload, None)
    }

    /// Host performs external requests. Credentials must remain in the closure.
    pub fn call_with_transport(
        &mut self,
        operation: &str,
        payload: Value,
        transport: &mut dyn FnMut(&Value) -> Result<Value>,
    ) -> Result<Value> {
        self.call_inner(operation, payload, Some(transport))
    }

    fn call_inner(
        &mut self,
        operation: &str,
        payload: Value,
        transport: Option<&mut dyn FnMut(&Value) -> Result<Value>>,
    ) -> Result<Value> {
        if self.poisoned {
            bail!("Core handle is poisoned; recreate it");
        }
        if let Some(read_heat) = payload
            .get("query_settings")
            .and_then(|settings| settings.get("read_heat"))
        {
            anyhow::ensure!(
                read_heat.is_boolean(),
                "query_settings.read_heat must be true or false"
            );
            anyhow::ensure!(self.native_read_heat_contract == Some(1),
                "explicit read_heat requires Core SDK read_heat_contract 1; install the paired SDK/CLI version");
        }
        if feedback_contract::validate(&payload, Some(FEEDBACK_CONTRACT))? {
            feedback_contract::require(self.native_feedback_contract)?;
        }
        self.next_request = self
            .next_request
            .checked_add(1)
            .context("request counter exhausted")?;
        let request_id = self.next_request.to_string();
        let request = serde_json::to_vec(&CoreRequest {
            schema_version: 1,
            request_id: &request_id,
            operation,
            payload: &payload,
        })?;
        drop(payload);
        let (code, bytes) = match transport {
            Some(transport) => self.handle.call_with_transport(&request, transport)?,
            None => self.handle.call(&request)?,
        };
        if code == 7 {
            self.poisoned = true;
        }
        let mut response: Value =
            serde_json::from_slice(&bytes).context("invalid Core response JSON")?;
        if code != 0 {
            bail!("Core error {code}: {}", response["error"]["message"]);
        }
        validate_response_identity(&response, &request_id)?;
        response_result(&mut response)
    }

    pub fn capabilities(&mut self) -> Result<Value> {
        self.call("capabilities", Value::Null)
    }

    /// Borrow model buffers for this synchronous call. Core retains its own session.
    pub fn load_model(&mut self, config: Value, model: &[u8], tokenizer: &[u8]) -> Result<Value> {
        anyhow::ensure!(!self.poisoned, "Core handle is poisoned; recreate it");
        let (code, bytes) =
            self.handle
                .load_model(&serde_json::to_vec(&config)?, model, tokenizer)?;
        if code == 7 {
            self.poisoned = true;
        }
        let mut response: Value =
            serde_json::from_slice(&bytes).context("invalid Core model response")?;
        anyhow::ensure!(
            code == 0,
            "Core error {code}: {}",
            response["error"]["message"]
        );
        response_result(&mut response)
    }

    #[cfg(windows)]
    fn register_providers(&mut self) -> Result<Vec<String>> {
        let providers = onnx::host_providers()?;
        let (code, bytes, diagnostics) = self.handle.register_providers(providers)?;
        let response: Value =
            serde_json::from_slice(&bytes).context("invalid Core provider response")?;
        anyhow::ensure!(
            code == 0,
            "Core provider registration error {code}: {}",
            response["error"]["message"]
        );
        Ok(diagnostics)
    }
}

/// Exclusive upload guard. Drop aborts a pending input; commit consumes it.
#[cfg(model_input_stream_contract)]
pub struct ModelInput<'a> {
    core: &'a mut Core,
    ticket: u64,
    offset: usize,
    pending: bool,
}

#[cfg(model_input_stream_contract)]
impl Core {
    fn model_input_response(&mut self, reply: (i32, Vec<u8>)) -> Result<Value> {
        let (code, bytes) = reply;
        if code == 7 {
            self.poisoned = true;
        }
        let mut response: Value =
            serde_json::from_slice(&bytes).context("invalid model input response")?;
        anyhow::ensure!(
            code == 0,
            "Core error {code}: {}",
            response["error"]["message"]
        );
        // Stream contract 1 uses the ABI ticket and offset for correlation;
        // these synchronous extension calls have an empty JSON request ID.
        validate_response_identity(&response, "")?;
        response_result(&mut response)
    }

    pub fn begin_model_input(&mut self, bytes: usize) -> Result<ModelInput<'_>> {
        anyhow::ensure!(!self.poisoned, "Core handle is poisoned; recreate it");
        let (ticket, code, bytes) = self.handle.input_begin(bytes);
        let guard = ModelInput {
            core: self,
            ticket,
            offset: 0,
            pending: ticket != 0,
        };
        if code == 7 {
            guard.core.poisoned = true;
        }
        guard.core.model_input_response((code, bytes?))?;
        anyhow::ensure!(ticket != 0, "Core returned a zero model input ticket");
        Ok(guard)
    }
}

#[cfg(model_input_stream_contract)]
impl ModelInput<'_> {
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        anyhow::ensure!(!self.core.poisoned, "Core handle is poisoned; recreate it");
        let expected = self
            .offset
            .checked_add(bytes.len())
            .context("model input length overflow")?;
        let reply = self
            .core
            .handle
            .input_append(self.ticket, self.offset, bytes)?;
        let response = self.core.model_input_response(reply)?;
        anyhow::ensure!(
            response["received"].as_u64() == Some(expected as u64),
            "Core model input offset mismatch"
        );
        self.offset = expected;
        Ok(())
    }
    pub fn commit(mut self, config: Value, tokenizer: &[u8]) -> Result<Value> {
        anyhow::ensure!(!self.core.poisoned, "Core handle is poisoned; recreate it");
        let config = serde_json::to_vec(&config)?;
        // This safe wrapper supplies valid preflight pointers, so commit consumes
        // the matching ticket even when configuration/native loading fails.
        self.pending = false;
        let reply = self
            .core
            .handle
            .input_commit(self.ticket, &config, tokenizer)?;
        self.core.model_input_response(reply)
    }
}

#[cfg(model_input_stream_contract)]
impl Drop for ModelInput<'_> {
    fn drop(&mut self) {
        if self.pending && !self.core.poisoned {
            if let Ok(reply) = self.core.handle.input_abort(self.ticket) {
                let _ = self.core.model_input_response(reply);
            }
        }
    }
}

#[cfg(test)]
mod core_wire_tests;

#[allow(unsafe_code)]
mod ffi;

#[cfg(all(test, model_input_stream_contract))]
mod model_input_guard_tests;

#[cfg(test)]
mod inference_thread_contract_tests;

#[cfg(test)]
mod related_contract_regressions;
