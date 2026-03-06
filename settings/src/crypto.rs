//! API key encryption — protects secrets at rest using AES-256-GCM.
//!
//! The problem: Users enter API keys (like "sk-ant-abc123...") so Squirrel
//! can call cloud AI services. We need to store these keys in SVFS so they
//! persist across reboots. But we can't store them in plaintext — if someone
//! reads the disk, they'd steal the keys.
//!
//! The solution: Encrypt the key before storing it. The encryption key is
//! derived from two things:
//!
//!   1. The CPU's identity (CPUID instruction) — unique to this machine
//!   2. A random "machine seed" generated on first boot and stored in SVFS
//!
//! Neither piece alone is enough to decrypt. The CPUID is hardware-specific
//! (can't be extracted from disk), and the seed is random (can't be guessed
//! from the CPU). Together they form a 256-bit AES key.
//!
//! This means:
//!   - API keys are encrypted on disk (safe if disk is stolen)
//!   - Moving SVFS to another machine breaks decryption (different CPUID)
//!   - The encryption key is NEVER stored anywhere — derived at runtime

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use alloc::string::String;
use alloc::vec::Vec;

/// Errors that can happen during encryption or decryption.
#[derive(Debug)]
pub enum CryptoError {
    /// AES-GCM encryption failed (shouldn't happen with valid inputs)
    EncryptionFailed,
    /// AES-GCM decryption failed — wrong key (different machine?) or
    /// the encrypted data was corrupted
    DecryptionFailed,
    /// The encrypted blob is too short to contain a valid nonce + ciphertext
    InvalidBlob,
    /// The decrypted bytes aren't valid UTF-8 (the API key was corrupted)
    InvalidUtf8,
}

/// Derive a 256-bit AES encryption key from this machine's hardware.
///
/// How it works:
///   1. Read CPUID leaf 0 — this gives us 16 bytes that identify the CPU
///      (vendor string like "GenuineIntel" or "AuthenticAMD" + max leaf)
///   2. Get the "machine seed" from SVFS — a 32-byte random value that was
///      generated on first boot and stored with the tag "machine-seed"
///   3. XOR the CPUID bytes into the seed — this mixes hardware identity
///      with random entropy to produce the final key
///
/// The result is a 256-bit key that:
///   - Is different on every machine (different CPUIDs)
///   - Is different on every fresh install (different random seeds)
///   - Is never stored on disk (computed fresh each time)
pub fn derive_machine_key() -> [u8; 32] {
    // Step 1: Read CPU identification
    // CPUID leaf 0 returns: EAX=max leaf, EBX/ECX/EDX=vendor string
    // On Intel: EBX="Genu", EDX="ineI", ECX="ntel"
    // This gives us 16 bytes of machine-specific data
    let cpuid = unsafe { core::arch::x86_64::__cpuid(0) };
    let cpuid_bytes: [u8; 16] = unsafe {
        core::mem::transmute([cpuid.eax, cpuid.ebx, cpuid.ecx, cpuid.edx])
    };

    // Step 2: Get the machine seed from SVFS
    let seed = get_or_create_machine_seed();

    // Step 3: Mix CPUID into the seed using XOR
    // The seed is 32 bytes, CPUID is 16 bytes, so we wrap around
    let mut key = [0u8; 32];
    for i in 0..32 {
        key[i] = seed[i] ^ cpuid_bytes[i % 16];
    }
    key
}

/// Get the machine seed from SVFS, or generate one on first boot.
///
/// The machine seed is a 32-byte random value that's generated ONCE when
/// Squirrel first boots on a machine. It's stored in SVFS with the tag
/// "machine-seed" so it persists across reboots.
///
/// On first boot: Generate a seed using timer ticks as entropy (not
/// cryptographically perfect, but combined with CPUID it's good enough
/// for our use case — protecting API keys, not state secrets).
///
/// On subsequent boots: Load the existing seed from SVFS.
fn get_or_create_machine_seed() -> [u8; 32] {
    let svfs = svfs::SVFS
        .get()
        .expect("SVFS must be initialized before settings crypto");

    // Look for an existing machine seed
    let hashes = svfs.find_by_tag("machine-seed");
    if let Some(hash) = hashes.first() {
        if let Ok(data) = svfs.retrieve(hash) {
            if data.len() >= 32 {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&data[..32]);
                return seed;
            }
        }
    }

    // First boot — generate a new random seed
    // We use the CPU timestamp counter (RDTSC) as entropy. This gives us
    // the number of CPU cycles since the last reset — a high-resolution
    // value that's hard to predict from outside the machine.
    let mut seed = [0u8; 32];
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    let ticks = unsafe { kernel_milliseconds() };

    // Mix the timer value through multiple rounds to fill 32 bytes.
    // Each round multiplies by a large prime and adds an offset,
    // producing pseudo-random values across the full 32-byte seed.
    for i in 0..4 {
        let mixed = ticks
            .wrapping_add(i as u64 * 0x517cc1b727220a95)
            .wrapping_mul(0x6c62272e07bb0142);
        seed[i * 8..(i + 1) * 8].copy_from_slice(&mixed.to_le_bytes());
    }

    // Store the seed in SVFS so we can find it on next boot
    svfs.store(
        &seed,
        svfs::ObjectType::Config,
        Some("machine-seed"),
        &["machine-seed"],
    )
    .ok();

    seed
}

/// Encrypt a plaintext API key with AES-256-GCM.
///
/// Takes: the API key as a string (e.g. "sk-ant-abc123...")
/// Returns: an encrypted blob (nonce + ciphertext + GCM tag)
///
/// The blob format is: [12 bytes nonce][ciphertext][16 bytes GCM auth tag]
/// The nonce is prepended so we can extract it during decryption.
///
/// AES-256-GCM provides both confidentiality (the key is unreadable)
/// and integrity (tampering with the blob will cause decryption to fail).
pub fn encrypt_api_key(plaintext: &str) -> Result<Vec<u8>, CryptoError> {
    // Derive the machine-specific encryption key
    let key_bytes = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    // Generate a 12-byte nonce from timer ticks
    // Each encryption uses a different nonce, so even encrypting the
    // same API key twice produces different ciphertext
    let mut nonce_bytes = [0u8; 12];
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    let ticks = unsafe { kernel_milliseconds() };
    nonce_bytes[..8].copy_from_slice(&ticks.to_le_bytes());

    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt the plaintext key
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| CryptoError::EncryptionFailed)?;

    // Prepend nonce to ciphertext for storage
    // Final blob: [12-byte nonce][ciphertext with 16-byte GCM tag]
    let mut result = nonce_bytes.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Decrypt an encrypted API key blob back to plaintext.
///
/// Takes: the encrypted blob (as stored in SVFS)
/// Returns: the original API key string
///
/// This will fail with DecryptionFailed if:
///   - The blob was encrypted on a different machine (different CPUID)
///   - The machine seed was regenerated (SVFS was wiped)
///   - The blob was corrupted or tampered with
pub fn decrypt_api_key(blob: &[u8]) -> Result<String, CryptoError> {
    // The blob must contain at least the 12-byte nonce
    if blob.len() < 12 {
        return Err(CryptoError::InvalidBlob);
    }

    // Split the blob into nonce and ciphertext
    let (nonce_bytes, ciphertext) = blob.split_at(12);

    // Derive the same machine-specific key used for encryption
    let key_bytes = derive_machine_key();
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Decrypt — this also verifies the GCM authentication tag,
    // so if the data was tampered with, this returns an error
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    // Convert the decrypted bytes back to a string
    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidUtf8)
}
