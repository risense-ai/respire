#![deny(unsafe_code)]

//! C ABI ownership boundary. Algorithms remain in the private engine.

use serde::Deserialize;
use serde_json::{json, Value};

mod analysis;
mod deepen_topology;
mod model_input;
mod business;
mod classify;
mod classify_dispatch;
mod reports;
mod selection;
mod related;
mod resident;
mod taxonomy;
mod metadata;
mod backend_probe;
#[path = "i18n/legacy_notation.rs"]
mod localized_notation;

const ABI_VERSION: u32 = (1 << 16) | 2;
const OK: i32 = 0;
const INVALID_ARGUMENT: i32 = 1;
const ABI_MISMATCH: i32 = 2;
const UNSUPPORTED_OPERATION: i32 = 3;
const ENGINE_FAILURE: i32 = 6;
const PANIC: i32 = 7;

#[derive(Default)]
struct Core {
    poisoned: bool,
    pending_input: Option<model_input::Pending>,
    business: business::State,
}

trait Transport {
    fn request(&mut self, request: &Value) -> anyhow::Result<Value>;
}

#[derive(Clone)]
struct RequestTransport<'call> {
    inner: std::rc::Rc<std::cell::RefCell<&'call mut dyn Transport>>,
}
impl<'call> RequestTransport<'call> {
    fn new(inner: &'call mut dyn Transport) -> Self { Self { inner: std::rc::Rc::new(std::cell::RefCell::new(inner)) } }
}
impl Transport for RequestTransport<'_> {
    fn request(&mut self, request: &Value) -> anyhow::Result<Value> {
        self.inner.try_borrow_mut().map_err(|_| anyhow::anyhow!("host transport reentry is not allowed"))?.request(request)
    }
}

struct NoTransport;
impl Transport for NoTransport {
    fn request(&mut self, _: &Value) -> anyhow::Result<Value> {
        anyhow::bail!("model request requires a host transport callback")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    request_id: String,
    operation: String,
    #[serde(default)]
    payload: Value,
}

struct Failure {
    code: i32,
    message: String,
}

#[cfg(test)]
mod failure_tests;

impl Failure {
    fn engine(error: anyhow::Error) -> Self {
        Self { code: ENGINE_FAILURE, message: format!("{error:#}") }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_ARGUMENT,
            message: message.into(),
        }
    }
}

/// Dispatch and the advertised ABI operation list are derived from this table.
enum AbiOperation {
    Capabilities,
    BackendLoadProbe,
    MetadataValidate,
    EvidenceEvaluate,
    ClassifyBusiness,
    QueryBusiness,
    Business(&'static str),
}

macro_rules! abi_operations {
    ($( $name:literal => $handler:ident ),+ $(,)?) => {
        const ABI_OPERATIONS: &[&str] = &[$($name),+];

        fn abi_operation(name: &str) -> Option<AbiOperation> {
            match name {
                $($name => Some(abi_operations!(@handler $handler $name)),)+
                _ => None,
            }
        }
    };
    (@handler Business $name:literal) => { AbiOperation::Business($name) };
    (@handler $handler:ident $name:literal) => { AbiOperation::$handler };
}

abi_operations! {
    "capabilities" => Capabilities,
    "prepare" => Business,
    "query" => Business,
    "remember_candidates" => Business,
    "candidate_report" => Business,
    "analyze_duplicates" => Business,
    "tree_cure" => Business,
    "deepen_plan" => Business,
    "taxonomy_classify" => Business,
    "classify_business" => ClassifyBusiness,
    "query_business" => QueryBusiness,
    "related_business" => Business,
    "tree_float" => Business,
    "index_generation" => Business,
    "index_status" => Business,
    "resident_publish" => Business,
    "resident_release" => Business,
    "index_reusable" => Business,
    "model_status" => Business,
    "model_probe" => Business,
    "backend_load_probe" => BackendLoadProbe,
    "engine_control" => Business,
    "memory_metadata_validate" => MetadataValidate,
    "memory_evidence_evaluate" => EvidenceEvaluate,
}

impl Core {
    fn call(&mut self, bytes: &[u8]) -> (i32, Vec<u8>) {
        self.call_with_transport(bytes, &mut NoTransport)
    }

    fn call_with_transport(&mut self, bytes: &[u8], transport: &mut dyn Transport) -> (i32, Vec<u8>) {
        if self.poisoned {
            return response(
                "",
                Err(Failure {
                    code: PANIC,
                    message: "Core handle is poisoned; destroy and recreate it".into(),
                }),
            );
        }
        let request = match serde_json::from_slice::<Request>(bytes) {
            Ok(request) => request,
            Err(error) => return response("", Err(Failure::invalid(error.to_string()))),
        };
        if request.schema_version != 1 {
            return response(
                &request.request_id,
                Err(Failure {
                    code: ABI_MISMATCH,
                    message: "unsupported schema version".into(),
                }),
            );
        }
        let result = self.dispatch(&request.operation, request.payload, transport);
        response(&request.request_id, result)
    }

    fn dispatch(&mut self, operation: &str, payload: Value, transport: &mut dyn Transport) -> Result<Value, Failure> {
        let mut transport = RequestTransport::new(transport);
        match abi_operation(operation) {
            Some(AbiOperation::BackendLoadProbe) => backend_probe::dispatch(payload, &mut transport).map_err(Failure::engine),
            Some(AbiOperation::MetadataValidate) => metadata::validate(payload).map_err(Failure::engine),
            Some(AbiOperation::EvidenceEvaluate) => metadata::evaluate(payload).map_err(Failure::engine),
            Some(AbiOperation::ClassifyBusiness) => classify_dispatch::dispatch(payload, &mut transport).map_err(Failure::engine),
            Some(AbiOperation::QueryBusiness) => selection::recall_with_transport(&mut self.business, payload, transport).map_err(Failure::engine),
            Some(AbiOperation::Business(operation)) => business::dispatch_with_transport(&mut self.business, operation, payload, Some(transport)).map_err(Failure::engine),
            Some(AbiOperation::Capabilities) => Ok(json!({
                "core_version": CORE_VERSION,
                "abi_version": ABI_VERSION,
                "schema_version": 1,
                "stage": "internal-business-sdk",
                "artifact_version": 1,
                "inference_execution": "in_process",
                "model_transport": "host_callback",
                "model_input": "host_memory_buffers",
                "model_input_stream_contract": if cfg!(feature = "cpu-owned-ort-experiment") { 1 } else { 0 },
                "model_input_max_bytes": model_input::MAX_MODEL_BYTES,
                "model_input_max_chunk_bytes": model_input::MAX_CHUNK_BYTES,
                "artifact_storage": "host",
                "retrieval_contract": 1,
                "recall_calibration_contract": 1,
                "recall_fusion_contract": 1,
                "resident_retrieval_contract": 1,
                "inference_thread_contract": 1,
                "embedding_profile_contract": 1,
                "embedding_transport_contract": 1,
                "backend_load_probe_contract": 1,
                "feedback_contract": FEEDBACK_CONTRACT,
                "render_budget_contract": 1,
                "brief_render_contract": 1,
                "render_min_score_contract": 1,
                "howto_recall_contract": 1,
                "superseded_status_contract": 1,
                "memory_metadata_contract": 1,
                "read_heat_contract": 1,
                "mlx_backend_contract": if cfg!(all(feature = "mlx", target_os = "macos", target_arch = "aarch64")) { 1 } else { 0 },
                "inference_sessions": {"foreground":1,"background":1,"run_concurrency_per_session":1},
                "operations": ABI_OPERATIONS
            })),
            None => Err(Failure {
                code: UNSUPPORTED_OPERATION,
                message: "unsupported operation".into(),
            }),
        }
    }
}

fn response(request_id: &str, result: Result<Value, Failure>) -> (i32, Vec<u8>) {
    let (code, body) = match result {
        Ok(result) => (
            OK,
            json!({"schema_version": 1, "request_id": request_id, "result": result}),
        ),
        Err(error) => (
            error.code,
            json!({
                "schema_version": 1, "request_id": request_id,
                "error": {"code": error.code, "message": error.message}
            }),
        ),
    };
    (code, body.to_string().into_bytes())
}

#[allow(unsafe_code)]
mod exports;
pub use exports::rs_core_abi_version;

// Keep the native and Rust consumers on the capability reported by dispatch.
pub const FEEDBACK_CONTRACT: u64 = 1;
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
#[path = "i18n/test_data.rs"]
mod localized_test_data;
