//! Network device abstraction — trait for all NIC drivers.
//!
//! Any struct implementing NicDriver can be plugged into SmoltcpAdapter.
//! The trait is object-safe so we can use Box<dyn NicDriver>.

/// Trait for any NIC driver — virtio-net, e1000e, rtl8139.
pub trait NicDriver: Send {
    /// Send a raw Ethernet frame (no device-specific headers).
    fn send_frame(&mut self, frame: &[u8]);

    /// Receive a raw Ethernet frame (non-blocking).
    /// Returns the number of bytes written to buf, or None if no frame available.
    fn recv_frame(&mut self, buf: &mut [u8]) -> Option<usize>;

    /// The device's 6-byte MAC address.
    fn mac_address(&self) -> [u8; 6];
}
