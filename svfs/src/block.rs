//! Block device abstraction — the hardware interface for SVFS.
//!
//! SVFS doesn't care whether it's writing to a real disk, a virtio device,
//! or a chunk of RAM. It talks through the BlockDevice trait, which provides
//! read_blocks() and write_blocks() at 512-byte granularity.
//!
//! This lets us test SVFS with a RAM-backed device (fast, non-persistent)
//! and later swap in a real virtio-blk driver without changing any SVFS code.

/// Size of a single block in bytes (standard disk sector size).
pub const BLOCK_SIZE: usize = 512;

/// Errors from block device operations.
#[derive(Debug)]
pub enum BlockError {
    /// LBA or count exceeds device capacity
    OutOfRange,
    /// Hardware reported an error
    HardwareError,
    /// Device isn't initialized yet
    NotReady,
}

/// Trait for any block storage device.
///
/// Implementations must be thread-safe (Send) since SVFS may be
/// accessed from multiple agents simultaneously.
pub trait BlockDevice: Send {
    /// Read `count` blocks starting at logical block address `lba`.
    /// `buf` must be exactly `count * BLOCK_SIZE` bytes.
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Write `count` blocks starting at `lba`.
    /// `buf` must be exactly `count * BLOCK_SIZE` bytes.
    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), BlockError>;

    /// Total number of 512-byte blocks on the device.
    fn block_count(&self) -> u64;

    /// Device name for logging (e.g., "ram-blk", "virtio-blk").
    fn name(&self) -> &str;
}
