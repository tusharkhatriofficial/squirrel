#![no_std]
extern crate alloc;

pub mod block;
pub mod ram_blk;

pub use block::{BlockDevice, BlockError, BLOCK_SIZE};
pub use ram_blk::RamBlk;
