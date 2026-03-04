//! Network device abstraction layer.
//!
//! Defines the NetworkDevice trait that all network drivers implement.
//! Currently supports virtio-net (for QEMU); future phases may add
//! e1000e for bare-metal hardware.

pub mod virtio_net;

/// Trait for any network device — virtio-net for QEMU, e1000e for bare metal.
pub trait NetworkDevice: Send {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetworkError>;
    fn recv_frame(&mut self, buf: &mut [u8]) -> Result<usize, NetworkError>;
    fn mac_address(&self) -> [u8; 6];
}

#[derive(Debug)]
pub enum NetworkError {
    NoDevice,
    TxFull,
    RxEmpty,
    HardwareError,
}
