//! Cryptographic random number generator using x86_64 RDRAND instruction.
//!
//! RDRAND is a hardware random number generator built into Intel (Ivy Bridge+)
//! and AMD (Zen+) processors. It draws from an on-chip entropy source and
//! passes NIST SP 800-90A DRBG tests.
//!
//! This is used by embedded-tls for TLS 1.3 key generation, nonces, and
//! other cryptographic operations that require secure randomness.

use rand_core::{CryptoRng, Error, RngCore};

/// Hardware-backed cryptographic RNG using x86_64 RDRAND.
pub struct RdRandRng;

impl RdRandRng {
    /// Read a random u64 from the hardware RNG.
    ///
    /// Retries up to 10 times if RDRAND signals a transient failure
    /// (carry flag not set). Panics if all retries fail, which would
    /// indicate a serious hardware issue.
    fn rdrand64() -> u64 {
        for _ in 0..10 {
            let mut val: u64 = 0;
            let success: u8;
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {success}",
                    val = out(reg) val,
                    success = out(reg_byte) success,
                );
            }
            if success != 0 {
                return val;
            }
            // RDRAND can transiently fail; retry
            core::hint::spin_loop();
        }
        // Hardware RNG failure — should never happen on real hardware
        panic!("RDRAND failed after 10 retries");
    }
}

impl RngCore for RdRandRng {
    fn next_u32(&mut self) -> u32 {
        Self::rdrand64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        Self::rdrand64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut offset = 0;
        while offset < dest.len() {
            let val = Self::rdrand64();
            let bytes = val.to_le_bytes();
            let remaining = dest.len() - offset;
            let to_copy = remaining.min(8);
            dest[offset..offset + to_copy].copy_from_slice(&bytes[..to_copy]);
            offset += to_copy;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// RDRAND is a NIST-certified hardware entropy source, safe for cryptographic use.
impl CryptoRng for RdRandRng {}
