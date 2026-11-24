//! Explicit, memory-only embedding configuration shared by inference backends.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProfile {
    pub model_id: String,
    pub revision: String,
    pub dimensions: usize,
    pub max_tokens: usize,
    pub pooling: Pooling,
    pub normalize: bool,
    #[serde(default)]
    pub query_prefix: String,
    #[serde(default)]
    pub document_prefix: String,
    #[serde(default)]
    pub pad_token_id: u32,
    #[serde(default)]
    pub pad_to_max_tokens: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pooling { Cls, Mean, SentenceEmbedding }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnnxNames {
    pub input_ids: String,
    pub attention_mask: Option<String>,
    pub token_type_ids: Option<String>,
    pub output: Option<String>,
}
impl Default for OnnxNames {
    fn default() -> Self { Self { input_ids: "input_ids".into(), attention_mask: Some("attention_mask".into()), token_type_ids: Some("token_type_ids".into()), output: None } }
}

impl OnnxNames {
    pub fn validate(&self) -> Result<()> {
        let inputs = std::iter::once(self.input_ids.as_str()).chain(self.attention_mask.as_deref()).chain(self.token_type_ids.as_deref()).collect::<Vec<_>>();
        ensure!(inputs.iter().all(|name| !name.trim().is_empty()), "ONNX input names must be nonempty");
        ensure!(inputs.iter().collect::<std::collections::HashSet<_>>().len() == inputs.len(), "ONNX input names must be distinct");
        ensure!(self.output.as_ref().is_none_or(|name| !name.trim().is_empty()), "ONNX output name must be nonempty");
        Ok(())
    }
}

impl EmbeddingProfile {
    pub fn m3() -> Self { Self { model_id:"Xenova/bge-m3".into(), revision:"4de13258303883538bd53b696b452bf8099f0858".into(), dimensions:1024, max_tokens:512, pooling:Pooling::Cls, normalize:true, query_prefix:String::new(), document_prefix:String::new(), pad_token_id:0, pad_to_max_tokens:false } }

    pub fn from_config(model: &str, config: &Value) -> Result<Self> {
        validate_config(model, config)?;
        let profile: Self = match config.get("embedding") {
            Some(value) => serde_json::from_value(value.clone()).context("invalid embedding profile")?,
            None => { ensure!(model == "m3", "custom models require an explicit embedding profile"); Self::m3() }
        };
        ensure!(!profile.model_id.trim().is_empty() && !profile.revision.trim().is_empty(), "embedding model_id and revision must be nonempty");
        ensure!((1..=65536).contains(&profile.dimensions), "invalid embedding dimensions");
        ensure!((4..=131072).contains(&profile.max_tokens), "invalid embedding max_tokens");
        Ok(profile)
    }

    pub fn document_identity(&self, asset_id: &str, tokenizer_id: &str) -> Result<String> {
        let mut document = self.clone();
        document.query_prefix.clear();
        let bytes = serde_json::to_vec(&(asset_id, tokenizer_id, document, "document-head-400:token-windows-v1"))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn pool(&self, shape: &[i64], data: &[f32], mask: &[i64]) -> Result<Vec<f32>> {
        let mut vector = match self.pooling {
            Pooling::SentenceEmbedding => {
                ensure!(shape == [1, self.dimensions as i64] && data.len() == self.dimensions, "invalid sentence embedding output shape: {shape:?}");
                data.to_vec()
            }
            Pooling::Cls | Pooling::Mean => {
                ensure!(shape == [1, mask.len() as i64, self.dimensions as i64] && data.len() == mask.len() * self.dimensions, "invalid embedding output shape: {shape:?}");
                if self.pooling == Pooling::Cls { data[..self.dimensions].to_vec() } else {
                    let mut pooled = vec![0.0; self.dimensions];
                    let mut count = 0usize;
                    for (token, present) in data.chunks_exact(self.dimensions).zip(mask) {
                        if *present == 0 { continue; }
                        count += 1;
                        for (sum, value) in pooled.iter_mut().zip(token) { *sum += value; }
                    }
                    ensure!(count > 0, "mean pooling requires a nonempty attention mask");
                    for value in &mut pooled { *value /= count as f32; }
                    pooled
                }
            }
        };
        ensure!(vector.iter().all(|value| value.is_finite()), "embedding contains nonfinite values");
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        ensure!(norm.is_finite() && norm > 1e-8, "invalid embedding norm");
        if self.normalize { for value in &mut vector { *value /= norm; } }
        Ok(vector)
    }
}


pub fn validate_config(model: &str, config: &Value) -> Result<()> {
    let fields = config.as_object().context("model configuration must be an object")?;
    ensure!(fields.keys().all(|name| matches!(name.as_str(), "model" | "asset_id" | "engine" | "execution_timeout_secs" | "session_options_id" | "reuse_current" | "cpu_owned_ort_experiment" | "background_intra_threads" | "backend" | "embedding" | "onnx" | "provider" | "protocol" | "model_sha256" | "tokenizer_sha256" | "host_execution_provider" | "execution_identity")), "unknown model configuration field; paths and credentials belong to the host");
    if let Some(name) = config.get("model") { ensure!(name.as_str() == Some(model), "request model and embedding configuration differ"); }
    if let Some(backend) = config.get("backend") { ensure!(matches!(backend.as_str(), Some("onnx" | "host_transport" | "mlx")), "unsupported embedding backend"); }
    ensure!(config.get("model_sha256").is_some() == config.get("tokenizer_sha256").is_some(), "model_sha256 and tokenizer_sha256 must be supplied together");
    Ok(())
}

pub fn index_identity(model: &str, config: &Value) -> Result<String> {
    let profile = EmbeddingProfile::from_config(model, config)?;
    if config.get("backend").and_then(Value::as_str) == Some("host_transport") {
        let provider = config["provider"].as_str().filter(|value| !value.is_empty()).context("host embedding configuration requires provider")?;
        let base = profile.document_identity(provider, &profile.revision)?;
        let execution_identity = config.get("execution_identity").map(|value| value.as_str().context("execution_identity must be a string")).transpose()?.unwrap_or("");
        return Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&("host_transport", base, execution_identity))?)));
    }
    let weights = config["model_sha256"].as_str().context("index generation requires model_sha256")?;
    let tokenizer = config["tokenizer_sha256"].as_str().context("index generation requires tokenizer_sha256")?;
    ensure!([weights, tokenizer].iter().all(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())), "asset checksums must be lowercase SHA-256");
    if config.get("backend").and_then(Value::as_str) == Some("mlx") {
        let execution_identity = config.get("execution_identity").map(|value| value.as_str().context("execution_identity must be a string")).transpose()?.unwrap_or("");
        let base = profile.document_identity(weights, tokenizer)?;
        return Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&("mlx", base, execution_identity))?)));
    }
    let names: OnnxNames = config.get("onnx").map(|value| serde_json::from_value(value.clone())).transpose()?.unwrap_or_default();
    names.validate()?;
    let old_weight = weights == "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d" || weights == "7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff";
    let execution_identity = config.get("execution_identity").map(|value| value.as_str().context("execution_identity must be a string")).transpose()?.unwrap_or("");
    ensure!(config["host_execution_provider"].as_bool() != Some(true) || !execution_identity.is_empty(), "host execution provider requires execution_identity");
    let mut document_profile = profile.clone();
    document_profile.query_prefix.clear();
    let m3_names = names.input_ids == "input_ids" && names.attention_mask.as_deref() == Some("attention_mask") && names.token_type_ids.as_deref() == Some("token_type_ids") && matches!(names.output.as_deref(), None | Some("last_hidden_state"));
    if model == "m3" && old_weight && tokenizer == "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790" && document_profile == EmbeddingProfile::m3() && m3_names && execution_identity.is_empty() { return Ok("m3".into()); }
    let base = profile.document_identity(weights, tokenizer)?;
    if names == OnnxNames::default() && execution_identity.is_empty() { return Ok(base); }
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&(base, names, execution_identity))?)))
}
