use super::*;

    pub(super) struct SessionOptionsContext {
        pub(super) profiling: Option<std::path::PathBuf>,
        pub(super) device_options: bool,
        pub(super) native: Option<crate::profiles::NativeProfile>,
        pub(super) cache: std::path::PathBuf,
        pub(super) error: Option<anyhow::Error>,
    }

    pub(crate) struct ProbeOptions(SessionOptionsContext);

    impl ProbeOptions {
        pub(crate) fn new(native: Option<crate::profiles::NativeProfile>) -> Self {
            Self(SessionOptionsContext {
                profiling: crate::env::var_os("RSRS_ORT_PROFILE").map(std::path::PathBuf::from),
                device_options: cfg!(windows),
                native,
                cache: crate::onnx::host_cache_dir(),
                error: None,
            })
        }

        pub(crate) fn configure(&mut self, options: usize, api: usize, environment: usize, device: usize) -> Result<Option<String>> {
            anyhow::ensure!(options != 0 && api != 0 && environment != 0, "invalid borrowed probe session handles");
            let provider = if let Some(native) = &self.0.native {
                Some(native.provider.clone())
            } else if device != 0 {
                // SAFETY: the paired Core lends these handles only during this synchronous callback.
                let api = unsafe { &*(api as *const ort_sys::OrtApi) };
                let name = unsafe { (api.EpDevice_EpName)(device as *const ort_sys::OrtEpDevice) };
                anyhow::ensure!(!name.is_null(), "probe selected device has no provider name");
                Some(unsafe { std::ffi::CStr::from_ptr(name) }.to_str()?.to_owned())
            } else { None };
            // Reuse the production callback, including its native options, cache and profiling policy.
            let status = unsafe { configure_session_options(ptr::from_mut(&mut self.0).cast(), options as *mut c_void,
                api as *const c_void, environment as *mut c_void, device as *const c_void) };
            if let Some(error) = self.0.error.take() { return Err(error); }
            anyhow::ensure!(status == 0, "probe native session configuration failed");
            Ok(provider)
        }
    }

    #[cfg(windows)]
    pub(crate) fn probe_register(environment: usize, api: usize, providers: Vec<crate::host_winml::Provider>) -> Result<()> {
        anyhow::ensure!(environment != 0 && api != 0, "invalid borrowed probe registration handles");
        let mut context = ProviderContext { providers, error: None, diagnostics: Vec::new() };
        // SAFETY: registration is synchronous and uses the same callback as normal inference.
        let status = unsafe { register_host_providers(ptr::from_mut(&mut context).cast(), environment as *mut c_void, api as *const c_void) };
        if let Some(error) = context.error { return Err(error); }
        anyhow::ensure!(status == 0 && context.diagnostics.is_empty(), "probe provider registration failed: {}", context.diagnostics.join("; "));
        Ok(())
    }

    #[cfg(test)]
    thread_local! {
        static TEST_OPTION_FAILURE: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
    }
    #[cfg(test)]
    pub(crate) fn set_test_option_failure(at: Option<usize>) {
        TEST_OPTION_FAILURE.with(|failure| failure.set(at.map(|at| (at, 0))));
    }
    #[cfg(test)]
    pub(crate) fn test_option_failure_armed() -> bool {
        TEST_OPTION_FAILURE.with(|failure| failure.get().is_some())
    }
    #[cfg(test)]
    pub(crate) fn test_option_callback_count() -> usize {
        TEST_OPTION_FAILURE.with(|failure| failure.get().map_or(0, |(_, count)| count))
    }
    #[cfg(test)]
    fn fail_options_for_test() -> Result<()> {
        TEST_OPTION_FAILURE.with(|failure| {
            if let Some((at, count)) = failure.get() {
                failure.set(Some((at, count + 1)));
                anyhow::ensure!(
                    count + 1 != at,
                    "intentional native options callback failure"
                );
            }
            Ok(())
        })
    }

    pub(super) unsafe extern "C" fn configure_session_options(
        context: *mut c_void,
        options: *mut c_void,
        api: *const c_void,
        environment: *mut c_void,
        device: *const c_void,
    ) -> i32 {
        let context = unsafe { &mut *context.cast::<SessionOptionsContext>() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            anyhow::ensure!(
                !options.is_null() && !api.is_null(),
                "invalid Core native session options"
            );
            #[cfg(test)]
            fail_options_for_test()?;
            let api = unsafe { &*api.cast::<ort_sys::OrtApi>() };
            if let Some(native) = &context.native {
                if native.provider == "CPUExecutionProvider" {
                    anyhow::ensure!(
                        native.device_id.as_deref().is_none_or(|id| id == "0"),
                        "CPU provider accepts only device 0"
                    );
                    for (key, value) in &native.provider_options {
                        let enabled = match value.as_str() {
                            "1" | "true" => true,
                            "0" | "false" => false,
                            _ => anyhow::bail!(
                                "CPU provider option {key} requires true/false or 1/0"
                            ),
                        };
                        let status = match key.as_str() {
                            "enable_cpu_mem_arena" => unsafe {
                                if enabled {
                                    (api.EnableCpuMemArena)(options.cast())
                                } else {
                                    (api.DisableCpuMemArena)(options.cast())
                                }
                            },
                            "enable_mem_pattern" => unsafe {
                                if enabled {
                                    (api.EnableMemPattern)(options.cast())
                                } else {
                                    (api.DisableMemPattern)(options.cast())
                                }
                            },
                            _ => anyhow::bail!("unsupported CPU provider option {key}"),
                        };
                        if !status.0.is_null() {
                            let message = unsafe {
                                std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0))
                            }
                            .to_string_lossy()
                            .into_owned();
                            unsafe { (api.ReleaseStatus)(status.0) };
                            anyhow::bail!("configure custom CPU provider: {message}");
                        }
                    }
                } else {
                    let name = std::ffi::CString::new(
                        native.provider.trim_end_matches("ExecutionProvider"),
                    )?;
                    let mut entries = native.provider_options.clone();
                    if let Some(id) = &native.device_id {
                        entries
                            .entry("device_id".to_owned())
                            .or_insert_with(|| id.clone());
                    }
                    let keys = entries
                        .keys()
                        .map(|s| std::ffi::CString::new(s.as_bytes()))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let values = entries
                        .values()
                        .map(|s| std::ffi::CString::new(s.as_bytes()))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let key_ptrs = keys.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
                    let value_ptrs = values.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
                    let status = unsafe {
                        (api.SessionOptionsAppendExecutionProvider)(
                            options.cast(),
                            name.as_ptr(),
                            key_ptrs.as_ptr(),
                            value_ptrs.as_ptr(),
                            keys.len(),
                        )
                    };
                    if !status.0.is_null() {
                        let message =
                            unsafe { std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0)) }
                                .to_string_lossy()
                                .into_owned();
                        unsafe { (api.ReleaseStatus)(status.0) };
                        anyhow::bail!("configure custom host provider: {message}");
                    }
                }
            } else if !device.is_null() {
                anyhow::ensure!(!environment.is_null(), "invalid Core native environment");
                let name = unsafe { (api.EpDevice_EpName)(device.cast()) };
                anyhow::ensure!(!name.is_null(), "missing Core selected provider name");
                let openvino = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes()
                    == b"OpenVINOExecutionProvider";
                let cache_key = std::ffi::CString::new("cache_dir")?;
                if openvino {
                    std::fs::create_dir_all(&context.cache)?;
                }
                let cache_path =
                    std::ffi::CString::new(context.cache.to_string_lossy().as_bytes())?;
                let keys = [cache_key.as_ptr()];
                let values = [cache_path.as_ptr()];
                let selected = [device.cast::<ort_sys::OrtEpDevice>()];
                let status = unsafe {
                    (api.SessionOptionsAppendExecutionProvider_V2)(
                        options.cast(),
                        environment.cast(),
                        selected.as_ptr(),
                        1,
                        if openvino { keys.as_ptr() } else { ptr::null() },
                        if openvino {
                            values.as_ptr()
                        } else {
                            ptr::null()
                        },
                        usize::from(openvino),
                    )
                };
                if !status.0.is_null() {
                    let message =
                        unsafe { std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0)) }
                            .to_string_lossy()
                            .into_owned();
                    unsafe { (api.ReleaseStatus)(status.0) };
                    anyhow::bail!("configure host selected provider: {message}");
                }
            }
            if let Some(path) = &context.profiling {
                #[cfg(windows)]
                let encoded: Vec<u16> = {
                    use std::os::windows::ffi::OsStrExt;
                    path.as_os_str().encode_wide().chain(Some(0)).collect()
                };
                #[cfg(not(windows))]
                let encoded = {
                    use std::os::unix::ffi::OsStrExt;
                    std::ffi::CString::new(path.as_os_str().as_bytes())?
                };
                let status = unsafe { (api.EnableProfiling)(options.cast(), encoded.as_ptr()) };
                if !status.0.is_null() {
                    let message =
                        unsafe { std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0)) }
                            .to_string_lossy()
                            .into_owned();
                    unsafe { (api.ReleaseStatus)(status.0) };
                    anyhow::bail!("configure host profiling: {message}");
                }
            }
            Ok(())
        }));
        match result {
            Ok(Ok(())) => 0,
            Ok(Err(error)) => {
                context.error = Some(error);
                6
            }
            Err(_) => {
                context.error = Some(anyhow::anyhow!("host session configuration panicked"));
                7
            }
        }
    }

    #[cfg(windows)]
    pub(super) struct ProviderContext {
        pub(super) providers: Vec<crate::host_winml::Provider>,
        pub(super) error: Option<anyhow::Error>,
        pub(super) diagnostics: Vec<String>,
    }

    #[cfg(windows)]
    pub(super) unsafe extern "C" fn register_host_providers(
        context: *mut c_void,
        environment: *mut c_void,
        api: *const c_void,
    ) -> i32 {
        let context = unsafe { &mut *context.cast::<ProviderContext>() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            anyhow::ensure!(
                !environment.is_null() && !api.is_null(),
                "invalid Core native environment"
            );
            let api = unsafe { &*api.cast::<ort_sys::OrtApi>() };
            static REGISTERED: std::sync::Mutex<std::collections::BTreeMap<String, std::path::PathBuf>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
            let mut registered = REGISTERED
                .lock()
                .map_err(|_| anyhow::anyhow!("host provider registry poisoned"))?;
            for provider in &context.providers {
                if !provider.ready {
                    continue;
                }
                let Some(path) = &provider.path else {
                    continue;
                };
                let canonical_path = match std::fs::canonicalize(path) {
                    Ok(path) => path,
                    Err(error) => {
                        context.diagnostics.push(format!("resolve {}: {error}", provider.name));
                        continue;
                    }
                };
                if let Some(existing) = registered.get(&provider.name) {
                    if existing != &canonical_path {
                        context.diagnostics.push(format!("{} is already registered from another library; host restart required", provider.name));
                    }
                    continue;
                }
                use std::os::windows::ffi::OsStrExt;
                let path: Vec<u16> = canonical_path.as_os_str().encode_wide().chain(Some(0)).collect();
                let name = std::ffi::CString::new(provider.name.as_str())?;
                let status = unsafe {
                    (api.RegisterExecutionProviderLibrary)(
                        environment.cast(),
                        name.as_ptr(),
                        path.as_ptr(),
                    )
                };
                if !status.0.is_null() {
                    let message =
                        unsafe { std::ffi::CStr::from_ptr((api.GetErrorMessage)(status.0)) }
                            .to_string_lossy()
                            .into_owned();
                    unsafe { (api.ReleaseStatus)(status.0) };
                    context
                        .diagnostics
                        .push(format!("register {}: {message}", provider.name));
                    continue;
                }
                registered.insert(provider.name.clone(), canonical_path);
            }
            Ok(())
        }));
        match result {
            Ok(Ok(())) => 0,
            Ok(Err(error)) => {
                context.error = Some(error);
                6
            }
            Err(_) => {
                context.error = Some(anyhow::anyhow!("host provider registration panicked"));
                7
            }
        }
    }
