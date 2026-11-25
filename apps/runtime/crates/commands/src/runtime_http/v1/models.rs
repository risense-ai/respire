//! Typed projections of the serving store's current model selection.
//! Never runs inference or opens a second profile store to answer a read.
use anyhow::Result;
use respire_app::{
    core_sdk::{onnx::Engine, profiles::Selection},
    embedding::{self, Registry},
    transport::local::LocalStore,
};
use serde::Serialize;

use super::{transport::unavailable, V1Result};

pub(super) fn active_assets(store: &LocalStore) -> V1Result<serde_json::Value> {
    if !crate::rpc_serve::model_assets_stable() {
        return Err(unavailable());
    }
    // A legacy profile dynamically resolves variants at load time.
    // Its configured paths cannot identify what the worker is still holding.
    let selection = store
        .active_selection()
        .map_err(|_| unavailable())?
        .filter(|selection| !selection.generation.is_empty())
        .ok_or_else(unavailable)?;
    selection.validate().map_err(|_| unavailable())?;
    let mut paths = Vec::new();
    if matches!(selection.backend_profile.backend.as_str(), "onnx" | "mlx") {
        for resource in [selection.profile.weights, selection.profile.tokenizer] {
            paths.push(resource.ok_or_else(unavailable)?.path);
        }
    }
    let mut active_paths = Vec::new();
    for path in paths {
        let path = path.canonicalize().map_err(|_| unavailable())?;
        if !path.is_file() {
            return Err(unavailable());
        }
        let path = path.to_str().ok_or_else(unavailable)?.to_owned();
        if !active_paths.contains(&path) {
            active_paths.push(path);
        }
    }
    Ok(serde_json::json!({
        "active_model_id": selection.model,
        "locked": !active_paths.is_empty(),
        "active_paths": active_paths,
    }))
}

#[derive(Serialize)]
pub(super) struct ModelCatalogResponse {
    active_model_id: String,
    models: Vec<ModelCatalogEntry>,
}

#[derive(Serialize)]
struct ModelCatalogEntry {
    id: String,
    display_name: String,
    engines: [EngineAvailability; 4],
}

#[derive(Serialize)]
struct EngineAvailability {
    engine: &'static str,
    available: bool,
    reason: Option<&'static str>,
}

pub(super) fn catalog(store: &LocalStore) -> Result<ModelCatalogResponse> {
    let mut registry = embedding::registry()?;
    let active_model_id = match store.active_selection()? {
        Some(selection) => {
            registry
                .models
                .entry(selection.model.clone())
                .or_insert(selection.profile);
            selection.model
        }
        None => store.retrieval_model()?,
    };
    Ok(project_catalog(active_model_id, &registry))
}

fn project_catalog(active_model_id: String, registry: &Registry) -> ModelCatalogResponse {
    let models = registry
        .models
        .iter()
        .map(|(id, model)| {
            let display_name = match id.as_str() {
                "m3" => "BGE-M3",
                "bge-small-zh-v1.5" => "BGE Small Chinese v1.5",
                "m3-mlx" => "BGE-M3 MLX",
                id => id,
            };
            let cpu = id != "m3-mlx"
                && model.onnx.is_object()
                && registry.backends.get("onnx").is_some_and(|backend| {
                    backend.backend == "onnx"
                        && backend.native.is_none()
                        && Selection {
                            model: id.clone(),
                            backend: backend.id.clone(),
                            engine: Engine::Cpu,
                            variant: None,
                            profile: model.clone(),
                            backend_profile: backend.clone(),
                            generation: String::new(),
                        }
                        .validate()
                        .is_ok()
                });
            let engines = [
                EngineAvailability {
                    engine: "cpu",
                    available: cpu,
                    reason: (!cpu).then_some("no_verified_standard_cpu_profile"),
                },
                EngineAvailability {
                    engine: "gpu",
                    available: false,
                    reason: Some("native_provider_not_verified"),
                },
                EngineAvailability {
                    engine: "npu",
                    available: false,
                    reason: Some("native_provider_not_verified"),
                },
                EngineAvailability {
                    engine: "mlx",
                    available: false,
                    reason: Some("native_provider_not_verified"),
                },
            ];
            ModelCatalogEntry {
                id: id.clone(),
                display_name: display_name.to_owned(),
                engines,
            }
        })
        .collect();
    ModelCatalogResponse {
        active_model_id,
        models,
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
