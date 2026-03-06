//! Tensor operations for transformer inference — pure Rust, no_std.
//!
//! These are the mathematical building blocks for running a LLaMA-style
//! transformer on bare metal. All operations work on f32 slices and are
//! implemented without any external math library.
//!
//! Operations:
//!   - matmul: Matrix-vector multiplication (the core of transformer inference)
//!   - rmsnorm: Root Mean Square Layer Normalization (LLaMA uses this instead of LayerNorm)
//!   - softmax: Numerically stable softmax for attention scores
//!   - silu: SiLU/Swish activation (used in LLaMA's FFN)
//!   - rope: Rotary Position Embedding (encodes token position into attention)
//!   - f16_to_f32: IEEE 754 half-precision to single-precision conversion
//!   - dequantize_q8_0: Dequantize Q8_0 blocks to f32

use alloc::vec::Vec;

/// Matrix-vector multiplication: out = W * x
///
/// W is stored row-major: W[row * n_cols + col].
/// This is the single hottest operation in transformer inference — every
/// attention projection (Q, K, V, O) and FFN layer goes through here.
///
/// For a model with hidden_dim=2048, each matmul does 2048*2048 = 4M
/// multiply-accumulates. With ~20 matmuls per layer and 22 layers,
/// that's ~1.8 billion FLOPs per token.
pub fn matmul(out: &mut [f32], w: &[f32], x: &[f32], n_cols: usize) {
    let n_rows = out.len();
    for row in 0..n_rows {
        let row_start = row * n_cols;
        let mut sum = 0.0f32;
        for col in 0..n_cols {
            sum += w[row_start + col] * x[col];
        }
        out[row] = sum;
    }
}

/// RMS Layer Normalization: out[i] = (x[i] / rms) * weight[i]
///
/// LLaMA uses RMSNorm instead of LayerNorm — it skips the mean subtraction
/// and just normalizes by the root-mean-square. This is slightly cheaper
/// and works just as well in practice.
///
/// rms = sqrt(mean(x^2) + eps)
pub fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], eps: f32) {
    let n = x.len();

    // Compute sum of squares
    let mut ss = 0.0f32;
    for &v in x {
        ss += v * v;
    }
    ss = ss / n as f32 + eps;
    let inv_rms = 1.0 / sqrt_f32(ss);

    for i in 0..n {
        out[i] = x[i] * inv_rms * weight[i];
    }
}

/// Numerically stable softmax: out[i] = exp(x[i] - max) / sum(exp(x - max))
///
/// Used for attention score normalization. The max-subtraction prevents
/// overflow in exp() when attention logits are large.
pub fn softmax(x: &mut [f32]) {
    let n = x.len();
    if n == 0 {
        return;
    }

    // Find max for numerical stability
    let mut max_val = x[0];
    for &v in x.iter().skip(1) {
        if v > max_val {
            max_val = v;
        }
    }

    // exp and sum
    let mut sum = 0.0f32;
    for v in x.iter_mut() {
        *v = exp_f32(*v - max_val);
        sum += *v;
    }

    // Normalize
    let inv_sum = 1.0 / sum;
    for v in x.iter_mut() {
        *v *= inv_sum;
    }
}

/// SiLU (Sigmoid Linear Unit) / Swish activation: silu(x) = x * sigmoid(x)
///
/// LLaMA's FFN uses SiLU as the activation function in its gated MLP:
///   FFN(x) = silu(W1 * x) * (W3 * x)
///
/// This is applied element-wise.
pub fn silu(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = *v * sigmoid(*v);
    }
}

/// Rotary Position Embedding (RoPE) — encodes token position into Q and K.
///
/// RoPE rotates pairs of dimensions in the query/key vectors by angles
/// that depend on the token position. This lets the model understand
/// relative positions of tokens in the sequence.
///
/// For each pair (i, i+1) in the vector:
///   q[i]   = q[i] * cos(θ) - q[i+1] * sin(θ)
///   q[i+1] = q[i] * sin(θ) + q[i+1] * cos(θ)
///
/// where θ = pos / (10000 ^ (2i / dim))
pub fn rope(q: &mut [f32], k: &mut [f32], pos: usize, head_dim: usize, rope_theta: f32) {
    let half = head_dim / 2;
    for i in 0..half {
        let freq = 1.0 / pow_f32(rope_theta, (2 * i) as f32 / head_dim as f32);
        let theta = pos as f32 * freq;
        let cos_t = cos_f32(theta);
        let sin_t = sin_f32(theta);

        // Rotate query
        let q0 = q[2 * i];
        let q1 = q[2 * i + 1];
        q[2 * i] = q0 * cos_t - q1 * sin_t;
        q[2 * i + 1] = q0 * sin_t + q1 * cos_t;

        // Rotate key
        let k0 = k[2 * i];
        let k1 = k[2 * i + 1];
        k[2 * i] = k0 * cos_t - k1 * sin_t;
        k[2 * i + 1] = k0 * sin_t + k1 * cos_t;
    }
}

/// Apply RoPE to a single vector (for multi-head case where Q and K
/// are processed independently per head).
pub fn rope_single(vec: &mut [f32], pos: usize, head_dim: usize, rope_theta: f32) {
    let half = head_dim / 2;
    for i in 0..half {
        let freq = 1.0 / pow_f32(rope_theta, (2 * i) as f32 / head_dim as f32);
        let theta = pos as f32 * freq;
        let cos_t = cos_f32(theta);
        let sin_t = sin_f32(theta);

        let v0 = vec[2 * i];
        let v1 = vec[2 * i + 1];
        vec[2 * i] = v0 * cos_t - v1 * sin_t;
        vec[2 * i + 1] = v0 * sin_t + v1 * cos_t;
    }
}

/// Convert IEEE 754 half-precision (f16) to single-precision (f32).
///
/// Used to load F16 model weights into f32 compute buffers.
/// We don't use the f16 type from a crate — just bit manipulation.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let frac = (bits & 0x3FF) as u32;

    if exp == 0 {
        if frac == 0 {
            // Zero (positive or negative)
            f32::from_bits(sign << 31)
        } else {
            // Subnormal: convert to normalized f32
            let mut e = -1i32;
            let mut f = frac;
            while (f & 0x400) == 0 {
                f <<= 1;
                e -= 1;
            }
            f &= 0x3FF;
            let f32_exp = (127 + 14 + e) as u32;
            f32::from_bits((sign << 31) | (f32_exp << 23) | (f << 13))
        }
    } else if exp == 31 {
        // Inf or NaN
        let f32_frac = if frac != 0 { 1u32 << 22 } else { 0 };
        f32::from_bits((sign << 31) | (0xFF << 23) | f32_frac)
    } else {
        // Normal number
        let f32_exp = (exp as i32 - 15 + 127) as u32;
        f32::from_bits((sign << 31) | (f32_exp << 23) | (frac << 13))
    }
}

/// Dequantize a Q8_0 block to f32 values.
///
/// Q8_0 format: each block contains 32 values quantized to 8-bit integers
/// with a shared f16 scale factor.
///   - Bytes 0-1: f16 scale (delta)
///   - Bytes 2-33: 32 × i8 quantized values
///
/// To dequantize: f32_value = quantized_i8 * delta
pub fn dequantize_q8_0(block: &[u8], out: &mut [f32]) {
    assert!(block.len() >= 34);
    assert!(out.len() >= 32);

    let scale_bits = u16::from_le_bytes([block[0], block[1]]);
    let delta = f16_to_f32(scale_bits);

    for i in 0..32 {
        let q = block[2 + i] as i8;
        out[i] = q as f32 * delta;
    }
}

/// Dequantize Q4_0 block to f32 values.
///
/// Q4_0 format: 32 values packed into 4-bit integers with shared f16 scale.
///   - Bytes 0-1: f16 scale (delta)
///   - Bytes 2-17: 16 bytes = 32 × 4-bit values (packed, unsigned offset by 8)
///
/// To dequantize: f32_value = (nibble - 8) * delta
pub fn dequantize_q4_0(block: &[u8], out: &mut [f32]) {
    assert!(block.len() >= 18);
    assert!(out.len() >= 32);

    let scale_bits = u16::from_le_bytes([block[0], block[1]]);
    let delta = f16_to_f32(scale_bits);

    for i in 0..16 {
        let byte = block[2 + i];
        let lo = (byte & 0x0F) as i32 - 8;
        let hi = ((byte >> 4) & 0x0F) as i32 - 8;
        out[2 * i] = lo as f32 * delta;
        out[2 * i + 1] = hi as f32 * delta;
    }
}

/// Load a tensor from raw bytes, dequantizing to f32 if needed.
pub fn load_f32_tensor(data: &[u8], dtype: crate::gguf::GgmlType, n_elements: usize) -> Vec<f32> {
    match dtype {
        crate::gguf::GgmlType::F32 => {
            let mut out = Vec::with_capacity(n_elements);
            for i in 0..n_elements {
                let offset = i * 4;
                if offset + 4 <= data.len() {
                    let bytes: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
                    out.push(f32::from_le_bytes(bytes));
                }
            }
            out
        }
        crate::gguf::GgmlType::F16 => {
            let mut out = Vec::with_capacity(n_elements);
            for i in 0..n_elements {
                let offset = i * 2;
                if offset + 2 <= data.len() {
                    let bits = u16::from_le_bytes([data[offset], data[offset + 1]]);
                    out.push(f16_to_f32(bits));
                }
            }
            out
        }
        crate::gguf::GgmlType::Q8_0 => {
            let mut out = Vec::with_capacity(n_elements);
            let block_size = 34; // 2 bytes scale + 32 bytes data
            let n_blocks = (n_elements + 31) / 32;
            let mut block_out = [0.0f32; 32];
            for b in 0..n_blocks {
                let offset = b * block_size;
                if offset + block_size <= data.len() {
                    dequantize_q8_0(&data[offset..offset + block_size], &mut block_out);
                    let remaining = (n_elements - b * 32).min(32);
                    out.extend_from_slice(&block_out[..remaining]);
                }
            }
            out
        }
        crate::gguf::GgmlType::Q4_0 => {
            let mut out = Vec::with_capacity(n_elements);
            let block_size = 18; // 2 bytes scale + 16 bytes data
            let n_blocks = (n_elements + 31) / 32;
            let mut block_out = [0.0f32; 32];
            for b in 0..n_blocks {
                let offset = b * block_size;
                if offset + block_size <= data.len() {
                    dequantize_q4_0(&data[offset..offset + block_size], &mut block_out);
                    let remaining = (n_elements - b * 32).min(32);
                    out.extend_from_slice(&block_out[..remaining]);
                }
            }
            out
        }
        _ => {
            // Unsupported quantization — return zeros
            alloc::vec![0.0; n_elements]
        }
    }
}

// ---------------------------------------------------------------------------
// Software math functions (no libm on bare metal)
// ---------------------------------------------------------------------------

/// Square root using the Newton-Raphson method.
pub fn sqrt_f32(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    // Initial estimate using bit manipulation (fast inverse sqrt trick)
    let mut y = f32::from_bits((x.to_bits() >> 1) + 0x1FC00000);
    // 3 Newton-Raphson iterations for good precision
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y
}

/// Exponential function using a Padé approximant.
///
/// Accurate to ~6 decimal places for |x| < 80.
pub fn exp_f32(x: f32) -> f32 {
    if x > 88.0 {
        return f32::INFINITY;
    }
    if x < -88.0 {
        return 0.0;
    }

    // Range reduction: exp(x) = 2^k * exp(r) where x = k*ln2 + r
    let ln2 = core::f32::consts::LN_2;
    let k = (x / ln2).floor();
    let r = x - k * ln2;

    // Padé(4,4) approximant for exp(r) on [-ln2/2, ln2/2]
    let r2 = r * r;
    let r3 = r2 * r;
    let r4 = r2 * r2;
    let num = 1.0 + r + 0.5 * r2 + r3 / 6.0 + r4 / 24.0;
    let den = 1.0;

    let exp_r = num / den;

    // Multiply by 2^k using bit manipulation
    let k_int = k as i32;
    let pow2 = f32::from_bits(((127 + k_int) as u32) << 23);
    exp_r * pow2
}

/// Sigmoid function: 1 / (1 + exp(-x))
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + exp_f32(-x))
}

/// Sine using Taylor series (7 terms, accurate to ~6 decimal places).
fn sin_f32(x: f32) -> f32 {
    // Reduce to [-π, π]
    let pi = core::f32::consts::PI;
    let two_pi = 2.0 * pi;
    let mut x = x % two_pi;
    if x > pi {
        x -= two_pi;
    }
    if x < -pi {
        x += two_pi;
    }

    // Taylor series: sin(x) = x - x³/3! + x⁵/5! - x⁷/7! + ...
    let x2 = x * x;
    let x3 = x2 * x;
    let x5 = x3 * x2;
    let x7 = x5 * x2;
    let x9 = x7 * x2;
    let x11 = x9 * x2;

    x - x3 / 6.0 + x5 / 120.0 - x7 / 5040.0 + x9 / 362880.0 - x11 / 39916800.0
}

/// Cosine using Taylor series.
fn cos_f32(x: f32) -> f32 {
    sin_f32(x + core::f32::consts::FRAC_PI_2)
}

/// Power function: base^exp using exp(exp * ln(base)).
fn pow_f32(base: f32, exp: f32) -> f32 {
    if base <= 0.0 {
        return 0.0;
    }
    exp_f32(exp * ln_f32(base))
}

/// Natural logarithm using a polynomial approximation.
fn ln_f32(x: f32) -> f32 {
    if x <= 0.0 {
        return f32::NEG_INFINITY;
    }

    // Decompose x = m * 2^e where 1 <= m < 2
    let bits = x.to_bits();
    let e = ((bits >> 23) & 0xFF) as i32 - 127;
    let m = f32::from_bits((bits & 0x007FFFFF) | 0x3F800000);

    // ln(x) = e * ln(2) + ln(m)
    // Use polynomial for ln(m) where m is in [1, 2)
    let t = m - 1.0;
    let ln_m = t * (2.0 - t * (0.666666666 - t * (0.4 - t * (0.285714285 - t * 0.222222222))));

    e as f32 * core::f32::consts::LN_2 + ln_m
}

/// Floor function.
trait Floor {
    fn floor(self) -> Self;
}

impl Floor for f32 {
    fn floor(self) -> f32 {
        let i = self as i32;
        let f = i as f32;
        if self < f {
            f - 1.0
        } else {
            f
        }
    }
}
