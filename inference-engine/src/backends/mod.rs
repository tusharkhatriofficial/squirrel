//! Inference backend implementations.
//!
//! Each backend implements the `InferenceBackend` trait from `backend.rs`.
//! The InferenceRouter picks the active backend and dispatches requests.

pub mod api;
pub mod local;
