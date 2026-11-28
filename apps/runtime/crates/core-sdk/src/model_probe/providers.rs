use super::*;
use ort::session::builder::SessionBuilder;

pub(super) struct Options {
    pub builder: SessionBuilder,
    pub provider: String,
    pub device: String,
}

pub(super) fn configure(
    engine: Engine,
    _window: i64,
    _diagnostics: &mut Vec<String>,
) -> Result<Options> {
    let builder = build(Session::builder()?.with_no_environment_execution_providers())?;
    let builder = build(builder.with_intra_threads(1))?;
    let builder = build(builder.with_inter_threads(1))?;
    let builder = build(builder.with_parallel_execution(false))?;
    let (builder, provider, device) = match engine {
        Engine::Cpu => (
            build(
                builder
                    .with_execution_providers([ort::ep::CPU::default().build().error_on_failure()]),
            )?,
            "CPUExecutionProvider".to_owned(),
            "CPU".to_owned(),
        ),
        #[cfg(windows)]
        Engine::Gpu => {
            let builder = build(builder.with_memory_pattern(false))?;
            let builder = build(builder.with_execution_providers([
                ort::ep::DirectML::default().build().error_on_failure(),
            ]))?;
            (
                builder,
                "DmlExecutionProvider".to_owned(),
                "DirectML adapter 0".to_owned(),
            )
        }
        #[cfg(target_os = "macos")]
        Engine::Gpu | Engine::Npu => {
            use ort::ep::coreml::{ComputeUnits, ModelFormat};
            let (units, device) = if engine == Engine::Npu {
                (ComputeUnits::CPUAndNeuralEngine, "Apple Neural Engine")
            } else {
                (ComputeUnits::CPUAndGPU, "Apple GPU")
            };
            let builder = build(
                builder.with_execution_providers([ort::ep::CoreML::default()
                    .with_compute_units(units)
                    .with_model_format(ModelFormat::MLProgram)
                    .build()
                    .error_on_failure()]),
            )?;
            (
                builder,
                "CoreMLExecutionProvider".to_owned(),
                device.to_owned(),
            )
        }
        #[cfg(target_os = "linux")]
        Engine::Gpu | Engine::Npu => {
            let mut builder = builder;
            let (provider, device) = crate::linux_ep::configure_builder(&mut builder, engine)?;
            (builder, provider, device)
        }
        #[cfg(windows)]
        Engine::Npu => {
            use ort::{environment::Environment, memory::DeviceType};
            let environment = Environment::current()?;
            let find = || {
                environment.devices().find(|device| {
                    device.hardware_device().ty() == DeviceType::NPU
                        && device.ep().is_ok_and(|name| !name.ends_with(".AUTO"))
                })
            };
            if find().is_none() {
                for provider in crate::onnx::host_providers()?
                    .into_iter()
                    .filter(|provider| provider.ready)
                {
                    if let Some(path) = provider.path {
                        if let Err(error) = environment.register_ep_library(&provider.name, path) {
                            _diagnostics.push(format!("Register {}: {error}", provider.name));
                        }
                    }
                }
            }
            let device = find()
                .with_context(|| format!("no installed ONNX execution provider for {engine:?}"))?;
            let provider = device.ep()?.to_owned();
            let hardware = device.hardware_device();
            let label = format!("NPU {} ({})", hardware.vendor()?, hardware.id());
            let builder = build(builder.with_devices([device], None))?;
            let builder = build(builder.with_disable_cpu_fallback())?;
            let builder = build(builder.with_memory_pattern(false))?;
            let builder = build(builder.with_dimension_override("batch_size", 1))?;
            let builder = build(builder.with_dimension_override("sequence_length", _window))?;
            (builder, provider, label)
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        _ => anyhow::bail!("{engine:?} is unsupported on this operating system"),
    };
    Ok(Options {
        builder,
        provider,
        device,
    })
}

fn build(
    result: std::result::Result<SessionBuilder, ort::Error<SessionBuilder>>,
) -> Result<SessionBuilder> {
    result.map_err(|error| anyhow!("{error}"))
}
