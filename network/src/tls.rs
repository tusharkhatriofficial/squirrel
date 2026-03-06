//! TLS stub — placeholder for Stage 2.
//!
//! TLS (via rustls + ring) requires cross-compiling C code to x86_64-unknown-none,
//! which needs a freestanding C library (no libc on bare metal). This is a solvable
//! problem but not a priority for the MVP.
//!
//! For the MVP, we use plain HTTP over TCP. This works because:
//! - QEMU's user-mode networking can proxy to any host endpoint
//! - Local inference (llama.cpp in Phase 10) uses localhost HTTP
//! - For cloud AI APIs, the host machine can run a TLS-terminating proxy
//!
//! Stage 2 will add TLS by either:
//! 1. Cross-compiling ring with a freestanding sysroot (clang --target=x86_64-elf)
//! 2. Using a pure-Rust crypto backend (RustCrypto crates)
//! 3. Running TLS in a WASM capability module with its own crypto

// No code needed — the HTTP client talks directly to TCP sockets.
