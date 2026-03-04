//! RAM-backed block device — non-persistent storage for MVP testing.
//!
//! RamBlk allocates a Vec<u8> on the kernel heap and provides block
//! read/write operations against it. Data is lost on reboot, but this
//! lets us test the entire SVFS stack (store, retrieve, tag queries)
//! without needing a real disk driver.
//!
//! When the full virtio-blk driver is ready (Phase 09+), SVFS will
//! switch to persistent storage. The SVFS code stays the same — only
//! the BlockDevice implementation changes.

use alloc::vec::Vec;
use spin::Mutex;

use crate::block::{BlockDevice, BlockError, BLOCK_SIZE};

/// RAM-backed block device for testing.
///
/// Allocates `blocks * 512` bytes on the kernel heap.
/// All data is zeroed on creation and lost on reboot.
pub struct RamBlk {
    data: Mutex<Vec<u8>>,
    blocks: u64,
}

impl RamBlk {
    /// Create a RAM disk of `blocks` 512-byte blocks.
    ///
    /// Example: `RamBlk::new(2048)` creates a 1 MB RAM disk.
    pub fn new(blocks: u64) -> Self {
        let size = (blocks as usize) * BLOCK_SIZE;
        let mut data = Vec::with_capacity(size);
        data.resize(size, 0u8);
        Self {
            data: Mutex::new(data),
            blocks,
        }
    }
}

impl BlockDevice for RamBlk {
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError> {
        let data = self.data.lock();
        let start = lba as usize * BLOCK_SIZE;
        let end = start + count * BLOCK_SIZE;
        if end > data.len() {
            return Err(BlockError::OutOfRange);
        }
        buf[..count * BLOCK_SIZE].copy_from_slice(&data[start..end]);
        Ok(())
    }

    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError> {
        let mut data = self.data.lock();
        let start = lba as usize * BLOCK_SIZE;
        let end = start + count * BLOCK_SIZE;
        if end > data.len() {
            return Err(BlockError::OutOfRange);
        }
        data[start..end].copy_from_slice(&buf[..count * BLOCK_SIZE]);
        Ok(())
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn name(&self) -> &str {
        "ram-blk"
    }
}
