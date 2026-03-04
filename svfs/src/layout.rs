//! SVFS on-disk layout — how objects are physically stored on the block device.
//!
//! The disk is divided into three regions:
//!
//!   Block 0:      Superblock (512 bytes)
//!                 Contains magic number, version, object count, next free LBA.
//!
//!   Blocks 1-16:  Object Index (16 blocks = 8192 bytes)
//!                 Holds up to 128 index entries, each 64 bytes.
//!                 Each entry records: blake3 hash, data LBA, size, type, name.
//!
//!   Blocks 17+:   Object Data (variable size)
//!                 Actual object bytes, followed by a tag block for each object.
//!
//! This is intentionally simple — no B-trees, no journaling, no free-space
//! bitmap. The MVP needs to prove that content-addressed storage works.
//! Production SVFS will use fjall (LSM-tree) for the index.

/// Magic number: "SQUIRFS\0" in ASCII, stored as little-endian u64.
/// Used to detect whether the disk has been formatted with SVFS.
pub const SUPERBLOCK_MAGIC: u64 = 0x5351_5552_5246_5300;

/// Current on-disk format version (increment when layout changes).
pub const SUPERBLOCK_VERSION: u32 = 1;

/// Number of blocks reserved for the object index (blocks 1-16).
pub const INDEX_BLOCKS: u64 = 16;

/// Maximum objects the index can hold.
/// 16 blocks × 512 bytes / 64 bytes per entry = 128 entries.
pub const MAX_OBJECTS: usize = 128;

/// Size of each index entry in bytes.
pub const INDEX_ENTRY_SIZE: usize = 64;

/// First LBA of the object data area (after superblock + index).
pub const DATA_START_LBA: u64 = 1 + INDEX_BLOCKS; // = block 17

/// The superblock — lives at block 0.
///
/// Contains the minimum metadata needed to find everything else.
/// When SVFS mounts the device, it reads this first to determine
/// if the disk is formatted and where the data starts.
#[repr(C)]
pub struct Superblock {
    pub magic: u64,
    pub version: u32,
    pub object_count: u32,
    pub data_lba_next: u64, // Next free LBA for storing new objects
    pub _reserved: [u8; 488],
}

impl Superblock {
    pub fn new() -> Self {
        Self {
            magic: SUPERBLOCK_MAGIC,
            version: SUPERBLOCK_VERSION,
            object_count: 0,
            data_lba_next: DATA_START_LBA,
            _reserved: [0u8; 488],
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == SUPERBLOCK_MAGIC && self.version == SUPERBLOCK_VERSION
    }
}

/// One entry in the object index (64 bytes total).
///
/// This is how SVFS finds an object: given a blake3 hash, scan the
/// index for a matching entry, then read from the stored LBA.
#[repr(C)]
pub struct IndexEntry {
    /// blake3 hash of the object content (32 bytes).
    /// This IS the object's identity — same content = same hash.
    pub hash: [u8; 32],
    /// LBA where the object data starts on disk.
    pub data_lba: u64,
    /// Size of the object in bytes (actual, not padded).
    pub data_size: u32,
    /// Object type: 0=Data, 1=Config, 2=Code, 3=Model.
    pub object_type: u8,
    /// Length of the name string (0-14 bytes).
    pub name_len: u8,
    /// Human-readable name (up to 14 bytes, UTF-8).
    pub name: [u8; 14],
    /// Padding to reach exactly 64 bytes.
    pub _pad: [u8; 4],
}
// Verify: 32 + 8 + 4 + 1 + 1 + 14 + 4 = 64 bytes ✓
