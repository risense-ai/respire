mod build_support;
use serde_json::{Value, json};
use std::{env, error::Error, fs, path::PathBuf};
use syn::visit_mut::VisitMut;

const SPEC: &str = "../../contracts/runtime-api/openapi.yaml";

// Progenitor consumes OpenAPI 3.0. Only adapt nullable forms used by the
// canonical 3.1 contract; clients in other languages consume the original.
fn adapt_nullable(value: &mut Value) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Object(map) => {
            if let Some(types) = map.get("type").and_then(Value::as_array) {
                let names = types.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                if names.len() != 2 || !names.contains(&"null") {
                    return Err("Unsupported OpenAPI 3.1 type union for Rust generation".into());
                }
                let nonnull = names
                    .into_iter()
                    .find(|name| *name != "null")
                    .ok_or("Missing non-null OpenAPI type")?
                    .to_owned();
                map.insert("type".into(), json!(nonnull));
                map.insert("nullable".into(), json!(true));
            }
            if let Some(variants) = map.get("oneOf").and_then(Value::as_array) {
                if variants.len() == 2 && variants.iter().any(|item| item["type"] == "null") {
                    let nonnull = variants
                        .iter()
                        .find(|item| item["type"] != "null")
                        .ok_or("Missing non-null OpenAPI branch")?
                        .clone();
                    map.remove("oneOf");
                    map.insert("allOf".into(), json!([nonnull]));
                    map.insert("nullable".into(), json!(true));
                }
            }
            for child in map.values_mut() {
                adapt_nullable(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                adapt_nullable(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={SPEC}");
    let mut doc: Value = serde_yaml::from_slice(&fs::read(SPEC)?)?;
    if doc["openapi"] != "3.1.0" {
        return Err("Expected the canonical OpenAPI 3.1.0 contract".into());
    }
    let presence = build_support::optional_nullable(&doc)?;
    doc["openapi"] = json!("3.0.3");
    adapt_nullable(&mut doc)?;
    // Progenitor groups default responses with success as well as errors.
    // The canonical default is a ProblemDetails error; project it as error ranges.
    for route in doc["paths"]
        .as_object_mut()
        .ok_or("Missing OpenAPI paths")?
        .values_mut()
    {
        for operation in route
            .as_object_mut()
            .ok_or("Invalid OpenAPI path item")?
            .values_mut()
        {
            let Some(responses) = operation
                .get_mut("responses")
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            if let Some(default) = responses.remove("default") {
                if default["$ref"] != "#/components/responses/RuntimeProblem" {
                    return Err("Unexpected default Runtime response".into());
                }
                responses.entry("4XX").or_insert_with(|| default.clone());
                responses.entry("5XX").or_insert(default);
            }
        }
    }
    let spec: openapiv3::OpenAPI = serde_json::from_value(doc)?;
    let tokens = progenitor::Generator::default().generate_tokens(&spec)?;
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("Missing OUT_DIR")?);
    let mut file: syn::File = syn::parse2(tokens)?;
    build_support::preserve_presence(&mut file, presence)?;
    build_support::SpecificStatusesFirst.visit_file_mut(&mut file);
    fs::write(out.join("runtime_sdk.rs"), prettyplease::unparse(&file))?;
    Ok(())
}
