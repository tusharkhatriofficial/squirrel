//! LLaMA-style transformer forward pass — pure Rust, no_std.
//!
//! This implements the core transformer architecture used by LLaMA, Mistral,
//! TinyLlama, and similar models. It reads model weights from a parsed GGUF
//! file and runs the full forward pass on CPU.
//!
//! Architecture:
//!   - RMSNorm (pre-norm, not post-norm like original transformer)
//!   - Grouped Query Attention (GQA) with RoPE positional encoding
//!   - SiLU-gated MLP (FFN)
//!   - Tied or separate output embeddings
//!
//! The inference runs one token at a time (autoregressive), caching past
//! key/value vectors in a KV-cache for efficient generation.

use alloc::{vec, vec::Vec};
use crate::gguf::GgufFile;
use crate::tensor;

/// Model hyperparameters extracted from GGUF metadata.
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// Vocabulary size
    pub vocab_size: usize,
    /// Hidden dimension (embedding size)
    pub hidden_dim: usize,
    /// Intermediate dimension (FFN inner size, typically 4x hidden or custom)
    pub intermediate_dim: usize,
    /// Number of transformer layers
    pub n_layers: usize,
    /// Number of attention heads
    pub n_heads: usize,
    /// Number of key/value heads (for GQA; equals n_heads for MHA)
    pub n_kv_heads: usize,
    /// Dimension per attention head
    pub head_dim: usize,
    /// RoPE theta (frequency base, typically 10000.0)
    pub rope_theta: f32,
    /// RMSNorm epsilon
    pub norm_eps: f32,
    /// Maximum sequence length
    pub max_seq_len: usize,
}

impl ModelConfig {
    /// Extract model config from GGUF metadata.
    pub fn from_gguf(gguf: &GgufFile<'_>) -> Result<Self, &'static str> {
        let vocab_size = gguf
            .meta_u32("llama.vocab_size")
            .or_else(|| gguf.meta_u32("general.vocab_size"))
            .ok_or("GGUF: missing vocab_size")? as usize;

        let hidden_dim = gguf
            .meta_u32("llama.embedding_length")
            .ok_or("GGUF: missing embedding_length")? as usize;

        let intermediate_dim = gguf
            .meta_u32("llama.feed_forward_length")
            .ok_or("GGUF: missing feed_forward_length")? as usize;

        let n_layers = gguf
            .meta_u32("llama.block_count")
            .ok_or("GGUF: missing block_count")? as usize;

        let n_heads = gguf
            .meta_u32("llama.attention.head_count")
            .ok_or("GGUF: missing head_count")? as usize;

        let n_kv_heads = gguf
            .meta_u32("llama.attention.head_count_kv")
            .unwrap_or(n_heads as u32) as usize;

        let head_dim = hidden_dim / n_heads;

        let rope_theta = gguf
            .meta_f32("llama.rope.freq_base")
            .unwrap_or(10000.0);

        let norm_eps = gguf
            .meta_f32("llama.attention.layer_norm_rms_epsilon")
            .unwrap_or(1e-5);

        let max_seq_len = gguf
            .meta_u32("llama.context_length")
            .unwrap_or(2048) as usize;

        Ok(ModelConfig {
            vocab_size,
            hidden_dim,
            intermediate_dim,
            n_layers,
            n_heads,
            n_kv_heads,
            head_dim,
            rope_theta,
            norm_eps,
            max_seq_len,
        })
    }
}

/// Weights for a single transformer layer.
struct LayerWeights {
    /// Attention input norm weights
    attn_norm: Vec<f32>,
    /// Query projection: [n_heads * head_dim, hidden_dim]
    wq: Vec<f32>,
    /// Key projection: [n_kv_heads * head_dim, hidden_dim]
    wk: Vec<f32>,
    /// Value projection: [n_kv_heads * head_dim, hidden_dim]
    wv: Vec<f32>,
    /// Output projection: [hidden_dim, n_heads * head_dim]
    wo: Vec<f32>,
    /// FFN input norm weights
    ffn_norm: Vec<f32>,
    /// FFN gate projection (W1 in LLaMA): [intermediate_dim, hidden_dim]
    w1: Vec<f32>,
    /// FFN down projection (W2 in LLaMA): [hidden_dim, intermediate_dim]
    w2: Vec<f32>,
    /// FFN up projection (W3 in LLaMA): [intermediate_dim, hidden_dim]
    w3: Vec<f32>,
}

/// The full transformer model loaded into memory.
pub struct TransformerModel {
    pub config: ModelConfig,
    /// Token embedding table: [vocab_size, hidden_dim]
    token_emb: Vec<f32>,
    /// Per-layer weights
    layers: Vec<LayerWeights>,
    /// Final RMSNorm weights
    final_norm: Vec<f32>,
    /// Output projection (lm_head): [vocab_size, hidden_dim]
    /// If None, tied to token_emb
    output_weight: Option<Vec<f32>>,
}

/// Runtime state for autoregressive generation (KV-cache + scratch buffers).
pub struct InferenceState {
    /// Current token position in the sequence
    pos: usize,
    /// Hidden state vector
    x: Vec<f32>,
    /// Scratch buffers for intermediate computations
    xb: Vec<f32>,
    xb2: Vec<f32>,
    /// Query buffer
    q: Vec<f32>,
    /// Attention scores buffer
    att: Vec<f32>,
    /// FFN hidden buffer (gate)
    hb: Vec<f32>,
    /// FFN hidden buffer (up)
    hb2: Vec<f32>,
    /// Output logits
    logits: Vec<f32>,
    /// KV-cache: key vectors per layer, per position
    /// Shape: [n_layers][max_seq_len * n_kv_heads * head_dim]
    key_cache: Vec<Vec<f32>>,
    /// KV-cache: value vectors per layer, per position
    val_cache: Vec<Vec<f32>>,
}

impl TransformerModel {
    /// Load a transformer model from a parsed GGUF file.
    ///
    /// Reads all tensor data and dequantizes to f32.
    /// This can use a lot of memory — a 1B parameter model in f32
    /// uses ~4GB RAM. Quantized models (Q4_0, Q8_0) are much smaller.
    pub fn from_gguf(gguf: &GgufFile<'_>) -> Result<Self, &'static str> {
        let config = ModelConfig::from_gguf(gguf)?;

        // Load token embeddings
        let token_emb = load_tensor(gguf, "token_embd.weight")?;

        // Load per-layer weights
        let mut layers = Vec::with_capacity(config.n_layers);
        for i in 0..config.n_layers {
            let prefix = alloc::format!("blk.{}", i);

            let layer = LayerWeights {
                attn_norm: load_tensor(gguf, &alloc::format!("{}.attn_norm.weight", prefix))?,
                wq: load_tensor(gguf, &alloc::format!("{}.attn_q.weight", prefix))?,
                wk: load_tensor(gguf, &alloc::format!("{}.attn_k.weight", prefix))?,
                wv: load_tensor(gguf, &alloc::format!("{}.attn_v.weight", prefix))?,
                wo: load_tensor(gguf, &alloc::format!("{}.attn_output.weight", prefix))?,
                ffn_norm: load_tensor(gguf, &alloc::format!("{}.ffn_norm.weight", prefix))?,
                w1: load_tensor(gguf, &alloc::format!("{}.ffn_gate.weight", prefix))?,
                w2: load_tensor(gguf, &alloc::format!("{}.ffn_down.weight", prefix))?,
                w3: load_tensor(gguf, &alloc::format!("{}.ffn_up.weight", prefix))?,
            };
            layers.push(layer);
        }

        // Load final norm
        let final_norm = load_tensor(gguf, "output_norm.weight")?;

        // Load output projection (may be tied to embeddings)
        let output_weight = load_tensor(gguf, "output.weight").ok();

        Ok(TransformerModel {
            config,
            token_emb,
            layers,
            final_norm,
            output_weight,
        })
    }

    /// Create an inference state with allocated buffers.
    pub fn new_state(&self) -> InferenceState {
        let c = &self.config;
        let kv_dim = c.n_kv_heads * c.head_dim;

        let mut key_cache = Vec::with_capacity(c.n_layers);
        let mut val_cache = Vec::with_capacity(c.n_layers);
        for _ in 0..c.n_layers {
            key_cache.push(vec![0.0f32; c.max_seq_len * kv_dim]);
            val_cache.push(vec![0.0f32; c.max_seq_len * kv_dim]);
        }

        InferenceState {
            pos: 0,
            x: vec![0.0; c.hidden_dim],
            xb: vec![0.0; c.hidden_dim],
            xb2: vec![0.0; c.hidden_dim],
            q: vec![0.0; c.n_heads * c.head_dim],
            att: vec![0.0; c.n_heads * c.max_seq_len],
            hb: vec![0.0; c.intermediate_dim],
            hb2: vec![0.0; c.intermediate_dim],
            logits: vec![0.0; c.vocab_size],
            key_cache,
            val_cache,
        }
    }

    /// Run one forward pass: given a token ID, produce logits over vocabulary.
    ///
    /// The KV-cache in `state` is updated with the new token's key/value vectors.
    /// Call this once per generated token.
    pub fn forward<'a>(&self, token: u32, state: &'a mut InferenceState) -> &'a [f32] {
        let c = &self.config;
        let pos = state.pos;
        let head_dim = c.head_dim;
        let kv_dim = c.n_kv_heads * head_dim;
        let kv_mul = c.n_heads / c.n_kv_heads; // GQA group size

        // 1. Token embedding lookup
        let emb_offset = token as usize * c.hidden_dim;
        if emb_offset + c.hidden_dim <= self.token_emb.len() {
            state.x.copy_from_slice(&self.token_emb[emb_offset..emb_offset + c.hidden_dim]);
        }

        // 2. Process each transformer layer
        for layer_idx in 0..c.n_layers {
            let layer = &self.layers[layer_idx];

            // 2a. Attention input norm (RMSNorm)
            tensor::rmsnorm(&mut state.xb, &state.x, &layer.attn_norm, c.norm_eps);

            // 2b. Compute Q, K, V projections
            tensor::matmul(&mut state.q, &layer.wq, &state.xb, c.hidden_dim);

            // K and V go into the KV-cache at the current position
            let cache_offset = pos * kv_dim;
            let k_slice = &mut state.key_cache[layer_idx][cache_offset..cache_offset + kv_dim];
            let v_slice = &mut state.val_cache[layer_idx][cache_offset..cache_offset + kv_dim];
            tensor::matmul(k_slice, &layer.wk, &state.xb, c.hidden_dim);
            tensor::matmul(v_slice, &layer.wv, &state.xb, c.hidden_dim);

            // 2c. Apply RoPE to Q and K (per head)
            for h in 0..c.n_heads {
                let q_head = &mut state.q[h * head_dim..(h + 1) * head_dim];
                tensor::rope_single(q_head, pos, head_dim, c.rope_theta);
            }
            for h in 0..c.n_kv_heads {
                let k_head = &mut state.key_cache[layer_idx]
                    [cache_offset + h * head_dim..cache_offset + (h + 1) * head_dim];
                tensor::rope_single(k_head, pos, head_dim, c.rope_theta);
            }

            // 2d. Multi-head attention with KV-cache
            // Clear xb for accumulating attention output
            for v in state.xb.iter_mut() {
                *v = 0.0;
            }

            for h in 0..c.n_heads {
                let kv_h = h / kv_mul; // GQA: which KV head this query head uses
                let q_head = &state.q[h * head_dim..(h + 1) * head_dim];

                // Compute attention scores for all cached positions
                let att_head = &mut state.att[h * c.max_seq_len..h * c.max_seq_len + pos + 1];
                let scale = 1.0 / tensor::sqrt_f32(head_dim as f32);

                for t in 0..=pos {
                    let k_offset = t * kv_dim + kv_h * head_dim;
                    let k_head = &state.key_cache[layer_idx][k_offset..k_offset + head_dim];
                    let mut score = 0.0f32;
                    for d in 0..head_dim {
                        score += q_head[d] * k_head[d];
                    }
                    att_head[t] = score * scale;
                }

                // Softmax over attention scores
                tensor::softmax(att_head);

                // Weighted sum of value vectors
                let xb_head = &mut state.xb[h * head_dim..(h + 1) * head_dim];
                for t in 0..=pos {
                    let v_offset = t * kv_dim + kv_h * head_dim;
                    let v_head = &state.val_cache[layer_idx][v_offset..v_offset + head_dim];
                    let a = att_head[t];
                    for d in 0..head_dim {
                        xb_head[d] += a * v_head[d];
                    }
                }
            }

            // 2e. Output projection
            tensor::matmul(&mut state.xb2, &layer.wo, &state.xb, c.n_heads * head_dim);

            // 2f. Residual connection
            for i in 0..c.hidden_dim {
                state.x[i] += state.xb2[i];
            }

            // 2g. FFN: norm → gate(W1) * up(W3) → down(W2)
            tensor::rmsnorm(&mut state.xb, &state.x, &layer.ffn_norm, c.norm_eps);

            // Gate and up projections
            tensor::matmul(&mut state.hb, &layer.w1, &state.xb, c.hidden_dim);
            tensor::matmul(&mut state.hb2, &layer.w3, &state.xb, c.hidden_dim);

            // SiLU activation on gate, then element-wise multiply with up
            tensor::silu(&mut state.hb);
            for i in 0..c.intermediate_dim {
                state.hb[i] *= state.hb2[i];
            }

            // Down projection
            tensor::matmul(&mut state.xb, &layer.w2, &state.hb, c.intermediate_dim);

            // Residual connection
            for i in 0..c.hidden_dim {
                state.x[i] += state.xb[i];
            }
        }

        // 3. Final RMSNorm
        tensor::rmsnorm(&mut state.xb, &state.x, &self.final_norm, c.norm_eps);

        // 4. Compute logits (output projection)
        let output_w = self.output_weight.as_deref().unwrap_or(&self.token_emb);
        tensor::matmul(&mut state.logits, output_w, &state.xb, c.hidden_dim);

        // Advance position
        state.pos += 1;

        &state.logits
    }

    /// Reset the inference state for a new sequence.
    pub fn reset_state(&self, state: &mut InferenceState) {
        state.pos = 0;
        // KV-cache is overwritten as we go, no need to zero it
    }
}

/// Sample a token from logits using temperature sampling.
///
/// temperature = 0.0: greedy (argmax)
/// temperature > 0.0: softmax with temperature scaling, then sample
pub fn sample_token(logits: &mut [f32], temperature: f32) -> u32 {
    if temperature <= 0.0 || temperature < 1e-6 {
        // Greedy: pick the highest logit
        let mut best_idx = 0u32;
        let mut best_val = logits[0];
        for (i, &v) in logits.iter().enumerate().skip(1) {
            if v > best_val {
                best_val = v;
                best_idx = i as u32;
            }
        }
        return best_idx;
    }

    // Apply temperature
    for v in logits.iter_mut() {
        *v /= temperature;
    }
    tensor::softmax(logits);

    // Sample from the probability distribution using RDRAND
    let random_u64 = rdrand_u64();
    let random_f32 = (random_u64 & 0xFFFFFF) as f32 / 0xFFFFFF as f32; // [0, 1]

    let mut cumulative = 0.0f32;
    for (i, &p) in logits.iter().enumerate() {
        cumulative += p;
        if cumulative >= random_f32 {
            return i as u32;
        }
    }

    // Fallback: return last token
    (logits.len() - 1) as u32
}

/// Load and dequantize a tensor from the GGUF file.
fn load_tensor(gguf: &GgufFile<'_>, name: &str) -> Result<Vec<f32>, &'static str> {
    let info = gguf
        .tensors
        .get(name)
        .ok_or("GGUF: missing tensor")?;
    let data = gguf
        .tensor_data(name)
        .ok_or("GGUF: tensor data out of bounds")?;
    Ok(tensor::load_f32_tensor(data, info.dtype, info.n_elements()))
}

/// Read a random u64 from RDRAND (for temperature sampling).
fn rdrand_u64() -> u64 {
    for _ in 0..10 {
        let mut val: u64 = 0;
        let success: u8;
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc {success}",
                val = out(reg) val,
                success = out(reg_byte) success,
            );
        }
        if success != 0 {
            return val;
        }
        core::hint::spin_loop();
    }
    // Fallback — should never happen
    0x42424242_42424242
}

/// Generate text from a transformer model.
///
/// Takes a list of prompt token IDs, runs the model forward for each,
/// then generates new tokens up to `max_tokens`.
///
/// Returns the generated token IDs (not including the prompt).
pub fn generate(
    model: &TransformerModel,
    state: &mut InferenceState,
    prompt_tokens: &[u32],
    max_tokens: usize,
    temperature: f32,
    stop_tokens: &[u32],
) -> Vec<u32> {
    model.reset_state(state);

    let mut output_tokens = Vec::new();

    // Process prompt tokens (prefill)
    for &token in prompt_tokens {
        model.forward(token, state);
    }

    // Generate new tokens
    let mut next_token = if prompt_tokens.is_empty() {
        1 // BOS token as fallback
    } else {
        // Sample from the last forward pass
        let logits = &mut state.logits.clone();
        sample_token(logits, temperature)
    };

    for _ in 0..max_tokens {
        if stop_tokens.contains(&next_token) {
            break;
        }

        output_tokens.push(next_token);

        if state.pos >= model.config.max_seq_len {
            break; // Hit context length limit
        }

        let logits = model.forward(next_token, state);
        let mut logits_copy = logits.to_vec();
        next_token = sample_token(&mut logits_copy, temperature);
    }

    output_tokens
}
