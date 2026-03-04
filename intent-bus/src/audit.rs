//! Glass Box audit log — records every intent that passes through the bus.
//!
//! The audit log is the foundation of Squirrel's Glass Box Execution principle:
//! every action in the system is recorded and inspectable. The AI can review
//! its own decision history, and humans can trace any action back to its origin.
//!
//! Uses a ring buffer with fixed capacity (512 entries). When full, the oldest
//! entry is dropped. Entries are lightweight — they store metadata only, not
//! the full payload, to keep memory usage bounded.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::intent::{Intent, IntentId};

/// A single audit log entry — lightweight metadata snapshot of an intent.
/// Stores enough to reconstruct the "who sent what when" without keeping
/// the full payload in memory.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: IntentId,
    pub reply_to: Option<IntentId>,
    pub semantic_type: String,
    pub sender: String,
    pub timestamp_ms: u64,
    pub payload_len: usize,
}

impl AuditEntry {
    pub fn from_intent(intent: &Intent) -> Self {
        Self {
            id: intent.id,
            reply_to: intent.reply_to,
            semantic_type: intent.semantic_type.to_string(),
            sender: intent.sender.clone(),
            timestamp_ms: intent.timestamp_ms,
            payload_len: intent.payload.len(),
        }
    }
}

/// Ring-buffer audit log — keeps the last CAPACITY entries.
const CAPACITY: usize = 512;

pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record an intent in the audit log. Drops the oldest entry if at capacity.
    pub fn record(&mut self, intent: &Intent) {
        if self.entries.len() >= CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(AuditEntry::from_intent(intent));
    }

    /// Return the last `n` entries, most recent first.
    pub fn last(&self, n: usize) -> Vec<AuditEntry> {
        let start = self.entries.len().saturating_sub(n);
        let mut result = self.entries[start..].to_vec();
        result.reverse();
        result
    }

    /// Total number of entries currently in the log.
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }
}
