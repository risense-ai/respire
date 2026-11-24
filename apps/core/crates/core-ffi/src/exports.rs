//! C ABI exports and buffer/host transport ownership.

use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{ptr, slice};

#[cfg(test)]
mod deepen_abi_tests {
    use super::*;
    include!("deepen_abi_tests.rs");
}

type HostRequest = unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize, *mut Buffer) -> i32;
type HostRelease = unsafe extern "C" fn(*mut std::ffi::c_void, *mut Buffer);

struct HostTransport {
    context: *mut std::ffi::c_void,
    request: HostRequest,
    release: HostRelease,
}

impl Transport for HostTransport {
    fn request(&mut self, request: &Value) -> anyhow::Result<Value> {
        let bytes = serde_json::to_vec(request)?;
        let mut output = Buffer::empty();
        let status = unsafe { (self.request)(self.context, bytes.as_ptr(), bytes.len(), &mut output) };
        // Host owns this buffer. Copy before releasing, including error responses.
        let copied = unsafe { input(output.data, output.len) }
            .map(<[u8]>::to_vec)
            .map_err(|_| anyhow::anyhow!("invalid host transport response buffer"));
        unsafe { (self.release)(self.context, &mut output) };
        let copied = copied?;
        if status != OK {
            let error: Value = serde_json::from_slice(&copied).unwrap_or_default();
            anyhow::bail!("host model transport failed (status {status}): {}",
                error["error"].as_str().unwrap_or("host callback did not provide an error"));
        }
        serde_json::from_slice(&copied).map_err(Into::into)
    }
}

/// Must match rs_buffer in the public C header.
#[repr(C)]
pub struct Buffer {
    data: *mut u8,
    len: usize,
}

impl Buffer {
    fn empty() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
        }
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        let bytes = bytes.into_boxed_slice();
        let len = bytes.len();
        Self {
            data: Box::into_raw(bytes).cast::<u8>(),
            len,
        }
    }
}

/// Caller guarantees valid readable memory for nonempty input.
unsafe fn input<'a>(data: *const u8, len: usize) -> Result<&'a [u8], Failure> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() || len > isize::MAX as usize {
        return Err(Failure::invalid("invalid input pointer or length"));
    }
    Ok(unsafe { slice::from_raw_parts(data, len) })
}

#[no_mangle]
pub extern "C" fn rs_core_abi_version() -> u32 {
    ABI_VERSION
}

/// Output slots must be valid, writable, and initialized empty.
#[no_mangle]
pub unsafe extern "C" fn rs_core_create(
    config: *const u8,
    config_len: usize,
    out_core: *mut *mut Core,
    out_error: *mut Buffer,
) -> i32 {
    if out_core.is_null() || out_error.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        ptr::write(out_core, ptr::null_mut());
        ptr::write(out_error, Buffer::empty());
    }
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Box<Core>, Failure> {
        let bytes = unsafe { input(config, config_len) }?;
        if !bytes.is_empty() {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Config { #[serde(default)] test_mode: bool }
            let config = serde_json::from_slice::<Config>(bytes)
                .map_err(|error| Failure::invalid(error.to_string()))?;
            return Ok(Box::new(Core { poisoned: false, pending_input: None, business: business::State::new(config.test_mode) }));
        }
        Ok(Box::new(Core::default()))
    }));
    let failure = match result {
        Ok(Ok(core)) => {
            unsafe {
                ptr::write(out_core, Box::into_raw(core));
            }
            return OK;
        }
        Ok(Err(error)) => error,
        Err(_) => Failure {
            code: PANIC,
            message: "Core creation panicked".into(),
        },
    };
    let (code, bytes) = response("", Err(failure));
    unsafe {
        ptr::write(out_error, Buffer::from_bytes(bytes));
    }
    code
}

/// Handle must be live, exclusively borrowed, and never concurrently destroyed.
#[no_mangle]
pub unsafe extern "C" fn rs_core_call(
    core: *mut Core,
    request: *const u8,
    request_len: usize,
    out_response: *mut Buffer,
) -> i32 {
    if out_response.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        ptr::write(out_response, Buffer::empty());
    }
    if core.is_null() {
        return INVALID_ARGUMENT;
    }
    let core = unsafe { &mut *core };
    let result = catch_unwind(AssertUnwindSafe(|| {
        match unsafe { input(request, request_len) } {
            Ok(bytes) => core.call(bytes),
            Err(error) => response("", Err(error)),
        }
    }));
    let (code, bytes) = match result {
        Ok(result) => result,
        Err(_) => {
            core.poisoned = true;
            response(
                "",
                Err(Failure {
                    code: PANIC,
                    message: "Core call panicked".into(),
                }),
            )
        }
    };
    unsafe {
        ptr::write(out_response, Buffer::from_bytes(bytes));
    }
    code
}

/// Model bytes are borrowed during this call and retained in Core-owned memory.
/// Empty byte buffers are valid for cached reuse and host-transport profiles.
#[no_mangle]
pub unsafe extern "C" fn rs_core_model_load(
    core: *mut Core,
    config: *const u8,
    config_len: usize,
    model: *const u8,
    model_len: usize,
    tokenizer: *const u8,
    tokenizer_len: usize,
    out_response: *mut Buffer,
) -> i32 {
    unsafe { rs_core_model_load_with_host(core, config, config_len, model, model_len, tokenizer, tokenizer_len, ptr::null_mut(), None, out_response) }
}

/// The host may configure native options during model loading. The callback,
/// context, API and option pointers are borrowed and never retained.
#[no_mangle]
pub unsafe extern "C" fn rs_core_model_load_with_host(
    core: *mut Core,
    config: *const u8,
    config_len: usize,
    model: *const u8,
    model_len: usize,
    tokenizer: *const u8,
    tokenizer_len: usize,
    context: *mut std::ffi::c_void,
    configure: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> i32>,
    out_response: *mut Buffer,
) -> i32 {
    if out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    if core.is_null() { return INVALID_ARGUMENT; }
    let core = unsafe { &mut *core };
    if core.poisoned {
        let (code, bytes) = response("", Err(Failure { code: PANIC, message: "Core handle is poisoned; destroy and recreate it".into() }));
        unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
        return code;
    }
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Value, Failure> {
        let config: Value = serde_json::from_slice(unsafe { input(config, config_len) }?)
            .map_err(|error| Failure::invalid(error.to_string()))?;
        let model = unsafe { input(model, model_len) }?;
        let tokenizer = unsafe { input(tokenizer, tokenizer_len) }?;
        if let Some(configure) = configure {
            core.business.load_with_options(&config, model, tokenizer, &|options, environment, device| {
                respire_core::onnx::with_native_environment(|_, api| {
                    let status = unsafe { configure(context, options, api, environment, device) };
                    anyhow::ensure!(status == OK, "host session options failed (status {status})");
                    Ok(())
                })
            }).map_err(Failure::engine)
        } else {
            core.business.load(&config, model, tokenizer).map_err(Failure::engine)
        }
    }));
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            core.poisoned = true;
            Err(Failure { code: PANIC, message: "Core model loading panicked".into() })
        }
    };
    let (code, bytes) = response("", result);
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}

mod input_stream;

#[cfg(test)]
mod model_input_tests;

/// Host registration owns provider library loading; Core exposes no paths.
#[no_mangle]
pub unsafe extern "C" fn rs_core_register_providers(
    core: *mut Core,
    context: *mut std::ffi::c_void,
    register: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> i32>,
    out_response: *mut Buffer,
) -> i32 {
    if out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    let Some(register) = register else { return INVALID_ARGUMENT; };
    if core.is_null() { return INVALID_ARGUMENT; }
    let core = unsafe { &mut *core };
    if core.poisoned {
        let (code, bytes) = response("", Err(Failure { code: PANIC, message: "Core handle is poisoned; destroy and recreate it".into() }));
        unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
        return code;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        respire_core::onnx::with_native_environment(|environment, api| {
            let status = unsafe { register(context, environment, api) };
            anyhow::ensure!(status == OK, "host provider registration failed (status {status})");
            Ok(Value::Null)
        }).map_err(Failure::engine)
    }));
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            core.poisoned = true;
            Err(Failure { code: PANIC, message: "Core provider registration panicked".into() })
        }
    };
    let (code, bytes) = response("", result);
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}

/// Callbacks are borrowed synchronously and must not unwind or reenter this handle.
#[no_mangle]
pub unsafe extern "C" fn rs_core_call_with_transport(
    core: *mut Core,
    request: *const u8,
    request_len: usize,
    context: *mut std::ffi::c_void,
    host_request: Option<HostRequest>,
    host_release: Option<HostRelease>,
    out_response: *mut Buffer,
) -> i32 {
    if out_response.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    let (Some(host_request), Some(host_release)) = (host_request, host_release) else {
        return INVALID_ARGUMENT;
    };
    if core.is_null() {
        return INVALID_ARGUMENT;
    }
    let core = unsafe { &mut *core };
    let mut transport = HostTransport { context, request: host_request, release: host_release };
    let result = catch_unwind(AssertUnwindSafe(|| {
        match unsafe { input(request, request_len) } {
            Ok(bytes) => core.call_with_transport(bytes, &mut transport),
            Err(error) => response("", Err(error)),
        }
    }));
    let (code, bytes) = match result {
        Ok(result) => result,
        Err(_) => {
            core.poisoned = true;
            response("", Err(Failure { code: PANIC, message: "Core call panicked".into() }))
        }
    };
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}

/// Only accepts a buffer returned by this Core; clears the original slot.
#[no_mangle]
pub unsafe extern "C" fn rs_core_buffer_free(buffer: *mut Buffer) {
    if buffer.is_null() {
        return;
    }
    let owned = unsafe { ptr::replace(buffer, Buffer::empty()) };
    if !owned.data.is_null() {
        unsafe {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
                owned.data, owned.len,
            )));
        }
    }
}

/// Only accepts a live handle once, or null. No calls may be in flight.
#[no_mangle]
pub unsafe extern "C" fn rs_core_destroy(core: *mut Core) {
    if !core.is_null() {
        unsafe {
            drop(Box::from_raw(core));
        }
    }
}
