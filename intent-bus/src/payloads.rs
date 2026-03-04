//! Common payload types for intents.
//!
//! These are the standard serializable payloads used across the system.
//! Components can define their own payload types too — any type that
//! implements serde Serialize/Deserialize can be sent through the bus.

use alloc::string::String;
use serde::{Deserialize, Serialize};

/// Empty payload for intents that carry no data (e.g. "system.shutdown").
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Empty {}

/// Generic string payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StringPayload {
    pub value: String,
}

/// Generic error payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorPayload {
    pub code: u32,
    pub message: String,
}

/// Key-value pair (used by settings, glass-box, etc.)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}
