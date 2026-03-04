//! Intent Bus — the central message routing system.
//!
//! The Intent Bus is Squirrel's replacement for syscalls, IPC, and event systems.
//! Components register as subscribers with semantic type patterns, then send and
//! receive Intents through BusConnection handles. Every intent is recorded in the
//! Glass Box audit log.
//!
//! Design:
//! - Single global static instance (INTENT_BUS) — no initialization needed
//! - Subscribers have bounded inboxes (64 intents) — drops oldest if full
//! - Routing uses exact match + prefix matching on semantic types
//! - Thread-safe via spin locks (kernel has no std mutexes)

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, RwLock};

use crate::audit::AuditLog;
use crate::intent::{Intent, IntentId};

/// Global monotonic intent ID counter — every intent gets a unique ID.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Time source — updated by the kernel timer. Returns milliseconds since boot.
static CURRENT_MS: AtomicU64 = AtomicU64::new(0);

/// Maximum intents queued per subscriber inbox.
const INBOX_CAPACITY: usize = 64;

/// Get current milliseconds since boot.
pub fn current_ms() -> u64 {
    CURRENT_MS.load(Ordering::Relaxed)
}

/// Set the current time (called by kernel timer to keep timestamps accurate).
pub fn set_current_ms(ms: u64) {
    CURRENT_MS.store(ms, Ordering::Relaxed);
}

/// A subscriber registered on the bus.
struct Subscriber {
    /// The semantic type patterns this subscriber listens to.
    /// Matching: exact match OR prefix match (e.g. "display" matches "display.render").
    subscriptions: Vec<String>,
    /// Inbox queue for this subscriber.
    inbox: Mutex<VecDeque<Intent>>,
    /// Human-readable name (for audit and lookup).
    name: String,
}

/// The kernel-native Intent Bus.
pub struct IntentBus {
    subscribers: RwLock<Vec<Subscriber>>,
    audit: Mutex<AuditLog>,
}

impl IntentBus {
    pub const fn new() -> Self {
        Self {
            subscribers: RwLock::new(Vec::new()),
            audit: Mutex::new(AuditLog::new()),
        }
    }

    /// Register a new subscriber. Returns a `BusConnection` for sending/receiving.
    ///
    /// `subscriptions` is a list of semantic type patterns to listen for.
    /// A subscriber with pattern "display" will receive intents of type
    /// "display", "display.render", "display.clear", etc.
    pub fn connect(&self, name: &str, subscriptions: &[&str]) -> BusConnection {
        let mut subs = self.subscribers.write();
        subs.push(Subscriber {
            subscriptions: subscriptions.iter().map(|s| String::from(*s)).collect(),
            inbox: Mutex::new(VecDeque::with_capacity(INBOX_CAPACITY)),
            name: String::from(name),
        });

        BusConnection {
            sender_name: String::from(name),
        }
    }

    /// Send an intent to all matching subscribers. Returns the assigned IntentId.
    ///
    /// The intent's `id` and `timestamp_ms` are set here (not by the caller).
    /// The intent is cloned to each matching subscriber's inbox and recorded
    /// in the audit log.
    pub fn send(&self, mut intent: Intent) -> IntentId {
        intent.id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        intent.timestamp_ms = current_ms();

        // Record in Glass Box audit log
        self.audit.lock().record(&intent);

        // Deliver to all matching subscribers
        let subs = self.subscribers.read();
        for sub in subs.iter() {
            let matches = sub.subscriptions.iter().any(|pat| {
                intent.semantic_type.matches(pat)
            });
            if matches {
                let mut inbox = sub.inbox.lock();
                if inbox.len() >= INBOX_CAPACITY {
                    inbox.pop_front(); // Drop oldest if full
                }
                inbox.push_back(intent.clone());
            }
        }

        intent.id
    }

    /// Try to receive an intent for the named subscriber (non-blocking).
    pub fn try_recv(&self, subscriber_name: &str) -> Option<Intent> {
        let subs = self.subscribers.read();
        for sub in subs.iter() {
            if sub.name == subscriber_name {
                return sub.inbox.lock().pop_front();
            }
        }
        None
    }

    /// Dump the last N audit entries (for Glass Box inspection).
    pub fn audit_snapshot(&self, n: usize) -> Vec<crate::audit::AuditEntry> {
        self.audit.lock().last(n)
    }
}

/// Handle returned to a registered component — used to send and receive intents.
///
/// Each BusConnection is associated with a subscriber name. Sending fills in
/// the sender field automatically. Receiving polls the subscriber's inbox.
#[derive(Clone)]
pub struct BusConnection {
    sender_name: String,
}

impl BusConnection {
    /// Send an intent onto the bus. The sender name is set automatically.
    pub fn send(&self, mut intent: Intent) -> IntentId {
        intent.sender = self.sender_name.clone();
        INTENT_BUS.send(intent)
    }

    /// Try to receive an intent (non-blocking). Returns None if inbox is empty.
    pub fn try_recv(&self) -> Option<Intent> {
        INTENT_BUS.try_recv(&self.sender_name)
    }

    /// Blocking receive — spins until an intent arrives.
    /// Uses spin_loop hint for power-efficient waiting.
    pub fn recv_blocking(&self) -> Intent {
        loop {
            if let Some(intent) = self.try_recv() {
                return intent;
            }
            core::hint::spin_loop();
        }
    }

    pub fn sender_name(&self) -> &str {
        &self.sender_name
    }
}

/// The global Intent Bus instance — no initialization needed.
pub static INTENT_BUS: IntentBus = IntentBus::new();
