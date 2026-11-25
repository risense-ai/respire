use super::*;
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;

const REQUEST: &str = include_str!("../../../../contracts/core-abi/request.schema.json");
const RESPONSE: &str = include_str!("../../../../contracts/core-abi/response.schema.json");
const HEADER: &str = include_str!("../../../../contracts/core-abi/respire_core.h");

struct NativeCore(*mut Core);

impl NativeCore {
    fn create() -> Result<Self> {
        let input = br#"{"test_mode":true}"#;
        let mut handle = ptr::null_mut();
        let mut result = Buffer::empty();
        // SAFETY: input lives through the call; both stack outputs are empty
        // and writable. The test exclusively owns the returned handle.
        let code = unsafe { rs_core_create(input.as_ptr(), input.len(), &mut handle, &mut result) };
        unsafe { rs_core_buffer_free(&mut result) };
        ensure!(
            code == OK && !handle.is_null(),
            "native Core creation failed"
        );
        Ok(Self(handle))
    }

    fn call(&mut self, bytes: &[u8]) -> Result<(i32, Value)> {
        let mut result = Buffer::empty();
        // SAFETY: both buffers are live, the output is initialized empty,
        // and all calls use the uniquely owned handle sequentially.
        let code = unsafe { rs_core_call(self.0, bytes.as_ptr(), bytes.len(), &mut result) };
        let body = if result.data.is_null() || result.len == 0 {
            Err(anyhow::anyhow!("native ABI omitted its response buffer"))
        } else {
            let readable = unsafe { slice::from_raw_parts(result.data, result.len) };
            serde_json::from_slice(readable).context("native response is not JSON")
        };
        unsafe { rs_core_buffer_free(&mut result) };
        Ok((code, body?))
    }
}

impl Drop for NativeCore {
    fn drop(&mut self) {
        // SAFETY: created by rs_core_create, exclusively owned here.
        unsafe { rs_core_destroy(self.0) };
    }
}

// This deliberately checks only the envelope vocabulary the schemas actually
// use, not all JSON Schema or operation-specific payload constraints.
fn envelope_matches(schema: &Value, actual: &Value) -> Result<bool> {
    let schema = schema
        .as_object()
        .context("envelope schema must be an object")?;
    let mut valid = true;
    for (keyword, rule) in schema {
        valid &= match keyword.as_str() {
            "$schema" | "title" | "$defs" => true,
            "const" => actual == rule,
            "enum" => rule
                .as_array()
                .context("invalid envelope enum")?
                .contains(actual),
            "type" => match rule.as_str() {
                Some("object") => actual.is_object(),
                Some("string") => actual.is_string(),
                _ => anyhow::bail!("unhandled envelope type: {rule}"),
            },
            "required" => rule
                .as_array()
                .context("invalid required list")?
                .iter()
                .map(|name| name.as_str().context("invalid required name"))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .all(|name| actual.get(name).is_some()),
            "additionalProperties" => {
                ensure!(rule == false, "unhandled additionalProperties");
                actual.as_object().is_some_and(|values| {
                    schema
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|fields| values.keys().all(|key| fields.contains_key(key)))
                })
            }
            "properties" => {
                let fields = rule.as_object().context("invalid envelope properties")?;
                let mut all = true;
                for (key, field) in fields {
                    if let Some(value) = actual.get(key) {
                        all &= envelope_matches(field, value)?;
                    }
                }
                all
            }
            "oneOf" => {
                rule.as_array()
                    .context("invalid envelope oneOf")?
                    .iter()
                    .map(|item| envelope_matches(item, actual))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|matched| *matched)
                    .count()
                    == 1
            }
            "not" => !envelope_matches(rule, actual)?,
            _ => anyhow::bail!("unhandled envelope keyword: {keyword}"),
        };
    }
    Ok(valid)
}

// The request's root allOf contains operation-specific payload refinements.
// Omitting them exercises v1 framing and dispatch without pretending that the
// empty diagnostic payload is a valid domain operation. Fail if a refinement
// starts modifying the envelope itself.
fn request_envelope_schema() -> Result<Value> {
    let mut schema: Value = serde_json::from_str(REQUEST)?;
    let refinements = schema
        .as_object_mut()
        .context("request schema must be an object")?
        .remove("allOf")
        .context("expected request payload refinements")?;
    for refinement in refinements
        .as_array()
        .context("invalid payload refinements")?
    {
        let fields = refinement
            .as_object()
            .context("invalid payload refinement")?;
        ensure!(
            fields.len() == 2 && fields.contains_key("if") && fields.contains_key("then"),
            "unknown root request refinement"
        );
        let then = fields["then"]
            .as_object()
            .context("invalid payload refinement action")?;
        for (keyword, constraint) in then {
            match keyword.as_str() {
                "required" => ensure!(
                    constraint == &json!(["payload"]),
                    "request refinement is not payload-only"
                ),
                "properties" => ensure!(
                    constraint
                        .as_object()
                        .is_some_and(|fields| fields.len() == 1 && fields.contains_key("payload")),
                    "request refinement is not payload-only"
                ),
                _ => anyhow::bail!("request refinement is not payload-only"),
            }
        }
    }
    Ok(schema)
}
fn distinct_strings(input: &Value) -> Result<BTreeSet<&str>> {
    let list = input.as_array().context("operation list missing")?;
    let values = list
        .iter()
        .map(|value| value.as_str().context("operation is not text"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(values.len() == list.len(), "duplicate operation");
    Ok(values)
}

fn header_value(name: &str) -> Result<&'static str> {
    HEADER
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == "#define" && parts.next()? == name)
                .then(|| parts.next())
                .flatten()
        })
        .with_context(|| format!("missing C header constant {name}"))
}

#[test]
fn ffi_schema_parity_advertises_and_dispatches_exact_v1_operations() -> Result<()> {
    let request_schema = request_envelope_schema()?;
    let response_schema: Value = serde_json::from_str(RESPONSE)?;
    let schema_version = request_schema["properties"]["schema_version"]["const"]
        .as_u64()
        .context("schema version missing")?;
    assert_eq!(
        response_schema["properties"]["schema_version"]["const"],
        schema_version
    );
    let request_id = "schema-parity";
    let mut core = NativeCore::create()?;
    let request = |operation: &str| json!({"schema_version":schema_version,"request_id":request_id,"operation":operation});
    let (code, capabilities) = core.call(&serde_json::to_vec(&request("capabilities"))?)?;
    assert_eq!(code, OK);
    assert!(envelope_matches(&response_schema, &capabilities)?);
    assert_eq!(capabilities["request_id"], request_id);
    assert_eq!(capabilities["result"]["schema_version"], schema_version);
    assert_eq!(capabilities["result"]["abi_version"], rs_core_abi_version());
    let abi_hex = header_value("RS_CORE_ABI_VERSION")?
        .strip_prefix("UINT32_C(0x")
        .and_then(|value| value.strip_suffix(')'))
        .context("unexpected C ABI version constant")?;
    assert_eq!(u32::from_str_radix(abi_hex, 16)?, rs_core_abi_version());

    let schema_operations = distinct_strings(&request_schema["properties"]["operation"]["enum"])?;
    assert_eq!(
        schema_operations,
        distinct_strings(&capabilities["result"]["operations"])?
    );
    for operation in schema_operations {
        let mut request = request(operation);
        request["payload"] = json!({});
        assert!(envelope_matches(&request_schema, &request)?);
        let (code, actual) = core.call(&serde_json::to_vec(&request)?)?;
        assert!(
            code == OK || code == ENGINE_FAILURE,
            "operation did not enter its handler: {operation}, code {code}"
        );
        assert!(
            envelope_matches(&response_schema, &actual)?,
            "bad envelope for {operation}"
        );
        assert_eq!(actual["request_id"], request_id);
        if code == ENGINE_FAILURE {
            assert_eq!(actual["error"]["code"], code);
            let message = actual["error"]["message"]
                .as_str()
                .context("missing engine message")?;
            assert!(
                !message.contains("unknown business operation")
                    && !message.contains("unsupported business operation"),
                "advertised but undispatched operation: {operation}"
            );
        }
    }
    Ok(())
}

#[test]
fn ffi_schema_parity_observes_header_status_and_envelope_rejection() -> Result<()> {
    let request_schema = request_envelope_schema()?;
    let response_schema: Value = serde_json::from_str(RESPONSE)?;
    let map = [
        ("RS_CORE_INVALID_ARGUMENT", INVALID_ARGUMENT),
        ("RS_CORE_ABI_MISMATCH", ABI_MISMATCH),
        ("RS_CORE_UNSUPPORTED_OPERATION", UNSUPPORTED_OPERATION),
        ("RS_CORE_ENGINE_FAILURE", ENGINE_FAILURE),
        ("RS_CORE_PANIC", PANIC),
    ];
    assert_eq!(header_value("RS_CORE_OK")?.parse::<i32>()?, OK);
    let code_schema = response_schema["properties"]["error"]["properties"]["code"]["enum"]
        .as_array()
        .context("error codes missing")?;
    let schema_codes = code_schema
        .iter()
        .map(|value| value.as_i64().context("error code not integral"))
        .collect::<Result<BTreeSet<_>>>()?;
    assert_eq!(schema_codes.len(), code_schema.len());
    assert_eq!(
        schema_codes,
        map.iter().map(|(_, code)| i64::from(*code)).collect()
    );
    for (macro_name, code) in map {
        assert_eq!(header_value(macro_name)?.parse::<i32>()?, code);
    }
    // Status 4 and 5 remain reserved C statuses, not JSON envelope codes.
    assert!(!schema_codes.contains(&i64::from(
        header_value("RS_CORE_MODEL_UNAVAILABLE")?.parse::<i32>()?
    )));
    assert!(!schema_codes.contains(&i64::from(
        header_value("RS_CORE_ARTIFACT_INCOMPATIBLE")?.parse::<i32>()?
    )));

    let mut core = NativeCore::create()?;
    for (request, expected, request_id) in [
        (
            json!({"schema_version":2,"request_id":"bad-version","operation":"capabilities"}),
            ABI_MISMATCH,
            "bad-version",
        ),
        (
            json!({"schema_version":1,"request_id":"bad-op","operation":"not_a_core_operation"}),
            UNSUPPORTED_OPERATION,
            "bad-op",
        ),
        (
            json!({"schema_version":1,"request_id":"extra","operation":"capabilities","extra":true}),
            INVALID_ARGUMENT,
            "",
        ),
        (
            json!({"schema_version":1,"operation":"capabilities"}),
            INVALID_ARGUMENT,
            "",
        ),
        (
            json!({"schema_version":1,"request_id":9,"operation":"capabilities"}),
            INVALID_ARGUMENT,
            "",
        ),
    ] {
        assert!(!envelope_matches(&request_schema, &request)?);
        let (code, actual) = core.call(&serde_json::to_vec(&request)?)?;
        assert_eq!(code, expected);
        assert_eq!(actual["request_id"], request_id);
        assert_eq!(actual["error"]["code"], code);
        assert!(envelope_matches(&response_schema, &actual)?);
        let mut invalid = actual.clone();
        invalid["result"] = Value::Null;
        assert!(!envelope_matches(&response_schema, &invalid)?);
        invalid
            .as_object_mut()
            .context("no response object")?
            .remove("error");
        invalid
            .as_object_mut()
            .context("no response object")?
            .remove("result");
        assert!(!envelope_matches(&response_schema, &invalid)?);
    }
    Ok(())
}
