//! OsSettings — the runtime settings store.
//!
//! This is the main interface that the rest of the OS uses to read and write
//! settings. It works like a cache backed by persistent storage:
//!
//!   ┌──────────────────────────────────────┐
//!   │  OsSettings                          │
//!   │  ┌──────────────────────────────┐    │
//!   │  │  RwLock<SquirrelSettings>    │    │  ← in-memory cache (fast reads)
//!   │  └──────────────────────────────┘    │
//!   │           ↕ sync on write            │
//!   │  ┌──────────────────────────────┐    │
//!   │  │  SVFS ("os-settings" tag)    │    │  ← persistent storage (survives reboot)
//!   │  └──────────────────────────────┘    │
//!   └──────────────────────────────────────┘
//!
//! Reading a setting (e.g. "what backend are we using?") is just a read lock
//! on the in-memory cache — no disk I/O. This is important because the
//! inference engine reads settings on every single AI request.
//!
//! Writing a setting updates the cache AND persists to SVFS immediately,
//! so changes survive reboots.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::RwLock;

use crate::crypto::{decrypt_api_key, encrypt_api_key, CryptoError};
use crate::schema::{InferenceSettings, SquirrelSettings};

/// Errors that can happen when reading or writing settings.
#[derive(Debug)]
pub enum SettingsError {
    /// SVFS operation failed (disk I/O error, store full, etc.)
    SvfsError(svfs::SvfsError),
    /// Crypto operation failed (encryption/decryption error)
    CryptoError(CryptoError),
    /// The hex-encoded hash couldn't be parsed
    ParseError,
    /// No API key has been configured yet
    NotConfigured,
}

/// Allow converting SVFS errors into SettingsErrors automatically
impl From<svfs::SvfsError> for SettingsError {
    fn from(e: svfs::SvfsError) -> Self {
        SettingsError::SvfsError(e)
    }
}

/// Allow converting crypto errors into SettingsErrors automatically
impl From<CryptoError> for SettingsError {
    fn from(e: CryptoError) -> Self {
        SettingsError::CryptoError(e)
    }
}

/// The OS settings store — loaded from SVFS on boot, cached in RAM.
///
/// This is the single source of truth for all OS settings. The kernel
/// creates one instance on boot and stores it in a global (OS_SETTINGS).
/// All other code reads settings through this store.
pub struct OsSettings {
    /// In-memory cache of all settings, protected by a read-write lock.
    /// Multiple readers can read simultaneously (RwLock::read), but
    /// writes (RwLock::write) are exclusive. This is important for
    /// performance — the inference engine reads settings on every request.
    current: RwLock<SquirrelSettings>,
}

impl OsSettings {
    /// Load settings from SVFS, or write defaults on first boot.
    ///
    /// This is called once during kernel boot, after SVFS is initialized.
    /// It looks for an SVFS object tagged "os-settings":
    ///   - Found: Parse the TOML and use those settings
    ///   - Not found: This is first boot — use defaults and save them
    pub fn load() -> Self {
        let svfs = svfs::SVFS
            .get()
            .expect("SVFS must be initialized before settings");

        // Search SVFS for any object tagged "os-settings"
        let hashes = svfs.find_by_tag("os-settings");

        let settings = if let Some(hash) = hashes.first() {
            // Found existing settings — parse the TOML
            if let Ok(bytes) = svfs.retrieve(hash) {
                let toml = core::str::from_utf8(&bytes).unwrap_or("");
                SquirrelSettings::from_toml(toml)
            } else {
                // SVFS retrieve failed — fall back to defaults
                SquirrelSettings::default()
            }
        } else {
            // No settings found — this is first boot
            SquirrelSettings::default()
        };

        let instance = Self {
            current: RwLock::new(settings),
        };

        // On first boot, persist the defaults to SVFS so they're there
        // for next boot. Also print a message so the user knows what happened.
        if hashes.is_empty() {
            instance.persist_internal().ok();
            crate::println!("[Settings] First boot — defaults written to SVFS");
        } else {
            crate::println!(
                "[Settings] Loaded from SVFS (backend={})",
                instance.current.read().inference.backend
            );
        }

        instance
    }

    /// Get the current inference settings.
    ///
    /// This is the primary API used by the InferenceRouter. It returns
    /// a CLONE of the inference settings, so the caller gets a snapshot
    /// that won't change while they're using it. The clone is cheap
    /// because InferenceSettings is just a few Strings.
    pub fn get_inference_settings(&self) -> InferenceSettings {
        self.current.read().inference.clone()
    }

    /// Get a setting value by its dot-path key.
    ///
    /// Keys use dot notation: "inference.backend", "display.glass_box_visible"
    /// Returns None for unknown keys.
    pub fn get(&self, key: &str) -> Option<String> {
        let s = self.current.read();
        match key {
            "inference.backend" => Some(s.inference.backend.clone()),
            "inference.model_id" => Some(s.inference.model_id.clone()),
            "inference.api_base_url" => Some(s.inference.api_base_url.clone()),
            "inference.local_model_hash" => Some(s.inference.local_model_hash.clone()),
            "inference.api_key_ref" => Some(s.inference.api_key_ref.clone()),
            "display.glass_box_visible" => Some(s.display.glass_box_visible.to_string()),
            "display.boot_animation" => Some(s.display.boot_animation.to_string()),
            _ => None,
        }
    }

    /// Set a setting value and immediately persist to SVFS.
    ///
    /// This is a two-step atomic operation:
    ///   1. Update the in-memory cache (so future reads see the new value)
    ///   2. Write the full settings TOML to SVFS (so it survives reboot)
    ///
    /// If SVFS write fails, the in-memory value is still updated (the
    /// setting works for this session but might not survive reboot).
    pub fn set(&self, key: &str, value: &str) -> Result<(), SettingsError> {
        {
            let mut s = self.current.write();
            match key {
                "inference.backend" => s.inference.backend = String::from(value),
                "inference.model_id" => s.inference.model_id = String::from(value),
                "inference.api_base_url" => s.inference.api_base_url = String::from(value),
                "inference.local_model_hash" => {
                    s.inference.local_model_hash = String::from(value)
                }
                "inference.api_key_ref" => s.inference.api_key_ref = String::from(value),
                "display.glass_box_visible" => s.display.glass_box_visible = value == "true",
                "display.boot_animation" => s.display.boot_animation = value == "true",
                _ => {}
            }
        } // RwLock write guard is dropped here
        self.persist_internal()
    }

    /// Store an API key securely.
    ///
    /// This is the safe way to store an API key. It does three things:
    ///   1. Encrypt the plaintext key using AES-256-GCM (crypto.rs)
    ///   2. Store the encrypted blob in SVFS as a separate object
    ///   3. Save the blob's SVFS hash in settings as "api_key_ref"
    ///
    /// The plaintext key is NEVER stored anywhere on disk.
    /// After this call, the key can be retrieved with get_api_key().
    pub fn set_api_key(&self, plaintext_key: &str) -> Result<(), SettingsError> {
        // Step 1: Encrypt the key
        let encrypted = encrypt_api_key(plaintext_key)?;

        // Step 2: Store encrypted blob in SVFS
        let svfs = svfs::SVFS.get().unwrap();
        let blob_hash = svfs.store(
            &encrypted,
            svfs::ObjectType::Config,
            Some("api-key-blob"),
            &["api-key-blob"],
        )?;

        // Step 3: Save the blob's hash (as hex) in settings
        let hash_hex = hex::encode(blob_hash);
        self.set("inference.api_key_ref", &hash_hex)
    }

    /// Retrieve the decrypted API key.
    ///
    /// This is ONLY called by the inference engine when it needs to make
    /// an API request. The decrypted key is returned as a String and held
    /// briefly in memory — it's never logged, never sent over the Intent
    /// Bus, and never displayed in the Glass Box.
    ///
    /// Returns NotConfigured if no API key has been set.
    /// Returns CryptoError if decryption fails (wrong machine, corrupted blob).
    pub fn get_api_key(&self) -> Result<String, SettingsError> {
        // Read the hash reference from settings
        let key_ref = {
            let s = self.current.read();
            s.inference.api_key_ref.clone()
        };

        if key_ref.is_empty() {
            return Err(SettingsError::NotConfigured);
        }

        // Decode the hex hash to bytes
        let hash_bytes: Vec<u8> = hex::decode(&key_ref).map_err(|_| SettingsError::ParseError)?;

        // Convert to a 32-byte array (blake3 hashes are always 32 bytes)
        let hash: [u8; 32] = hash_bytes
            .try_into()
            .map_err(|_| SettingsError::ParseError)?;

        // Retrieve the encrypted blob from SVFS
        let svfs = svfs::SVFS.get().unwrap();
        let blob = svfs.retrieve(&hash)?;

        // Decrypt and return the plaintext key
        Ok(decrypt_api_key(&blob)?)
    }

    /// Write the current settings to SVFS as a TOML document.
    ///
    /// This is called internally after every set() call. It serializes
    /// the entire settings struct to TOML and stores it in SVFS with
    /// the tag "os-settings".
    fn persist_internal(&self) -> Result<(), SettingsError> {
        let toml = {
            let s = self.current.read();
            s.to_toml()
        };

        let svfs = svfs::SVFS.get().unwrap();
        svfs.store(
            toml.as_bytes(),
            svfs::ObjectType::Config,
            Some("squirrel-setti"),  // 14 chars max (SVFS name limit)
            &["os-settings"],
        )?;

        Ok(())
    }
}
