#![no_std]
extern crate alloc;

pub mod block;
pub mod layout;
pub mod ram_blk;
pub mod store;

pub use block::{BlockDevice, BlockError, BLOCK_SIZE};
pub use ram_blk::RamBlk;
pub use store::{ContentHash, ObjectType, Svfs, SvfsError};

use spin::Once;

/// The global SVFS instance (initialized once on boot).
///
/// SVFS is a singleton because there's one storage device. All agents
/// and modules access it through this global.
pub static SVFS: Once<Svfs> = Once::new();

/// Initialize SVFS with the given block device.
/// Called once by the kernel during boot.
pub fn init(device: alloc::boxed::Box<dyn BlockDevice>) {
    SVFS.call_once(|| Svfs::new(device).expect("SVFS initialization failed"));
}
