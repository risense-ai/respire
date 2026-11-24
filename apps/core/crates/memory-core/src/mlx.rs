//! Apple MLX BGE-M3 dense embedding backend.
//!
//! Only compiled for Apple Silicon macOS with the `mlx` feature. Model bytes
//! arrive from the host; Core never opens model files.

use crate::embedding::EmbeddingProfile;
use crate::onnx::{Engine, InferenceOptions, InferenceOutput};
use anyhow::{anyhow, bail, Context, Result};
use mlx_rs::{
    fast,
    ops::addmm,
    with_stream, Array, Stream,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const HIDDEN: i32 = 1024;
const HEADS: i32 = 16;
const HEAD_DIM: i32 = HIDDEN / HEADS;
const LAYERS: usize = 24;
const INTERMEDIATE: i32 = 4096;
const MAX_POSITIONS: i32 = 8194;
const LAYER_NORM_EPS: f32 = 1e-5;

pub(crate) struct MlxSession {
    weights: HashMap<String, Array>,
    profile: EmbeddingProfile,
    engine: Engine,
    selected: String,
    pub stalled: Arc<AtomicBool>,
    execution_limit: Duration,
}

impl MlxSession {
    pub(crate) fn load(model_bytes: &[u8], options: InferenceOptions, profile: &EmbeddingProfile) -> Result<Self> {
        anyhow::ensure!(!model_bytes.is_empty(), "host supplied an empty MLX model");
        anyhow::ensure!(profile.dimensions == HIDDEN as usize, "MLX backend supports BGE-M3 only ({HIDDEN} dimensions)");
        anyhow::ensure!(profile.max_tokens <= MAX_POSITIONS as usize, "MLX model position window exceeded");
        anyhow::ensure!(profile.pooling != crate::embedding::Pooling::SentenceEmbedding, "MLX backend produces token states; sentence_embedding pooling is unsupported");
        let selected = match options.engine {
            Engine::Cpu => "MLX CPU",
            Engine::Gpu => "MLX GPU",
            Engine::Npu => bail!("MLX backend does not support an NPU engine; select cpu or gpu"),
        };
        let tensors = safetensors::SafeTensors::deserialize(model_bytes)
            .context("parse host-supplied MLX safetensors")?;
        let mut weights = HashMap::with_capacity(tensors.len());
        for (name, view) in tensors.tensors() {
            let array = Array::try_from(view)
                .with_context(|| format!("load MLX tensor {name}"))?;
            weights.insert(name.to_owned(), array);
        }
        let session = Self {
            weights,
            profile: profile.clone(),
            engine: options.engine,
            selected: selected.to_owned(),
            stalled: Arc::new(AtomicBool::new(false)),
            execution_limit: Duration::from_secs(options.execution_timeout_secs),
        };
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn selected(&self) -> &str { &self.selected }

    pub(crate) fn stalled(&self) -> &Arc<AtomicBool> { &self.stalled }

    fn validate(&self) -> Result<()> {
        self.expect_shape("embeddings.word_embeddings.weight", &[250002, HIDDEN])?;
        self.expect_shape("embeddings.position_embeddings.weight", &[MAX_POSITIONS, HIDDEN])?;
        self.expect_shape("embeddings.token_type_embeddings.weight", &[1, HIDDEN])?;
        self.expect_shape("embeddings.LayerNorm.weight", &[HIDDEN])?;
        self.expect_shape("embeddings.LayerNorm.bias", &[HIDDEN])?;
        for layer in 0..LAYERS {
            let prefix = format!("encoder.layer.{layer}.");
            for name in ["attention.self.query", "attention.self.key", "attention.self.value",
                "attention.output.dense"] {
                self.expect_shape(&format!("{prefix}{name}.weight"), &[HIDDEN, HIDDEN])?;
                self.expect_shape(&format!("{prefix}{name}.bias"), &[HIDDEN])?;
            }
            for name in ["attention.output.LayerNorm", "output.LayerNorm"] {
                self.expect_shape(&format!("{prefix}{name}.weight"), &[HIDDEN])?;
                self.expect_shape(&format!("{prefix}{name}.bias"), &[HIDDEN])?;
            }
            self.expect_shape(&format!("{prefix}intermediate.dense.weight"), &[INTERMEDIATE, HIDDEN])?;
            self.expect_shape(&format!("{prefix}intermediate.dense.bias"), &[INTERMEDIATE])?;
            self.expect_shape(&format!("{prefix}output.dense.weight"), &[HIDDEN, INTERMEDIATE])?;
            self.expect_shape(&format!("{prefix}output.dense.bias"), &[HIDDEN])?;
        }
        Ok(())
    }

    fn expect_shape(&self, name: &str, shape: &[i32]) -> Result<()> {
        let tensor = self.weights.get(name).ok_or_else(|| anyhow!("MLX model is missing {name}"))?;
        anyhow::ensure!(tensor.shape() == shape, "MLX tensor {name} has shape {:?}, expected {shape:?}", tensor.shape());
        Ok(())
    }

    fn tensor(&self, name: &str) -> Result<&Array> {
        self.weights.get(name).ok_or_else(|| anyhow!("MLX model is missing {name}"))
    }

    fn linear(&self, x: &Array, prefix: &str) -> Result<Array> {
        let weight = self.tensor(&format!("{prefix}.weight"))?;
        let bias = self.tensor(&format!("{prefix}.bias"))?;
        addmm(bias, x, weight.t(), None, None).with_context(|| format!("MLX {prefix} matmul"))
    }

    fn layer_norm(&self, x: &Array, prefix: &str) -> Result<Array> {
        let weight = self.tensor(&format!("{prefix}.weight"))?;
        let bias = self.tensor(&format!("{prefix}.bias"))?;
        fast::layer_norm(x, Some(weight), Some(bias), LAYER_NORM_EPS)
            .with_context(|| format!("MLX {prefix} layer norm"))
    }

    fn embedding(&self, name: &str, ids: &Array) -> Result<Array> {
        self.tensor(name)?.take_axis(ids, 0).with_context(|| format!("MLX {name} lookup"))
    }

    pub(crate) fn run(
        &mut self,
        ids: &[i64],
        mask: &[i64],
        types: &[i64],
        batch: usize,
        seq: usize,
    ) -> Result<InferenceOutput> {
        self.run_with_engine(self.engine, ids, mask, types, batch, seq)
    }

    pub(crate) fn run_cpu(
        &mut self,
        ids: &[i64],
        mask: &[i64],
        types: &[i64],
        batch: usize,
        seq: usize,
    ) -> Result<InferenceOutput> {
        self.run_with_engine(Engine::Cpu, ids, mask, types, batch, seq)
    }

    fn run_with_engine(
        &mut self,
        engine: Engine,
        ids: &[i64],
        mask: &[i64],
        types: &[i64],
        batch: usize,
        seq: usize,
    ) -> Result<InferenceOutput> {
        let count = batch.checked_mul(seq).ok_or_else(|| anyhow!("MLX input shape overflow"))?;
        anyhow::ensure!(batch > 0 && seq > 0 && seq <= self.profile.max_tokens,
            "invalid MLX inference input shape");
        anyhow::ensure!(ids.len() == count && mask.len() == count && types.len() == count,
            "invalid MLX inference input shape");
        anyhow::ensure!(types.iter().all(|value| *value == 0),
            "MLX BGE-M3 accepts only token type 0");
        anyhow::ensure!(!self.stalled.load(Ordering::Acquire),
            "inference session is unresponsive; host runtime recovery is required");
        let started = Instant::now();
        let stream = match engine {
            Engine::Cpu => Stream::cpu(),
            Engine::Gpu => Stream::gpu(),
            Engine::Npu => bail!("MLX backend does not support an NPU engine"),
        };
        let hidden = with_stream(&stream, || self.forward(ids, mask, batch, seq))
            .context("MLX inference failed")?;
        hidden.eval().context("materialize MLX inference output")?;
        let shape = hidden.shape().iter().map(|value| *value as i64).collect::<Vec<_>>();
        let data = hidden.to_vec_cast::<f32>().context("convert MLX output to f32")?;
        let elapsed = started.elapsed();
        if elapsed > self.execution_limit {
            self.stalled.store(true, Ordering::Release);
            bail!("MLX inference exceeded its execution deadline; retry the command");
        }
        Ok(InferenceOutput::Owned { shape, data })
    }

    fn forward(&self, ids: &[i64], mask: &[i64], batch: usize, seq: usize) -> Result<Array> {
        let b = batch as i32;
        let s = seq as i32;
        let ids_i32 = ids.iter().map(|value| *value as i32).collect::<Vec<_>>();
        let input_ids = Array::from_slice(&ids_i32, &[b, s]);
        let positions = position_ids(mask, batch, seq);
        let position_ids = Array::from_slice(&positions, &[b, s]);
        let type_values = vec![0i32; batch * seq];
        let type_ids = Array::from_slice(&type_values, &[b, s]);
        let x = self.embedding("embeddings.word_embeddings.weight", &input_ids)?
            .add(self.embedding("embeddings.position_embeddings.weight", &position_ids)?)?
            .add(self.embedding("embeddings.token_type_embeddings.weight", &type_ids)?)?;
        let mut x = self.layer_norm(&x, "embeddings.LayerNorm")?;
        let mask_values = mask.iter().map(|value| if *value == 0 { -10000.0f32 } else { 0.0f32 }).collect::<Vec<_>>();
        let bias = Array::from_slice(&mask_values, &[b, 1, 1, s]).as_dtype(x.dtype())?;
        for layer in 0..LAYERS {
            let prefix = format!("encoder.layer.{layer}.");
            let query = self.linear(&x, &format!("{prefix}attention.self.query"))?
                .reshape(&[b, s, HEADS, HEAD_DIM])?
                .transpose_axes(&[0, 2, 1, 3])?;
            let key = self.linear(&x, &format!("{prefix}attention.self.key"))?
                .reshape(&[b, s, HEADS, HEAD_DIM])?
                .transpose_axes(&[0, 2, 1, 3])?;
            let value = self.linear(&x, &format!("{prefix}attention.self.value"))?
                .reshape(&[b, s, HEADS, HEAD_DIM])?
                .transpose_axes(&[0, 2, 1, 3])?;
            let context = fast::scaled_dot_product_attention(
                &query, &key, &value, 1.0 / (HEAD_DIM as f32).sqrt(), &bias, None,
            )?
            .transpose_axes(&[0, 2, 1, 3])?
            .reshape(&[b, s, HIDDEN])?;
            let attention = self.linear(&context, &format!("{prefix}attention.output.dense"))?;
            x = self.layer_norm(&x.add(&attention)?, &format!("{prefix}attention.output.LayerNorm"))?;
            let intermediate = self.linear(&x, &format!("{prefix}intermediate.dense"))?;
            let intermediate = gelu(&intermediate)?;
            let output = self.linear(&intermediate, &format!("{prefix}output.dense"))?;
            x = self.layer_norm(&x.add(&output)?, &format!("{prefix}output.LayerNorm"))?;
        }
        Ok(x)
    }
}

fn gelu(x: &Array) -> Result<Array> {
    let erf = mlx_rs::ops::erf(&(x * std::f32::consts::FRAC_1_SQRT_2))?;
    Ok((x * 0.5f32) * &(Array::from_f32(1.0) + &erf))
}

/// XLM-R position ids: cumsum over non-padding tokens plus the padding index.
fn position_ids(mask: &[i64], batch: usize, seq: usize) -> Vec<i32> {
    let mut positions = Vec::with_capacity(batch * seq);
    for row in 0..batch {
        let mut running = 0i64;
        for column in 0..seq {
            let present = mask[row * seq + column] != 0;
            if present {
                running += 1;
                positions.push((running + 1) as i32);
            } else {
                positions.push(1);
            }
        }
    }
    positions
}
