    use super::*;
    use std::{ffi::c_void, ptr, slice};

    #[repr(C)]
    struct Buffer {
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
        fn bytes(&self) -> Result<Vec<u8>> {
            if self.len == 0 {
                return Ok(Vec::new());
            }
            if self.data.is_null() || self.len > isize::MAX as usize {
                bail!("invalid Core output buffer");
            }
            // The ABI guarantees readable bytes until the matching Core free.
            Ok(unsafe { slice::from_raw_parts(self.data, self.len) }.to_vec())
        }
    }

    impl Drop for Buffer {
        fn drop(&mut self) {
            // Only Core-created buffers enter this wrapper; free also clears it.
            unsafe {
                rs_core_buffer_free(self);
            }
        }
    }

    extern "C" {
        fn rs_core_create(
            config: *const u8,
            len: usize,
            core: *mut *mut c_void,
            error: *mut Buffer,
        ) -> i32;
        fn rs_core_call(
            core: *mut c_void,
            input: *const u8,
            len: usize,
            output: *mut Buffer,
        ) -> i32;
        fn rs_core_buffer_free(buffer: *mut Buffer);
        fn rs_core_model_load(
            core: *mut c_void,
            config: *const u8,
            config_len: usize,
            model: *const u8,
            model_len: usize,
            tokenizer: *const u8,
            tokenizer_len: usize,
            output: *mut Buffer,
        ) -> i32;
        fn rs_core_model_load_with_host(
            core: *mut c_void,
            config: *const u8,
            config_len: usize,
            model: *const u8,
            model_len: usize,
            tokenizer: *const u8,
            tokenizer_len: usize,
            context: *mut c_void,
            configure: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const c_void,
                *mut c_void,
                *const c_void,
            ) -> i32,
            output: *mut Buffer,
        ) -> i32;
        #[cfg(windows)]
        fn rs_core_register_providers(
            core: *mut c_void,
            context: *mut c_void,
            configure: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> i32,
            output: *mut Buffer,
        ) -> i32;
        fn rs_core_call_with_transport(
            core: *mut c_void,
            input: *const u8,
            len: usize,
            context: *mut c_void,
            request: unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut HostBuffer) -> i32,
            release: unsafe extern "C" fn(*mut c_void, *mut HostBuffer),
            output: *mut Buffer,
        ) -> i32;
        fn rs_core_destroy(core: *mut c_void);
    }

    #[cfg(model_input_stream_contract)]
    extern "C" {
        fn rs_core_model_input_begin(
            core: *mut c_void,
            bytes: usize,
            ticket: *mut u64,
            out: *mut Buffer,
        ) -> i32;
        fn rs_core_model_input_append(
            core: *mut c_void,
            ticket: u64,
            offset: usize,
            bytes: *const u8,
            len: usize,
            out: *mut Buffer,
        ) -> i32;
        fn rs_core_model_input_commit(
            core: *mut c_void,
            ticket: u64,
            config: *const u8,
            config_len: usize,
            tokenizer: *const u8,
            tokenizer_len: usize,
            out: *mut Buffer,
        ) -> i32;
        fn rs_core_model_input_abort(core: *mut c_void, ticket: u64, out: *mut Buffer) -> i32;
    }

    #[cfg(model_input_stream_contract)]
    impl Handle {
        pub(super) fn input_begin(&mut self, bytes: usize) -> (u64, i32, Result<Vec<u8>>) {
            let mut ticket = 0;
            let mut output = Buffer::empty();
            let code = unsafe {
                rs_core_model_input_begin(self.pointer.as_ptr(), bytes, &mut ticket, &mut output)
            };
            (ticket, code, output.bytes())
        }
        pub(super) fn input_append(
            &mut self,
            ticket: u64,
            offset: usize,
            bytes: &[u8],
        ) -> Result<(i32, Vec<u8>)> {
            let mut output = Buffer::empty();
            let code = unsafe {
                rs_core_model_input_append(
                    self.pointer.as_ptr(),
                    ticket,
                    offset,
                    bytes.as_ptr(),
                    bytes.len(),
                    &mut output,
                )
            };
            Ok((code, output.bytes()?))
        }
        pub(super) fn input_commit(
            &mut self,
            ticket: u64,
            config: &[u8],
            tokenizer: &[u8],
        ) -> Result<(i32, Vec<u8>)> {
            let mut output = Buffer::empty();
            let code = unsafe {
                rs_core_model_input_commit(
                    self.pointer.as_ptr(),
                    ticket,
                    config.as_ptr(),
                    config.len(),
                    tokenizer.as_ptr(),
                    tokenizer.len(),
                    &mut output,
                )
            };
            Ok((code, output.bytes()?))
        }
        pub(super) fn input_abort(&mut self, ticket: u64) -> Result<(i32, Vec<u8>)> {
            let mut output = Buffer::empty();
            let code =
                unsafe { rs_core_model_input_abort(self.pointer.as_ptr(), ticket, &mut output) };
            Ok((code, output.bytes()?))
        }
    }

    pub(super) fn worker_stdout() -> Result<std::fs::File> {
        // Keep the IPC pipe private before any native library can write to stdout.
        #[cfg(unix)]
        {
            use std::os::fd::AsFd;
            extern "C" {
                fn dup2(old: std::ffi::c_int, new: std::ffi::c_int) -> std::ffi::c_int;
            }
            let protocol = std::io::stdout()
                .as_fd()
                .try_clone_to_owned()
                .context("duplicate Core worker protocol output")?;
            if unsafe { dup2(2, 1) } == -1 {
                return Err(std::io::Error::last_os_error()).context("redirect Core worker stdout");
            }
            Ok(std::fs::File::from(protocol))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsHandle, AsRawHandle};
            extern "C" {
                fn _dup2(old: std::ffi::c_int, new: std::ffi::c_int) -> std::ffi::c_int;
            }
            extern "system" {
                fn SetStdHandle(kind: u32, handle: *mut std::ffi::c_void) -> i32;
            }
            let protocol = std::io::stdout()
                .as_handle()
                .try_clone_to_owned()
                .context("duplicate Core worker protocol output")?;
            if unsafe { _dup2(2, 1) } == -1 {
                bail!("redirect Core worker CRT stdout failed (_dup2)");
            }
            if unsafe { SetStdHandle((-11i32) as u32, std::io::stderr().as_raw_handle()) } == 0 {
                return Err(std::io::Error::last_os_error()).context("redirect Core worker stdout");
            }
            Ok(std::fs::File::from(protocol))
        }
    }

    #[cfg(test)]
    pub(super) fn worker_stdout_test_diagnostic() -> Result<()> {
        extern "C" {
            fn puts(text: *const std::ffi::c_char) -> std::ffi::c_int;
            fn fflush(stream: *mut std::ffi::c_void) -> std::ffi::c_int;
        }
        anyhow::ensure!(unsafe { puts(c"worker-native-diagnostic".as_ptr()) } >= 0);
        anyhow::ensure!(unsafe { fflush(std::ptr::null_mut()) } == 0);
        Ok(())
    }

    pub(super) fn abi_version() -> u32 {
        respire_core_ffi::rs_core_abi_version()
    }

    pub(super) struct Handle {
        pointer: ptr::NonNull<c_void>,
    }

    // Host allocations must never enter Buffer, whose Drop uses the Core allocator.
    #[repr(C)]
    struct HostBuffer {
        data: *mut u8,
        len: usize,
    }

    struct TransportContext<'a> {
        callback: &'a mut dyn FnMut(&Value) -> Result<Value>,
        response: Vec<u8>,
    }

    unsafe extern "C" fn host_request(
        context: *mut c_void,
        input: *const u8,
        len: usize,
        output: *mut HostBuffer,
    ) -> i32 {
        let context = unsafe { &mut *context.cast::<TransportContext<'_>>() };
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<u8>> {
                anyhow::ensure!(
                    !input.is_null() && len <= isize::MAX as usize,
                    "invalid host request buffer"
                );
                let request: Value =
                    serde_json::from_slice(unsafe { slice::from_raw_parts(input, len) })?;
                serde_json::to_vec(&(context.callback)(&request)?).map_err(Into::into)
            }));
        let (status, bytes) = match result {
            Ok(Ok(bytes)) => (0, bytes),
            Ok(Err(error)) => (
                6,
                json!({"error":error.to_string()}).to_string().into_bytes(),
            ),
            Err(_) => (7, Vec::new()),
        };
        context.response = bytes;
        unsafe {
            ptr::write(
                output,
                HostBuffer {
                    data: context.response.as_mut_ptr(),
                    len: context.response.len(),
                },
            )
        };
        status
    }

    unsafe extern "C" fn host_release(context: *mut c_void, output: *mut HostBuffer) {
        let context = unsafe { &mut *context.cast::<TransportContext<'_>>() };
        context.response.clear();
        unsafe {
            ptr::write(
                output,
                HostBuffer {
                    data: ptr::null_mut(),
                    len: 0,
                },
            )
        };
    }

    impl Handle {
        pub(super) fn create(config: &[u8]) -> Result<Self> {
            let mut pointer = ptr::null_mut();
            let mut error = Buffer::empty();
            let code =
                unsafe { rs_core_create(config.as_ptr(), config.len(), &mut pointer, &mut error) };
            if code != 0 {
                bail!(
                    "Core creation error {code}: {}",
                    String::from_utf8_lossy(&error.bytes()?)
                );
            }
            Ok(Self {
                pointer: ptr::NonNull::new(pointer).context("Core returned a null handle")?,
            })
        }

        pub(super) fn call(&mut self, input: &[u8]) -> Result<(i32, Vec<u8>)> {
            let mut output = Buffer::empty();
            // &mut self provides exclusive access; Core is not Send or Sync.
            let code = unsafe {
                rs_core_call(
                    self.pointer.as_ptr(),
                    input.as_ptr(),
                    input.len(),
                    &mut output,
                )
            };
            Ok((code, output.bytes()?))
        }

        pub(super) fn load_model(
            &mut self,
            config: &[u8],
            model: &[u8],
            tokenizer: &[u8],
        ) -> Result<(i32, Vec<u8>)> {
            let mut output = Buffer::empty();
            let config_value: Value = serde_json::from_slice(config)?;
            let device_options = cfg!(windows) && config_value["engine"].as_str() != Some("cpu");
            let mut options = SessionOptionsContext {
                profiling: crate::env::var_os("RSRS_ORT_PROFILE").map(std::path::PathBuf::from),
                device_options,
                native: if config_value["host_execution_provider"].as_bool() == Some(true) {
                    crate::profiles::current()?.and_then(|s| s.backend_profile.native)
                } else {
                    None
                },
                cache: crate::onnx::host_cache_dir(),
                error: None,
            };
            // Borrowed slices and host option callback remain alive through this call.
            let options_required = config_value["backend"].as_str() != Some("host_transport")
                && (options.profiling.is_some()
                    || options.device_options
                    || options.native.is_some());
            #[cfg(test)]
            let options_required = options_required || test_option_failure_armed();
            let code = if options_required {
                unsafe {
                    rs_core_model_load_with_host(
                        self.pointer.as_ptr(),
                        config.as_ptr(),
                        config.len(),
                        model.as_ptr(),
                        model.len(),
                        tokenizer.as_ptr(),
                        tokenizer.len(),
                        ptr::from_mut(&mut options).cast(),
                        configure_session_options,
                        &mut output,
                    )
                }
            } else {
                unsafe {
                    rs_core_model_load(
                        self.pointer.as_ptr(),
                        config.as_ptr(),
                        config.len(),
                        model.as_ptr(),
                        model.len(),
                        tokenizer.as_ptr(),
                        tokenizer.len(),
                        &mut output,
                    )
                }
            };
            if let Some(error) = options.error {
                return Err(error);
            }
            Ok((code, output.bytes()?))
        }

        #[cfg(windows)]
        pub(super) fn register_providers(
            &mut self,
            providers: Vec<crate::host_winml::Provider>,
        ) -> Result<(i32, Vec<u8>, Vec<String>)> {
            let mut context = ProviderContext {
                providers,
                error: None,
                diagnostics: Vec::new(),
            };
            let mut output = Buffer::empty();
            // The callback and provider paths live until this synchronous call returns.
            let code = unsafe {
                rs_core_register_providers(
                    self.pointer.as_ptr(),
                    ptr::from_mut(&mut context).cast(),
                    register_host_providers,
                    &mut output,
                )
            };
            if let Some(error) = context.error {
                return Err(error);
            }
            Ok((code, output.bytes()?, context.diagnostics))
        }

        pub(super) fn call_with_transport(
            &mut self,
            input: &[u8],
            callback: &mut dyn FnMut(&Value) -> Result<Value>,
        ) -> Result<(i32, Vec<u8>)> {
            let mut context = TransportContext {
                callback,
                response: Vec::new(),
            };
            let mut output = Buffer::empty();
            let code = unsafe {
                rs_core_call_with_transport(
                    self.pointer.as_ptr(),
                    input.as_ptr(),
                    input.len(),
                    ptr::from_mut(&mut context).cast(),
                    host_request,
                    host_release,
                    &mut output,
                )
            };
            Ok((code, output.bytes()?))
        }
    }

mod session_options;
use session_options::{configure_session_options, SessionOptionsContext};
#[cfg(windows)]
use session_options::{register_host_providers, ProviderContext};
pub(super) use session_options::ProbeOptions;
#[cfg(windows)]
pub(super) use session_options::probe_register;
#[cfg(test)]
pub(super) use session_options::{
    set_test_option_failure, test_option_callback_count, test_option_failure_armed,
};

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                rs_core_destroy(self.pointer.as_ptr());
            }
        }
    }
