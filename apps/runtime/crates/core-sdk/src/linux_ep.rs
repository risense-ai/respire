use crate::onnx::Engine;
use anyhow::{bail, Context, Result};
use ort::{environment::Environment, memory::DeviceType, AsPointer};
use ort_sys::{OrtApi, OrtEnv, OrtSessionOptions, OrtStatusPtr};
use std::{ffi::CStr, ptr};

fn check(api: &OrtApi, status: OrtStatusPtr) -> Result<()> {
    if !status.0.is_null() {
        let message = unsafe { CStr::from_ptr((api.GetErrorMessage)(status.0)) }
            .to_string_lossy()
            .into_owned();
        unsafe { (api.ReleaseStatus)(status.0) };
        bail!("configure Linux execution provider: {message}");
    }
    Ok(())
}

fn linked_providers(api: &OrtApi) -> Result<Vec<String>> {
    let mut names = ptr::null_mut();
    let mut count = 0;
    check(api, unsafe {
        (api.GetAvailableProviders)(&mut names, &mut count)
    })?;
    let providers = (0..count)
        .map(|index| {
            unsafe { CStr::from_ptr(*names.add(index as usize)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    check(api, unsafe {
        (api.ReleaseAvailableProviders)(names, count)
    })?;
    Ok(providers)
}

pub(crate) fn configure_builder(
    builder: &mut ort::session::builder::SessionBuilder,
    engine: Engine,
) -> Result<(String, String)> {
    let environment = Environment::current()?;
    let options = builder.ptr_mut();
    unsafe { configure(options, ort::api(), environment.ptr().cast_mut(), engine) }
}

pub(crate) unsafe fn configure(
    options: *mut OrtSessionOptions,
    api: &OrtApi,
    environment: *mut OrtEnv,
    engine: Engine,
) -> Result<(String, String)> {
    let device_type = if engine == Engine::Npu {
        DeviceType::NPU
    } else {
        DeviceType::GPU
    };
    let env = Environment::current()?;
    anyhow::ensure!(
        env.ptr() == environment.cast_const(),
        "Linux execution provider environment mismatch"
    );
    if engine == Engine::Npu {
        check(api, unsafe {
            (api.AddSessionConfigEntry)(
                options,
                c"session.disable_cpu_ep_fallback".as_ptr(),
                c"1".as_ptr(),
            )
        })?;
        check(api, unsafe { (api.DisableMemPattern)(options) })?;
    }
    if let Some(device) = env.devices().find(|device| {
        device.hardware_device().ty() == device_type
            && device.ep().is_ok_and(|name| !name.ends_with(".AUTO"))
    }) {
        let selected = [device.ptr()];
        check(api, unsafe {
            (api.SessionOptionsAppendExecutionProvider_V2)(
                options,
                environment,
                selected.as_ptr(),
                1,
                ptr::null(),
                ptr::null(),
                0,
            )
        })?;
        let hardware = device.hardware_device();
        return Ok((
            device.ep()?.to_owned(),
            format!("{device_type:?} {} ({})", hardware.vendor()?, hardware.id()),
        ));
    }
    let available = linked_providers(api)?;
    let candidates: &[&str] = if engine == Engine::Npu {
        &["OpenVINOExecutionProvider"]
    } else {
        &["CUDAExecutionProvider", "ROCMExecutionProvider"]
    };
    let provider = candidates
        .iter()
        .copied()
        .find(|name| available.iter().any(|value| value == name))
        .with_context(|| {
            format!(
                "no installed ONNX execution provider for {engine:?}; linked: {}",
                available.join(", ")
            )
        })?;
    let label = match provider {
        "CUDAExecutionProvider" => {
            let mut settings = ptr::null_mut();
            check(api, unsafe {
                (api.CreateCUDAProviderOptions)(&mut settings)
            })?;
            let status =
                unsafe { (api.SessionOptionsAppendExecutionProvider_CUDA_V2)(options, settings) };
            unsafe { (api.ReleaseCUDAProviderOptions)(settings) };
            check(api, status)?;
            "CUDA device 0"
        }
        "ROCMExecutionProvider" => {
            let settings = ort_sys::OrtROCMProviderOptions {
                device_id: 0,
                miopen_conv_exhaustive_search: 0,
                gpu_mem_limit: usize::MAX,
                arena_extend_strategy: 0,
                do_copy_in_default_stream: 1,
                has_user_compute_stream: 0,
                user_compute_stream: ptr::null_mut(),
                default_memory_arena_cfg: ptr::null_mut(),
                enable_hip_graph: 0,
                tunable_op_enable: 0,
                tunable_op_tuning_enable: 0,
                tunable_op_max_tuning_duration_ms: 0,
            };
            check(api, unsafe {
                (api.SessionOptionsAppendExecutionProvider_ROCM)(options, &settings)
            })?;
            "ROCm device 0"
        }
        _ => {
            let keys = [c"device_type".as_ptr()];
            let values = [c"NPU".as_ptr()];
            check(api, unsafe {
                (api.SessionOptionsAppendExecutionProvider_OpenVINO_V2)(
                    options,
                    keys.as_ptr(),
                    values.as_ptr(),
                    1,
                )
            })?;
            "OpenVINO NPU"
        }
    };
    Ok((provider.to_owned(), label.to_owned()))
}
