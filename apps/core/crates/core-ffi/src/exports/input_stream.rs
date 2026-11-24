//! Streaming model input operations for the C ABI.

use super::*;

// Extension calls retain the same exclusive live-handle/output-slot contract.
// Structural rejection leaves pending input untouched. Once a matching ticket
// is taken, commit consumes it on every outcome, including a caught panic.
fn input_operation(core: &mut Core, run: impl FnOnce(&mut Core) -> Result<Value, Failure>) -> (i32, Vec<u8>) {
    if core.poisoned { return response("", Err(Failure { code: PANIC, message: "Core handle is poisoned; destroy and recreate it".into() })); }
    let result = match catch_unwind(AssertUnwindSafe(|| run(core))) {
        Ok(result) => result,
        Err(_) => {
            core.poisoned = true;
            Err(Failure { code: PANIC, message: "Core model input operation panicked".into() })
        }
    };
    response("", result)
}

#[no_mangle]
pub unsafe extern "C" fn rs_core_model_input_begin(core: *mut Core, model_len: usize, out_ticket: *mut u64, out_response: *mut Buffer) -> i32 {
    if out_ticket.is_null() || out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_ticket, 0); ptr::write(out_response, Buffer::empty()); }
    if core.is_null() { return INVALID_ARGUMENT; }
    let (code, bytes) = input_operation(unsafe { &mut *core }, |core| {
        if core.pending_input.is_some() { return Err(Failure::invalid("a model input is already pending")); }
        let pending = model_input::Pending::begin(model_len)?;
        let ticket = pending.ticket();
        core.pending_input = Some(pending);
        unsafe { ptr::write(out_ticket, ticket) };
        Ok(json!({"ticket":ticket,"received":0}))
    });
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}

#[no_mangle]
pub unsafe extern "C" fn rs_core_model_input_append(core: *mut Core, ticket: u64, expected_offset: usize, bytes: *const u8, bytes_len: usize, out_response: *mut Buffer) -> i32 {
    if out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    if core.is_null() { return INVALID_ARGUMENT; }
    let (code, response_bytes) = input_operation(unsafe { &mut *core }, |core| {
        let pending = core.pending_input.as_mut().filter(|pending| pending.ticket() == ticket)
            .ok_or_else(|| Failure::invalid("unknown model input ticket"))?;
        let chunk = unsafe { input(bytes, bytes_len) }?;
        let received = pending.append(expected_offset, chunk)?;
        Ok(json!({"received":received}))
    });
    unsafe { ptr::write(out_response, Buffer::from_bytes(response_bytes)) };
    code
}

#[no_mangle]
pub unsafe extern "C" fn rs_core_model_input_abort(core: *mut Core, ticket: u64, out_response: *mut Buffer) -> i32 {
    if out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    if core.is_null() { return INVALID_ARGUMENT; }
    let (code, bytes) = input_operation(unsafe { &mut *core }, |core| {
        let aborted = core.pending_input.as_ref().is_some_and(|pending| pending.ticket() == ticket);
        if aborted { drop(core.pending_input.take()); }
        Ok(json!({"aborted":aborted}))
    });
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}

#[no_mangle]
pub unsafe extern "C" fn rs_core_model_input_commit(core: *mut Core, ticket: u64, config: *const u8, config_len: usize, tokenizer: *const u8, tokenizer_len: usize, out_response: *mut Buffer) -> i32 {
    if out_response.is_null() { return INVALID_ARGUMENT; }
    unsafe { ptr::write(out_response, Buffer::empty()) };
    if core.is_null() { return INVALID_ARGUMENT; }
    let (code, bytes) = input_operation(unsafe { &mut *core }, |core| {
        let pending = model_input::take(&mut core.pending_input, ticket)?;
        let config: Value = serde_json::from_slice(unsafe { input(config, config_len) }?)
            .map_err(|error| Failure::invalid(error.to_string()))?;
        let tokenizer = unsafe { input(tokenizer, tokenizer_len) }?;
        let (model, reservation) = pending.seal()?;
        #[cfg(feature = "cpu-owned-ort-experiment")]
        let result = core.business.load_owned(&config, model, tokenizer).map_err(Failure::engine);
        #[cfg(not(feature = "cpu-owned-ort-experiment"))]
        let result = { let _ = (config, model, tokenizer); Err(Failure { code: UNSUPPORTED_OPERATION, message: "CPU owned model input is unavailable in this build".into() }) };
        // Keep reservation through all native construction and failure cleanup.
        drop(reservation);
        result
    });
    unsafe { ptr::write(out_response, Buffer::from_bytes(bytes)) };
    code
}
