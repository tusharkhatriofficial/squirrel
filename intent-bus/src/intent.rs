//! Core intent types — the universal message format for all Squirrel IPC.
//!
//! Every component in Squirrel AIOS communicates exclusively through Intents.
//! An Intent carries a semantic type (what it represents), a serialized payload,
//! a priority level, and metadata for routing and audit. This replaces traditional
//! syscalls with a semantic messaging layer that AI can reason about.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Unique identifier for an intent (monotonically increasing, assigned by the bus).
pub type IntentId = u64;

/// Semantic type string — dot-separated namespaced name, e.g. "display.render".
/// This is the routing key: subscribers match on semantic types to receive intents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticType(pub String);

impl SemanticType {
    pub fn new(s: &str) -> Self {
        Self(String::from(s))
    }

    /// Check if this type matches a subscription pattern.
    /// Supports exact match and prefix matching (e.g. pattern "display" matches
    /// "display.render", "display.clear", etc.)
    pub fn matches(&self, pattern: &str) -> bool {
        self.0 == pattern || self.0.starts_with(&alloc::format!("{}.", pattern))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SemanticType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Priority level for intent delivery (higher = processed first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IntentPriority {
    Background = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl Default for IntentPriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// An Intent — the universal message type for all Squirrel IPC.
///
/// Intents are the Squirrel equivalent of syscalls + IPC messages + events,
/// unified into a single semantic abstraction. Every action in the system
/// is expressed as an intent, making the AI's reasoning fully transparent
/// through the Glass Box audit log.
#[derive(Debug, Clone)]
pub struct Intent {
    /// Unique ID assigned by the bus at send time
    pub id: IntentId,
    /// If this is a response, the ID of the request that triggered it
    pub reply_to: Option<IntentId>,
    /// Semantic type (what this intent represents)
    pub semantic_type: SemanticType,
    /// Name of the sender (for audit + routing)
    pub sender: String,
    /// Serialized payload (postcard-encoded binary)
    pub payload: Vec<u8>,
    /// Priority
    pub priority: IntentPriority,
    /// Timestamp (milliseconds since boot, set by the bus)
    pub timestamp_ms: u64,
}

impl Intent {
    /// Create a new outgoing intent (request).
    /// The `id` and `timestamp_ms` are set by the bus when the intent is sent.
    pub fn request<T: Serialize>(semantic_type: &str, sender: &str, payload: &T) -> Self {
        let payload = postcard::to_allocvec(payload).unwrap_or_default();
        Self {
            id: 0, // assigned by bus on send
            reply_to: None,
            semantic_type: SemanticType::new(semantic_type),
            sender: String::from(sender),
            payload,
            priority: IntentPriority::Normal,
            timestamp_ms: 0, // set by bus on send
        }
    }

    /// Create a response to a prior intent.
    /// The response semantic type is the original type with ".response" appended.
    pub fn response<T: Serialize>(origin: &Intent, sender: &str, payload: &T) -> Self {
        let response_type = alloc::format!("{}.response", origin.semantic_type.as_str());
        let mut intent = Self::request(&response_type, sender, payload);
        intent.reply_to = Some(origin.id);
        intent
    }

    /// Decode the payload back into a typed Rust struct.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> Result<T, postcard::Error> {
        postcard::from_bytes(&self.payload)
    }

    /// Set priority (builder pattern).
    pub fn with_priority(mut self, p: IntentPriority) -> Self {
        self.priority = p;
        self
    }
}
