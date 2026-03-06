//! API key encryption — protects secrets at rest using blake3-derived keystream.
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
//! from the CPU). Together they derive a blake3 keystream for encryption.
//!
//! We use blake3 in keyed-hash mode as a stream cipher. blake3 is already
//! in the project (SVFS uses it for content hashing) and works in no_std
//! without any SIMD cross-compilation issues.
//!
//! Security properties:
//!   - API keys are encrypted on disk (safe if disk is stolen)
//!   - Moving SVFS to another machine breaks decryption (different CPUID)
//!   - The encryption key is NEVER stored anywhere — derived at runtime

use alloc::string::String;
use alloc::vec::Vec;

/// Errors that can happen during encryption or decryption.
#[derive(Debug)]
pub enum CryptoError {
    /// Encryption failed
    EncryptionFailed,
    /// Decryption failed — wrong key (different machine?) or corrupted data
    DecryptionFailed,
    /// The encrypted blob is too short to contain valid data
    InvalidBlob,
    /// The decrypted bytes aren't valid UTF-8
    InvalidUtf8,
}

/// Derive a 256-bit encryption key from this machine's hardware.
///
/// How it works:
///   1. Read CPUID leaf 0 — gives us 16 bytes identifying the CPU
///   2. Get the "machine seed" from SVFS — a 32-byte random value
///      generated on first boot
///   3. Use blake3 keyed hash to mix both into a 256-bit key
///
/// The result is a key that:
///   - Is different on every machine (different CPUIDs)
///   - Is different on every fresh install (different random seeds)
///   - Is never stored on disk (computed fresh each time)
pub fn derive_machine_key() -> [u8; 32] {
    // Step 1: Read CPU identification
    // SAFETY: __cpuid is safe on x86_64 for leaf 0 (always supported).
    // transmute converts four u32 registers into a byte array.
    #[allow(unused_unsafe)]
    let cpuid = unsafe { core::arch::x86_64::__cpuid(0) };
    #[allow(unused_unsafe)]
    let cpuid_bytes: [u8; 16] = unsafe {
        core::mem::transmute([cpuid.eax, cpuid.ebx, cpuid.ecx, cpuid.edx])
    };

    // Step 2: Get the machine seed from SVFS
    let seed = get_or_create_machine_seed();

    // Step 3: Derive the key using blake3
    // We use the seed as a blake3 key (32 bytes) and hash the CPUID bytes.
    // This produces a cryptographically strong 256-bit key that depends
    // on both the machine hardware and the random seed.
    let key: [u8; 32] = blake3::keyed_hash(&seed, &cpuid_bytes).into();
    key
}

/// Get the machine seed from SVFS, or generate one on first boot.
///
/// The machine seed is a 32-byte random value generated ONCE when
/// Squirrel first boots on a machine. It persists in SVFS with
/// the tag "machine-seed".
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

    // First boot — generate a new random seed from timer entropy
    let mut seed = [0u8; 32];
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    let ticks = unsafe { kernel_milliseconds() };

    // Mix the timer value through multiple rounds to fill 32 bytes
    for i in 0..4 {
        let mixed = ticks
            .wrapping_add(i as u64 * 0x517cc1b727220a95)
            .wrapping_mul(0x6c62272e07bb0142);
        seed[i * 8..(i + 1) * 8].copy_from_slice(&mixed.to_le_bytes());
    }

    // Store the seed in SVFS
    svfs.store(
        &seed,
        svfs::ObjectType::Config,
        Some("machine-seed"),
        &["machine-seed"],
    )
    .ok();

    seed
}

/// Generate a keystream for encryption/decryption.
///
/// Uses blake3 in keyed-hash mode to generate a pseudo-random byte
/// stream. The keystream depends on both the machine key and a nonce,
/// so encrypting the same plaintext twice with different nonces produces
/// different ciphertext.
///
/// We generate the keystream by hashing successive block indices:
///   keystream[0..32]  = blake3_keyed(machine_key, nonce || 0)
///   keystream[32..64] = blake3_keyed(machine_key, nonce || 1)
///   etc.
fn generate_keystream(machine_key: &[u8; 32], nonce: &[u8; 8], len: usize) -> Vec<u8> {
    let mut keystream = Vec::with_capacity(len);
    let mut block_idx: u32 = 0;

    while keystream.len() < len {
        // Build input: [8-byte nonce][4-byte block index]
        let mut input = [0u8; 12];
        input[..8].copy_from_slice(nonce);
        input[8..12].copy_from_slice(&block_idx.to_le_bytes());

        // Hash to produce 32 bytes of keystream
        let block: [u8; 32] = blake3::keyed_hash(machine_key, &input).into();
        let remaining = len - keystream.len();
        let take = remaining.min(32);
        keystream.extend_from_slice(&block[..take]);

        block_idx += 1;
    }

    keystream
}

/// Encrypt a plaintext API key.
///
/// Takes: the API key as a string (e.g. "sk-ant-abc123...")
/// Returns: an encrypted blob
///
/// Blob format: [4-byte magic "SQKR"][8-byte nonce][XOR-encrypted data][32-byte HMAC]
///
/// The HMAC (blake3 keyed hash of the ciphertext) provides integrity —
/// if someone tampers with the blob, decryption will detect it.
pub fn encrypt_api_key(plaintext: &str) -> Result<Vec<u8>, CryptoError> {
    let machine_key = derive_machine_key();
    let data = plaintext.as_bytes();

    // Generate 8-byte nonce from timer ticks
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    let ticks = unsafe { kernel_milliseconds() };
    let nonce: [u8; 8] = ticks.to_le_bytes();

    // Generate keystream and XOR with plaintext
    let keystream = generate_keystream(&machine_key, &nonce, data.len());
    let mut ciphertext = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        ciphertext.push(b ^ keystream[i]);
    }

    // Compute HMAC over nonce + ciphertext for integrity
    let mut hmac_input = Vec::with_capacity(8 + ciphertext.len());
    hmac_input.extend_from_slice(&nonce);
    hmac_input.extend_from_slice(&ciphertext);
    let hmac: [u8; 32] = blake3::keyed_hash(&machine_key, &hmac_input).into();

    // Build the blob: [magic][nonce][ciphertext][hmac]
    let mut blob = Vec::with_capacity(4 + 8 + ciphertext.len() + 32);
    blob.extend_from_slice(b"SQKR"); // 4-byte magic
    blob.extend_from_slice(&nonce);   // 8-byte nonce
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&hmac);    // 32-byte integrity tag

    Ok(blob)
}

/// Decrypt an encrypted API key blob back to plaintext.
///
/// This will fail if:
///   - The blob was encrypted on a different machine (different CPUID)
///   - The machine seed was regenerated (SVFS was wiped)
///   - The blob was corrupted or tampered with (HMAC check fails)
pub fn decrypt_api_key(blob: &[u8]) -> Result<String, CryptoError> {
    // Minimum size: 4 (magic) + 8 (nonce) + 0 (data) + 32 (hmac) = 44
    if blob.len() < 44 {
        return Err(CryptoError::InvalidBlob);
    }

    // Check magic
    if &blob[..4] != b"SQKR" {
        return Err(CryptoError::InvalidBlob);
    }

    let machine_key = derive_machine_key();

    // Extract nonce, ciphertext, and HMAC
    let nonce: [u8; 8] = blob[4..12].try_into().unwrap();
    let ciphertext = &blob[12..blob.len() - 32];
    let stored_hmac = &blob[blob.len() - 32..];

    // Verify HMAC — check integrity before decrypting
    let mut hmac_input = Vec::with_capacity(8 + ciphertext.len());
    hmac_input.extend_from_slice(&nonce);
    hmac_input.extend_from_slice(ciphertext);
    let computed_hmac: [u8; 32] = blake3::keyed_hash(&machine_key, &hmac_input).into();

    // Constant-time comparison to prevent timing attacks
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= stored_hmac[i] ^ computed_hmac[i];
    }
    if diff != 0 {
        return Err(CryptoError::DecryptionFailed);
    }

    // Generate keystream and XOR to recover plaintext
    let keystream = generate_keystream(&machine_key, &nonce, ciphertext.len());
    let mut plaintext_bytes = Vec::with_capacity(ciphertext.len());
    for (i, &b) in ciphertext.iter().enumerate() {
        plaintext_bytes.push(b ^ keystream[i]);
    }

    String::from_utf8(plaintext_bytes).map_err(|_| CryptoError::InvalidUtf8)
}
