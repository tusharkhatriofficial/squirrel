#![no_std]
extern crate alloc;

pub mod intent;

pub use intent::{Intent, IntentId, IntentPriority, SemanticType};
