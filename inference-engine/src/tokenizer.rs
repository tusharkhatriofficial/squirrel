//! BPE tokenizer — reads vocabulary from GGUF metadata.
//!
//! GGUF files embed the tokenizer vocabulary directly in metadata:
//!   - tokenizer.ggml.tokens: array of token strings
//!   - tokenizer.ggml.scores: array of merge priority scores
//!   - tokenizer.ggml.token_type: array of token types (normal, control, etc.)
//!   - tokenizer.ggml.bos_token_id: beginning-of-sequence token
//!   - tokenizer.ggml.eos_token_id: end-of-sequence token
//!
//! This tokenizer implements SentencePiece-style BPE (Byte Pair Encoding),
//! which is what LLaMA, Mistral, and TinyLlama use.
//!
//! BPE algorithm:
//!   1. Start with each byte/character as a separate token
//!   2. Find the pair of adjacent tokens with highest merge score
//!   3. Merge that pair into a single token
//!   4. Repeat until no more merges can be made

use alloc::{string::String, vec::Vec};
use crate::gguf::{GgufFile, MetadataValue};

/// A loaded tokenizer vocabulary.
pub struct Tokenizer {
    /// Token strings indexed by token ID
    vocab: Vec<String>,
    /// Merge priority scores (higher = merge earlier)
    scores: Vec<f32>,
    /// Beginning-of-sequence token ID
    pub bos_id: u32,
    /// End-of-sequence token ID
    pub eos_id: u32,
}

impl Tokenizer {
    /// Load the tokenizer from GGUF metadata.
    pub fn from_gguf(gguf: &GgufFile<'_>) -> Result<Self, &'static str> {
        // Read token strings
        let tokens_meta = gguf
            .metadata
            .get("tokenizer.ggml.tokens")
            .ok_or("GGUF: missing tokenizer.ggml.tokens")?;

        let vocab = match tokens_meta {
            MetadataValue::Array(arr) => {
                let mut v = Vec::with_capacity(arr.len());
                for item in arr {
                    match item {
                        MetadataValue::String(s) => v.push(s.clone()),
                        _ => v.push(String::new()),
                    }
                }
                v
            }
            _ => return Err("GGUF: tokenizer.ggml.tokens is not an array"),
        };

        // Read scores
        let scores_meta = gguf.metadata.get("tokenizer.ggml.scores");
        let scores = match scores_meta {
            Some(MetadataValue::Array(arr)) => {
                let mut s = Vec::with_capacity(arr.len());
                for item in arr {
                    match item {
                        MetadataValue::F32(v) => s.push(*v),
                        _ => s.push(0.0),
                    }
                }
                s
            }
            _ => {
                // No scores — use sequential values (lower ID = higher priority)
                (0..vocab.len()).map(|i| -(i as f32)).collect()
            }
        };

        // Read special token IDs
        let bos_id = gguf.meta_u32("tokenizer.ggml.bos_token_id").unwrap_or(1);
        let eos_id = gguf.meta_u32("tokenizer.ggml.eos_token_id").unwrap_or(2);

        Ok(Tokenizer {
            vocab,
            scores,
            bos_id,
            eos_id,
        })
    }

    /// Encode a text string into token IDs using BPE.
    ///
    /// The algorithm:
    ///   1. Convert input to UTF-8 bytes
    ///   2. Look up each byte as a single-byte token
    ///   3. Iteratively merge the highest-scoring adjacent pair
    ///   4. Return the final token sequence
    pub fn encode(&self, text: &str) -> Vec<u32> {
        if text.is_empty() {
            return Vec::new();
        }

        // Step 1: Initialize with single-character tokens
        let mut tokens: Vec<u32> = Vec::new();

        for ch in text.chars() {
            // Try to find the character as a token
            let ch_str: String = alloc::format!("{}", ch);
            if let Some(id) = self.find_token(&ch_str) {
                tokens.push(id);
            } else {
                // Fall back to byte-level tokens
                let mut buf = [0u8; 4];
                let bytes = ch.encode_utf8(&mut buf).as_bytes();
                for &b in bytes {
                    // Look for byte token like "<0x41>"
                    let byte_token = alloc::format!("<0x{:02X}>", b);
                    if let Some(id) = self.find_token(&byte_token) {
                        tokens.push(id);
                    }
                    // If byte token not found, skip (shouldn't happen with proper vocab)
                }
            }
        }

        // Step 2: Iteratively merge the best pair
        loop {
            let mut best_score = f32::NEG_INFINITY;
            let mut best_idx = usize::MAX;
            let mut best_token_id = 0u32;

            // Find the highest-scoring mergeable pair
            for i in 0..tokens.len().saturating_sub(1) {
                let merged = alloc::format!(
                    "{}{}",
                    self.token_str(tokens[i]),
                    self.token_str(tokens[i + 1])
                );
                if let Some(id) = self.find_token(&merged) {
                    let score = if (id as usize) < self.scores.len() {
                        self.scores[id as usize]
                    } else {
                        0.0
                    };
                    if score > best_score {
                        best_score = score;
                        best_idx = i;
                        best_token_id = id;
                    }
                }
            }

            if best_idx == usize::MAX {
                break; // No more merges possible
            }

            // Apply the merge: replace tokens[best_idx] and tokens[best_idx+1]
            // with the merged token
            tokens[best_idx] = best_token_id;
            tokens.remove(best_idx + 1);
        }

        tokens
    }

    /// Decode a sequence of token IDs back into text.
    pub fn decode(&self, tokens: &[u32]) -> String {
        let mut result = String::new();
        for &id in tokens {
            let s = self.token_str(id);
            // Handle SentencePiece-style space encoding
            // The ▁ character (U+2581) represents a space
            for ch in s.chars() {
                if ch == '\u{2581}' {
                    result.push(' ');
                } else {
                    result.push(ch);
                }
            }
        }
        // Trim leading space (SentencePiece adds one before the first word)
        if result.starts_with(' ') {
            result.remove(0);
        }
        result
    }

    /// Decode a single token ID to its string representation.
    pub fn token_str(&self, id: u32) -> &str {
        if (id as usize) < self.vocab.len() {
            &self.vocab[id as usize]
        } else {
            ""
        }
    }

    /// Find a token string in the vocabulary, returning its ID.
    fn find_token(&self, s: &str) -> Option<u32> {
        // Linear search — O(vocab_size) per lookup
        // For production, this should be a HashMap, but BTreeMap or linear
        // search works for the MVP with typical vocab sizes of 32K-128K.
        for (i, token) in self.vocab.iter().enumerate() {
            if token == s {
                return Some(i as u32);
            }
        }
        None
    }

    /// Vocabulary size.
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }
}
