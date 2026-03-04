//! Kernel timer — tracks elapsed time since boot via the APIC timer.
//!
//! The APIC timer fires at 100 Hz (10ms per tick). This module provides
//! monotonic time queries and a busy-wait sleep for early kernel use.
//! In Phase 05, the SART scheduler will hook into the timer interrupt
//! to perform preemptive scheduling.

use core::sync::atomic::{AtomicU64, Ordering};

/// Incremented by the timer interrupt handler on every tick (100 Hz).
pub static TICK: AtomicU64 = AtomicU64::new(0);

/// Number of timer ticks since boot (100 Hz = 10ms per tick).
pub fn ticks() -> u64 {
    TICK.load(Ordering::Relaxed)
}

/// Approximate milliseconds since boot.
pub fn milliseconds() -> u64 {
    ticks() * 10
}

/// Approximate nanoseconds since boot.
pub fn nanoseconds() -> u64 {
    ticks() * 10_000_000 // 10ms per tick = 10,000,000 ns
}

/// Busy-wait for approximately `ms` milliseconds.
/// Uses HLT to save power while waiting for the next timer tick.
pub fn sleep_ms(ms: u64) {
    let target = milliseconds() + ms;
    while milliseconds() < target {
        x86_64::instructions::hlt();
    }
}
