//! Local inference backend — runs GGUF models on bare-metal CPU.
//!
//! This backend loads a GGUF model file into memory, parses it using our
//! pure-Rust GGUF parser, builds the transformer model, and runs the full
//! forward pass for text generation — all without any C dependencies.
//!
//! Model loading:
//!   The model must be provided as a byte slice (loaded from SVFS or
//!   embedded at build time). In the future, SVFS objects tagged with
//!   "ai-model" and "gguf" will be automatically discovered.
//!
//! Supported architectures:
//!   - LLaMA-family (LLaMA 2, LLaMA 3, TinyLlama, Mistral, etc.)
//!   - Any model using the standard GGUF tensor naming convention
//!
//! Supported quantization types:
//!   - F32 (full precision, largest)
//!   - F16 (half precision)
//!   - Q8_0 (8-bit quantized, good quality/size tradeoff)
//!   - Q4_0 (4-bit quantized, smallest but lower quality)

use alloc::{format, string::String};

use crate::backend::{
    FinishReason, InferenceBackend, InferenceError, InferenceRequest, InferenceResponse,
};
use crate::gguf::GgufFile;
use crate::tokenizer::Tokenizer;
use crate::transformer::{self, InferenceState, TransformerModel};

/// Local inference backend using pure-Rust GGUF inference.
pub struct LocalInferenceBackend {
    /// The loaded transformer model (weights in memory)
    model: TransformerModel,
    /// The tokenizer loaded from GGUF metadata
    tokenizer: Tokenizer,
    /// Pre-allocated inference state (KV-cache + scratch buffers)
    state: InferenceState,
    /// Model name (from GGUF metadata)
    model_name: String,
}

impl LocalInferenceBackend {
    /// Attempt to load a local model.
    ///
    /// Currently returns NoLocalModel since no model file is embedded or
    /// loaded from SVFS yet. Once a model is available (via SVFS or
    /// build-time embedding), this will parse the GGUF and initialize
    /// the transformer.
    pub fn try_load() -> Result<Self, InferenceError> {
        // In the future, this will:
        // 1. Query SVFS for objects tagged "ai-model" + "gguf"
        // 2. Load the model bytes
        // 3. Call Self::from_gguf_bytes()
        //
        // For now, no model is embedded, so we return NoLocalModel.
        // The InferenceRouter will fall through to the cloud API backend.
        Err(InferenceError::NoLocalModel)
    }

    /// Load a model from raw GGUF bytes.
    ///
    /// This is the actual model loading path. Call this with model data
    /// loaded from SVFS or embedded at build time.
    pub fn from_gguf_bytes(data: &[u8]) -> Result<Self, InferenceError> {
        // Parse the GGUF file
        let gguf = GgufFile::parse(data).map_err(|e| {
            crate::println!("[Inference] GGUF parse error: {}", e);
            InferenceError::ModelLoadFailed
        })?;

        // Extract model name from metadata
        let model_name: String = gguf
            .meta_str("general.name")
            .unwrap_or("unknown")
            .into();

        let arch = gguf
            .meta_str("general.architecture")
            .unwrap_or("unknown");

        crate::println!(
            "[Inference] Loading local model: {} (arch: {}, {} tensors)",
            model_name,
            arch,
            gguf.tensors.len()
        );

        // Load the tokenizer from GGUF metadata
        let tokenizer = Tokenizer::from_gguf(&gguf).map_err(|e| {
            crate::println!("[Inference] Tokenizer load error: {}", e);
            InferenceError::ModelLoadFailed
        })?;

        crate::println!(
            "[Inference] Tokenizer loaded: {} tokens, BOS={}, EOS={}",
            tokenizer.vocab_size(),
            tokenizer.bos_id,
            tokenizer.eos_id
        );

        // Build the transformer model (loads and dequantizes all weights)
        let model = TransformerModel::from_gguf(&gguf).map_err(|e| {
            crate::println!("[Inference] Model load error: {}", e);
            InferenceError::ModelLoadFailed
        })?;

        crate::println!(
            "[Inference] Model loaded: {} layers, {} hidden, {} heads, {} vocab",
            model.config.n_layers,
            model.config.hidden_dim,
            model.config.n_heads,
            model.config.vocab_size,
        );

        // Allocate inference state (KV-cache + scratch buffers)
        let state = model.new_state();

        Ok(LocalInferenceBackend {
            model,
            tokenizer,
            state,
            model_name,
        })
    }
}

impl InferenceBackend for LocalInferenceBackend {
    fn generate(
        &mut self,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        let start_ms = kernel_ms();

        // Tokenize the prompt
        let mut prompt_tokens = alloc::vec![self.tokenizer.bos_id];
        prompt_tokens.extend(self.tokenizer.encode(&request.prompt));

        crate::println!(
            "[Inference] Local: {} prompt tokens, max_tokens={}",
            prompt_tokens.len(),
            request.max_tokens,
        );

        // Tokenize stop sequences
        let mut stop_token_ids = alloc::vec![self.tokenizer.eos_id];
        for seq in &request.stop_sequences {
            let encoded = self.tokenizer.encode(seq);
            if encoded.len() == 1 {
                stop_token_ids.push(encoded[0]);
            }
        }

        // Run the transformer forward pass
        let output_tokens = transformer::generate(
            &self.model,
            &mut self.state,
            &prompt_tokens,
            request.max_tokens,
            request.temperature,
            &stop_token_ids,
        );

        // Decode output tokens to text
        let text = self.tokenizer.decode(&output_tokens);
        let tokens_generated = output_tokens.len();
        let latency = kernel_ms() - start_ms;

        // Determine finish reason
        let finish_reason = if tokens_generated >= request.max_tokens {
            FinishReason::MaxTokens
        } else if output_tokens
            .last()
            .map(|t| stop_token_ids.contains(t))
            .unwrap_or(false)
        {
            FinishReason::StopSequence
        } else {
            FinishReason::EndOfSequence
        };

        crate::println!(
            "[Inference] Local: generated {} tokens in {}ms",
            tokens_generated,
            latency,
        );

        Ok(InferenceResponse {
            text,
            tokens_generated,
            finish_reason,
            backend_used: format!("local ({})", self.model_name),
            latency_ms: latency,
        })
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn is_available(&self) -> bool {
        true
    }
}

fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
