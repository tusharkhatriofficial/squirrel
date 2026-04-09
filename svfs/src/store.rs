//! Svfs — the Semantic Virtual File System core.
//!
//! This is the heart of SVFS: a content-addressed object store backed by
//! a block device. The three fundamental operations are:
//!
//!   store(data, type, name, tags) → blake3 hash
//!     Hash the data, write it to disk, record in index, return hash.
//!     If the same content already exists, return existing hash (dedup).
//!
//!   retrieve(hash) → data bytes
//!     Look up the hash in the index, read from the stored LBA.
//!
//!   find_by_tag(tag) → list of hashes
//!     Scan the index, read each object's tag block, return matches.
//!
//! This is intentionally NOT a filesystem. There are no directories, no
//! permissions, no inodes. Objects are identified by what they contain
//! (their hash), not where they live. This is the foundation for AI-native
//! storage: the AI asks "find everything tagged 'project-alpha'" rather
//! than navigating a directory tree.

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use spin::Mutex;

use crate::block::{BlockDevice, BlockError, BLOCK_SIZE};
use crate::layout::{
    IndexEntry, Superblock, INDEX_BLOCKS,
    INDEX_ENTRY_SIZE, MAX_OBJECTS, SUPERBLOCK_MAGIC,
};

/// The type of a stored object — hints for AI reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Data = 0,   // Generic data (text, binary)
    Config = 1, // Configuration (settings, preferences)
    Code = 2,   // Executable code (WASM modules)
    Model = 3,  // AI model weights
}

/// Errors from SVFS operations.
#[derive(Debug)]
pub enum SvfsError {
    /// Block device I/O failed
    BlockError(BlockError),
    /// Index is full (128 objects max in MVP)
    StoreFull,
    /// No object with that hash exists
    NotFound,
    /// Superblock magic/version mismatch
    InvalidFormat,
}

impl From<BlockError> for SvfsError {
    fn from(e: BlockError) -> Self {
        SvfsError::BlockError(e)
    }
}

/// A blake3 content hash — the identity of an SVFS object.
pub type ContentHash = [u8; 32];

/// The SVFS store — manages all objects on a block device.
pub struct Svfs {
    device: Mutex<Box<dyn BlockDevice>>,
    /// Cached copy of the on-disk index (loaded on init, synced on write)
    index: Mutex<Vec<IndexEntry>>,
    /// Cached superblock metadata
    superblock: Mutex<Superblock>,
}

impl Svfs {
    /// Initialize SVFS on the given block device.
    ///
    /// If the device has a valid SVFS superblock (magic number matches),
    /// load the existing index. Otherwise, format the device fresh.
    pub fn new(device: Box<dyn BlockDevice>) -> Result<Self, SvfsError> {
        let svfs = Self {
            device: Mutex::new(device),
            index: Mutex::new(Vec::new()),
            superblock: Mutex::new(Superblock::new()),
        };

        // Read block 0 to check if this device is already formatted
        let mut sb_buf = [0u8; BLOCK_SIZE];
        svfs.device.lock().read_blocks(0, 1, &mut sb_buf)?;

        let magic = u64::from_le_bytes(sb_buf[0..8].try_into().unwrap());
        if magic == SUPERBLOCK_MAGIC {
            // Device already has SVFS — load the existing data
            svfs.load_superblock(&sb_buf)?;
            svfs.load_index()?;
        } else {
            // Fresh device — write a new empty filesystem
            svfs.format()?;
        }

        Ok(svfs)
    }

    /// Parse superblock from raw bytes.
    fn load_superblock(&self, buf: &[u8]) -> Result<(), SvfsError> {
        let mut sb = self.superblock.lock();
        sb.magic = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        sb.version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        sb.object_count = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        sb.data_lba_next = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        if !sb.is_valid() {
            return Err(SvfsError::InvalidFormat);
        }
        Ok(())
    }

    /// Load the object index from blocks 1-16.
    fn load_index(&self) -> Result<(), SvfsError> {
        let mut index_buf = vec![0u8; INDEX_BLOCKS as usize * BLOCK_SIZE];
        self.device
            .lock()
            .read_blocks(1, INDEX_BLOCKS as usize, &mut index_buf)?;

        let count = self.superblock.lock().object_count as usize;
        let mut index = self.index.lock();
        index.clear();

        for i in 0..count.min(MAX_OBJECTS) {
            let offset = i * INDEX_ENTRY_SIZE;
            let eb = &index_buf[offset..offset + INDEX_ENTRY_SIZE];
            let mut entry = IndexEntry {
                hash: [0u8; 32],
                data_lba: 0,
                data_size: 0,
                object_type: 0,
                name_len: 0,
                name: [0u8; 14],
                _pad: [0u8; 4],
            };
            entry.hash.copy_from_slice(&eb[0..32]);
            entry.data_lba = u64::from_le_bytes(eb[32..40].try_into().unwrap());
            entry.data_size = u32::from_le_bytes(eb[40..44].try_into().unwrap());
            entry.object_type = eb[44];
            entry.name_len = eb[45];
            entry.name.copy_from_slice(&eb[46..60]);
            index.push(entry);
        }
        Ok(())
    }

    /// Format a fresh SVFS filesystem on the device.
    fn format(&self) -> Result<(), SvfsError> {
        // Write fresh superblock at block 0
        let mut sb_buf = [0u8; BLOCK_SIZE];
        let sb = Superblock::new();
        sb_buf[0..8].copy_from_slice(&sb.magic.to_le_bytes());
        sb_buf[8..12].copy_from_slice(&sb.version.to_le_bytes());
        sb_buf[12..16].copy_from_slice(&0u32.to_le_bytes());
        sb_buf[16..24].copy_from_slice(&sb.data_lba_next.to_le_bytes());
        self.device.lock().write_blocks(0, 1, &sb_buf)?;

        // Write empty index at blocks 1-16
        let empty_index = vec![0u8; INDEX_BLOCKS as usize * BLOCK_SIZE];
        self.device
            .lock()
            .write_blocks(1, INDEX_BLOCKS as usize, &empty_index)?;

        *self.superblock.lock() = sb;
        Ok(())
    }

    /// Store an object. Returns its blake3 content hash.
    ///
    /// If an object with the same content already exists, returns the
    /// existing hash without writing anything (content-addressed dedup).
    pub fn store(
        &self,
        data: &[u8],
        object_type: ObjectType,
        name: Option<&str>,
        tags: &[&str],
    ) -> Result<ContentHash, SvfsError> {
        // Hash the content — this IS the object's identity
        let hash: [u8; 32] = blake3::hash(data).into();

        // Check for deduplication
        {
            let index = self.index.lock();
            if index.iter().any(|e| e.hash == hash) {
                return Ok(hash); // Already stored — same content, same hash
            }
            if index.len() >= MAX_OBJECTS {
                return Err(SvfsError::StoreFull);
            }
        }

        // Calculate how many blocks we need for the data
        let data_blocks = if data.is_empty() {
            1
        } else {
            (data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE
        };

        // Allocate LBAs: data blocks + 1 tag block
        let data_lba = {
            let mut sb = self.superblock.lock();
            let lba = sb.data_lba_next;
            sb.data_lba_next += data_blocks as u64 + 1; // +1 for tag block
            lba
        };

        // Write data (padded to block boundary)
        let mut padded = data.to_vec();
        let total_data_bytes = data_blocks * BLOCK_SIZE;
        padded.resize(total_data_bytes, 0u8);
        self.device
            .lock()
            .write_blocks(data_lba, data_blocks, &padded)?;

        // Write tag block (tags as comma-separated string in 512 bytes)
        let tag_str: String = tags.join(",");
        let mut tag_block = [0u8; BLOCK_SIZE];
        let tag_bytes = tag_str.as_bytes();
        let len = tag_bytes.len().min(BLOCK_SIZE);
        tag_block[..len].copy_from_slice(&tag_bytes[..len]);
        self.device
            .lock()
            .write_blocks(data_lba + data_blocks as u64, 1, &tag_block)?;

        // Build the index entry
        let name_bytes = name.unwrap_or("").as_bytes();
        let name_len = name_bytes.len().min(14) as u8;
        let mut name_arr = [0u8; 14];
        name_arr[..name_len as usize].copy_from_slice(&name_bytes[..name_len as usize]);

        let entry = IndexEntry {
            hash,
            data_lba,
            data_size: data.len() as u32,
            object_type: object_type as u8,
            name_len,
            name: name_arr,
            _pad: [0u8; 4],
        };
        self.index.lock().push(entry);

        // Persist the updated index and superblock to disk
        self.persist_index()?;

        Ok(hash)
    }

    /// Retrieve an object by its content hash.
    pub fn retrieve(&self, hash: &ContentHash) -> Result<Vec<u8>, SvfsError> {
        let (data_lba, data_size) = {
            let index = self.index.lock();
            let entry = index
                .iter()
                .find(|e| &e.hash == hash)
                .ok_or(SvfsError::NotFound)?;
            (entry.data_lba, entry.data_size as usize)
        };

        let blocks = if data_size == 0 {
            1
        } else {
            (data_size + BLOCK_SIZE - 1) / BLOCK_SIZE
        };
        let mut buf = vec![0u8; blocks * BLOCK_SIZE];
        self.device.lock().read_blocks(data_lba, blocks, &mut buf)?;
        buf.truncate(data_size);
        Ok(buf)
    }

    /// Find all objects that have a given semantic tag.
    pub fn find_by_tag(&self, tag: &str) -> Vec<ContentHash> {
        let index = self.index.lock();
        let mut results = Vec::new();

        for entry in index.iter() {
            // Calculate the LBA of this object's tag block
            let data_blocks = if entry.data_size == 0 {
                1
            } else {
                ((entry.data_size as usize) + BLOCK_SIZE - 1) / BLOCK_SIZE
            };
            let tag_lba = entry.data_lba + data_blocks as u64;

            let mut tag_block = [0u8; BLOCK_SIZE];
            if self
                .device
                .lock()
                .read_blocks(tag_lba, 1, &mut tag_block)
                .is_ok()
            {
                let tag_str = core::str::from_utf8(&tag_block)
                    .unwrap_or("")
                    .trim_matches('\0');
                if tag_str.split(',').any(|t| t.trim() == tag) {
                    results.push(entry.hash);
                }
            }
        }
        results
    }

    /// Read the raw tag block for an object identified by its content hash.
    ///
    /// Returns the tag string (up to 512 bytes). The tag block lives
    /// immediately after the object's data blocks on disk.
    pub fn get_tags(&self, hash: &ContentHash) -> Option<String> {
        let index = self.index.lock();
        let entry = index.iter().find(|e| &e.hash == hash)?;

        let data_blocks = if entry.data_size == 0 {
            1
        } else {
            ((entry.data_size as usize) + BLOCK_SIZE - 1) / BLOCK_SIZE
        };
        let tag_lba = entry.data_lba + data_blocks as u64;

        let mut tag_block = [0u8; BLOCK_SIZE];
        self.device.lock().read_blocks(tag_lba, 1, &mut tag_block).ok()?;

        let raw = core::str::from_utf8(&tag_block).unwrap_or("").trim_matches('\0');
        Some(String::from(raw))
    }

    /// Write the in-memory index and superblock back to disk.
    fn persist_index(&self) -> Result<(), SvfsError> {
        let index = self.index.lock();
        let mut index_buf = vec![0u8; INDEX_BLOCKS as usize * BLOCK_SIZE];

        for (i, entry) in index.iter().enumerate() {
            if i >= MAX_OBJECTS {
                break;
            }
            let offset = i * INDEX_ENTRY_SIZE;
            index_buf[offset..offset + 32].copy_from_slice(&entry.hash);
            index_buf[offset + 32..offset + 40].copy_from_slice(&entry.data_lba.to_le_bytes());
            index_buf[offset + 40..offset + 44].copy_from_slice(&entry.data_size.to_le_bytes());
            index_buf[offset + 44] = entry.object_type;
            index_buf[offset + 45] = entry.name_len;
            index_buf[offset + 46..offset + 60].copy_from_slice(&entry.name);
        }

        self.device
            .lock()
            .write_blocks(1, INDEX_BLOCKS as usize, &index_buf)?;

        // Update the superblock's object count and next-free-LBA on disk
        let mut sb_buf = [0u8; BLOCK_SIZE];
        self.device.lock().read_blocks(0, 1, &mut sb_buf)?;
        let count = index.len() as u32;
        sb_buf[12..16].copy_from_slice(&count.to_le_bytes());
        let next_lba = self.superblock.lock().data_lba_next;
        sb_buf[16..24].copy_from_slice(&next_lba.to_le_bytes());
        self.device.lock().write_blocks(0, 1, &sb_buf)?;

        Ok(())
    }

    /// Find an object by its human-readable name.
    ///
    /// Scans the index for the first entry whose name matches (case-insensitive).
    /// Returns the content hash if found, which can be passed to `retrieve()`.
    pub fn find_by_name(&self, name: &str) -> Option<ContentHash> {
        let index = self.index.lock();
        let needle = name.as_bytes();
        for entry in index.iter() {
            let entry_name = &entry.name[..entry.name_len as usize];
            if entry_name.len() == needle.len()
                && entry_name
                    .iter()
                    .zip(needle.iter())
                    .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
            {
                return Some(entry.hash);
            }
        }
        None
    }

    /// List all stored objects.
    ///
    /// Returns a Vec of (name, object_type, data_size) tuples for every
    /// object in the index. Used by the primary agent's "list files" command.
    pub fn list_all(&self) -> Vec<(String, ObjectType, u32)> {
        let index = self.index.lock();
        let mut results = Vec::new();
        for entry in index.iter() {
            let name = core::str::from_utf8(&entry.name[..entry.name_len as usize])
                .unwrap_or("<invalid>");
            let obj_type = match entry.object_type {
                0 => ObjectType::Data,
                1 => ObjectType::Config,
                2 => ObjectType::Code,
                3 => ObjectType::Model,
                _ => ObjectType::Data,
            };
            results.push((String::from(name), obj_type, entry.data_size));
        }
        results
    }

    /// Delete an object by name.
    ///
    /// Removes the entry from the in-memory index and persists the change.
    /// The data blocks on disk are NOT reclaimed (append-only in MVP).
    /// Returns true if an object was found and deleted, false if not found.
    pub fn delete_by_name(&self, name: &str) -> Result<bool, SvfsError> {
        let needle = name.as_bytes();
        let mut index = self.index.lock();
        let pos = index.iter().position(|entry| {
            let entry_name = &entry.name[..entry.name_len as usize];
            entry_name.len() == needle.len()
                && entry_name
                    .iter()
                    .zip(needle.iter())
                    .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
        });

        if let Some(i) = pos {
            index.remove(i);
            // Update superblock count
            self.superblock.lock().object_count = index.len() as u32;
            drop(index); // release lock before persisting
            self.persist_index()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Number of objects currently stored.
    pub fn object_count(&self) -> usize {
        self.index.lock().len()
    }
}
