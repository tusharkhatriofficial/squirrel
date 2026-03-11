//! TLS 1.3 implementation using embedded-tls (pure Rust, no_std).
//!
//! This module provides TLS encryption for HTTPS connections. It wraps
//! raw TCP sockets from the NetworkStack in a TLS session using the
//! embedded-tls crate, which is designed for embedded/bare-metal systems.
//!
//! Key design choices:
//!   - Uses embedded-tls blocking API (not async) since we have no async runtime.
//!   - Uses embedded-tls (pure Rust, RustCrypto) instead of rustls+ring
//!     because ring compiles C code that needs libc headers, which don't
//!     exist on bare-metal x86_64.
//!   - Uses RDRAND (hardware RNG) for all cryptographic randomness.
//!   - Certificate verification is skipped for now (UnsecureProvider) — we
//!     still get encrypted communication, just without server identity
//!     verification. Proper cert verification comes when we embed root CAs.
//!   - The entire HTTPS request is performed in a single function call
//!     to avoid self-referential lifetime issues with TLS record buffers.

use alloc::{vec, vec::Vec};
use embedded_io::{ErrorType, Read, Write};
use embedded_tls::blocking::{TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use embedded_tls::Aes128GcmSha256;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;

use crate::rng::RdRandRng;
use crate::stack::NetworkStack;

// ---------------------------------------------------------------------------
// embedded-io adapter for smoltcp TCP sockets
// ---------------------------------------------------------------------------

/// Bridges a smoltcp TCP socket to embedded-io's Read/Write traits.
///
/// embedded-tls requires a transport layer implementing embedded-io traits.
/// This adapter wraps a smoltcp TCP socket handle and provides blocking
/// read/write by polling the network stack until data is available.
struct TcpTransport<'a> {
    stack: &'a mut NetworkStack,
    handle: SocketHandle,
}

impl<'a> TcpTransport<'a> {
    fn new(stack: &'a mut NetworkStack, handle: SocketHandle) -> Self {
        Self { stack, handle }
    }
}

#[derive(Debug)]
struct TcpError;

impl core::fmt::Display for TcpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TCP transport error")
    }
}

impl core::error::Error for TcpError {}

impl embedded_io::Error for TcpError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl<'a> ErrorType for TcpTransport<'a> {
    type Error = TcpError;
}

impl<'a> Read for TcpTransport<'a> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        let deadline = kernel_ms() + 30_000;

        loop {
            self.stack.poll();

            let socket = self.stack.sockets.get_mut::<tcp::Socket>(self.handle);
            if socket.can_recv() {
                return socket.recv_slice(buf).map_err(|_| TcpError);
            }

            if !socket.is_open() {
                return Ok(0); // EOF
            }

            if kernel_ms() > deadline {
                return Err(TcpError);
            }

            x86_64::instructions::hlt();
        }
    }
}

impl<'a> Write for TcpTransport<'a> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        let deadline = kernel_ms() + 10_000;

        loop {
            self.stack.poll();

            let socket = self.stack.sockets.get_mut::<tcp::Socket>(self.handle);
            if socket.can_send() {
                return socket.send_slice(buf).map_err(|_| TcpError);
            }

            if kernel_ms() > deadline {
                return Err(TcpError);
            }

            x86_64::instructions::hlt();
        }
    }

    fn flush(&mut self) -> Result<(), TcpError> {
        self.stack.poll();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TLS-wrapped HTTP POST
// ---------------------------------------------------------------------------

/// Buffer size for TLS records. TLS 1.3 records can be up to 16KB + overhead.
const TLS_RECORD_BUF_SIZE: usize = 16640;

/// Perform a complete HTTPS request over TLS 1.3.
///
/// This function owns the TLS record buffers and transport for the entire
/// duration of the request, avoiding self-referential lifetime issues.
///
/// Steps:
///   1. Wrap the TCP socket in an embedded-io adapter
///   2. Perform TLS 1.3 handshake (ECDHE key exchange, AES-128-GCM cipher)
///   3. Send the HTTP request (headers + body) over the encrypted channel
///   4. Read the full HTTP response over the encrypted channel
///   5. Return the raw response bytes
pub fn tls_post(
    stack: &mut NetworkStack,
    tcp_handle: SocketHandle,
    server_name: &str,
    request_bytes: &[u8],
) -> Result<Vec<u8>, &'static str> {
    let transport = TcpTransport::new(stack, tcp_handle);
    let rng = RdRandRng;

    // TLS record buffers — live for the entire request
    let mut read_buf = vec![0u8; TLS_RECORD_BUF_SIZE];
    let mut write_buf = vec![0u8; TLS_RECORD_BUF_SIZE];

    // Configure TLS — UnsecureProvider skips cert verification for MVP
    let config = TlsConfig::new().with_server_name(server_name);

    let mut tls: TlsConnection<TcpTransport<'_>, Aes128GcmSha256> =
        TlsConnection::new(transport, &mut read_buf, &mut write_buf);

    // Create crypto provider with our RDRAND-based RNG
    let provider = UnsecureProvider::new::<Aes128GcmSha256>(rng);

    // TLS 1.3 handshake
    tls.open(TlsContext::new(&config, provider))
        .map_err(|_| "TLS handshake failed")?;

    // Send the HTTP request over TLS
    let mut offset = 0;
    while offset < request_bytes.len() {
        let n = tls
            .write(&request_bytes[offset..])
            .map_err(|_| "TLS write failed")?;
        if n == 0 {
            return Err("TLS: connection closed during write");
        }
        offset += n;
    }
    tls.flush().map_err(|_| "TLS flush failed")?;

    // Read the full HTTP response over TLS
    let mut result = Vec::new();
    let mut buf = vec![0u8; 4096];
    let deadline = kernel_ms() + 30_000;

    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => result.extend_from_slice(&buf[..n]),
            Err(_) => {
                if result.is_empty() {
                    return Err("TLS read failed");
                }
                break;
            }
        }

        if kernel_ms() > deadline {
            if result.is_empty() {
                return Err("TLS read timeout");
            }
            break;
        }
    }

    Ok(result)
}

fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
