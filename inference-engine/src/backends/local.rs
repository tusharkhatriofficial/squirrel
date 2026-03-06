//! Local inference backend — stub for Stage 2.
//!
//! The original plan called for embedding llama.cpp via C FFI to run GGUF
//! models directly on bare-metal x86_64. However, llama.cpp (like ring for
//! TLS) requires C standard library headers (assert.h, stdlib.h, math.h)
//! that don't exist on the x86_64-unknown-none target.
//!
//! This is the same fundamental problem we hit with TLS in Phase 09:
//! cross-compiling C code to freestanding bare metal requires a custom
//! freestanding C sysroot (clang --target=x86_64-elf with no libc).
//!
//! For the MVP, all inference goes through the cloud API backend. Local
//! inference will be added in Stage 2 via one of these approaches:
//!
//! 1. **Cross-compile llama.cpp** with a freestanding sysroot:
//!    Build a minimal C library that provides the math/memory functions
//!    llama.cpp needs (memcpy, malloc, sinf, expf, etc.) without POSIX.
//!
//! 2. **Pure-Rust inference** using the `candle` crate:
//!    candle (by Hugging Face) is a pure-Rust ML framework that can run
//!    transformer models without any C dependencies. It already supports
//!    no_std with some configuration.
//!
//! 3. **WASM inference module**:
//!    Compile an inference engine to WebAssembly and run it in the wasmi
//!    runtime that's already in the kernel (Phase 08). The WASM module
//!    gets its own memory and can include whatever C libs it needs.
//!
//! This stub implements the InferenceBackend trait but always returns
//! NoLocalModel, so the InferenceRouter knows to fall through to the
//! cloud API backend.

use alloc::string::String;

use crate::backend::{InferenceBackend, InferenceError, InferenceRequest, InferenceResponse};

/// Stub local inference backend.
///
/// Always reports as unavailable and returns NoLocalModel errors.
/// The InferenceRouter uses is_available() to skip this backend
/// and fall through to the cloud API.
pub struct LocalInferenceBackend {
    _private: (),
}

impl LocalInferenceBackend {
    /// Attempt to load a local model.
    ///
    /// Always returns Err(NoLocalModel) in the MVP stub. In Stage 2,
    /// this will scan SVFS for objects tagged "ai-model" and "gguf",
    /// load the model bytes, and initialize the inference runtime.
    pub fn try_load() -> Result<Self, InferenceError> {
        Err(InferenceError::NoLocalModel)
    }
}

impl InferenceBackend for LocalInferenceBackend {
    fn generate(
        &mut self,
        _request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        // This should never be called because is_available() returns false,
        // but if it somehow is, return a clear error.
        Err(InferenceError::NoLocalModel)
    }

    fn name(&self) -> &str {
        "local-stub"
    }

    fn is_available(&self) -> bool {
        false
    }
}
