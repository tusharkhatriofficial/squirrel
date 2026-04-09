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

// =====================================================================
// TLS Connection Pool — reuse TLS connections across API requests.
//
// The TLS 1.3 handshake (ECDHE + AES-GCM) takes ~10s on bare-metal
// with pure-Rust crypto. By keeping the connection alive between
// requests, subsequent API calls skip DNS + TCP + TLS = ~15s saved.
//
// Safety: SART is single-threaded (cooperative scheduling on one core),
// so there's no data-race risk with static mut. We still use unsafe
// blocks to satisfy the compiler.
// =====================================================================

use alloc::boxed::Box;

// Static TLS record buffers — avoids 32KB heap allocation per request.
static mut POOL_READ_BUF: [u8; TLS_RECORD_BUF_SIZE] = [0u8; TLS_RECORD_BUF_SIZE];
static mut POOL_WRITE_BUF: [u8; TLS_RECORD_BUF_SIZE] = [0u8; TLS_RECORD_BUF_SIZE];

// Global pointer to the NetworkStack, set during pool operations.
static mut ACTIVE_STACK: *mut NetworkStack = core::ptr::null_mut();

// Cached TLS connection state.
type PooledTls = TlsConnection<'static, PoolTransport, Aes128GcmSha256>;
static mut POOL_TLS: *mut PooledTls = core::ptr::null_mut();
static mut POOL_HOSTNAME: [u8; 128] = [0u8; 128];
static mut POOL_HOSTNAME_LEN: usize = 0;

/// TCP transport that accesses NetworkStack via global pointer.
/// Used by pooled TLS connections so we don't need lifetime parameters.
struct PoolTransport {
    handle: SocketHandle,
}

impl ErrorType for PoolTransport {
    type Error = TcpError;
}

impl Read for PoolTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        let deadline = kernel_ms() + 30_000;
        loop {
            let stack = unsafe { &mut *ACTIVE_STACK };
            stack.poll();
            let socket = stack.sockets.get_mut::<tcp::Socket>(self.handle);
            if socket.can_recv() {
                return socket.recv_slice(buf).map_err(|_| TcpError);
            }
            if !socket.is_open() {
                return Ok(0);
            }
            if kernel_ms() > deadline {
                return Err(TcpError);
            }
            x86_64::instructions::hlt();
        }
    }
}

impl Write for PoolTransport {
    fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        let deadline = kernel_ms() + 10_000;
        loop {
            let stack = unsafe { &mut *ACTIVE_STACK };
            stack.poll();
            let socket = stack.sockets.get_mut::<tcp::Socket>(self.handle);
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
        let stack = unsafe { &mut *ACTIVE_STACK };
        stack.poll();
        Ok(())
    }
}

/// Try to reuse a cached TLS connection to the same hostname.
/// Returns the raw HTTP response bytes on success, or Err if no
/// cached connection exists or the connection is dead.
pub fn tls_try_reuse(
    stack: &mut NetworkStack,
    hostname: &str,
    request_bytes: &[u8],
) -> Result<Vec<u8>, &'static str> {
    unsafe {
        if POOL_TLS.is_null() || POOL_HOSTNAME_LEN == 0 {
            return Err("no cached connection");
        }
        let cached = core::str::from_utf8(&POOL_HOSTNAME[..POOL_HOSTNAME_LEN]).unwrap_or("");
        if cached != hostname {
            return Err("hostname mismatch");
        }

        ACTIVE_STACK = stack as *mut _;
        let tls = &mut *POOL_TLS;
        let result = pool_send_and_recv(tls, request_bytes);
        ACTIVE_STACK = core::ptr::null_mut();

        if result.is_err() {
            // Connection is dead — clean up
            let _ = Box::from_raw(POOL_TLS);
            POOL_TLS = core::ptr::null_mut();
            POOL_HOSTNAME_LEN = 0;
        }

        result
    }
}

/// Create a new TLS connection, send a request, and cache the connection
/// for future reuse. The tcp_handle must be an already-connected TCP socket.
pub fn tls_new_and_cache(
    stack: &mut NetworkStack,
    tcp_handle: SocketHandle,
    hostname: &str,
    request_bytes: &[u8],
) -> Result<Vec<u8>, &'static str> {
    unsafe {
        // Drop old cached connection (releases &mut refs to buffers)
        if !POOL_TLS.is_null() {
            let _ = Box::from_raw(POOL_TLS);
            POOL_TLS = core::ptr::null_mut();
            POOL_HOSTNAME_LEN = 0;
        }

        ACTIVE_STACK = stack as *mut _;

        let transport = PoolTransport { handle: tcp_handle };
        let read_buf: &'static mut [u8] = &mut *core::ptr::addr_of_mut!(POOL_READ_BUF);
        let write_buf: &'static mut [u8] = &mut *core::ptr::addr_of_mut!(POOL_WRITE_BUF);

        let mut tls: PooledTls = TlsConnection::new(transport, read_buf, write_buf);

        // TLS 1.3 handshake (this is the expensive part — ~10s)
        let rng = RdRandRng;
        let config = TlsConfig::new().with_server_name(hostname);
        let provider = UnsecureProvider::new::<Aes128GcmSha256>(rng);
        tls.open(TlsContext::new(&config, provider))
            .map_err(|_| "TLS handshake failed")?;

        let result = pool_send_and_recv(&mut tls, request_bytes);

        if result.is_ok() {
            // Cache connection for reuse
            POOL_TLS = Box::into_raw(Box::new(tls));
            let bytes = hostname.as_bytes();
            let len = bytes.len().min(128);
            POOL_HOSTNAME[..len].copy_from_slice(&bytes[..len]);
            POOL_HOSTNAME_LEN = len;
        }

        ACTIVE_STACK = core::ptr::null_mut();
        result
    }
}

/// Send an HTTP request and read the full HTTP response over a TLS connection.
/// Uses Content-Length or chunked encoding to determine when the response
/// is complete (needed for keep-alive — server doesn't close the connection).
fn pool_send_and_recv(
    tls: &mut PooledTls,
    request_bytes: &[u8],
) -> Result<Vec<u8>, &'static str> {
    // Send the full HTTP request
    let mut offset = 0;
    while offset < request_bytes.len() {
        let n = tls
            .write(&request_bytes[offset..])
            .map_err(|_| "TLS write failed")?;
        if n == 0 {
            return Err("TLS: closed during write");
        }
        offset += n;
    }
    tls.flush().map_err(|_| "TLS flush failed")?;

    // Read HTTP response with proper framing
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = kernel_ms() + 30_000;

    // Phase 1: read until we have complete headers (\r\n\r\n)
    loop {
        match tls.read(&mut buf) {
            Ok(0) => return Err("connection closed before headers"),
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(_) => return Err("TLS read error"),
        }
        if find_header_end(&data).is_some() {
            break;
        }
        if kernel_ms() > deadline {
            return Err("timeout reading headers");
        }
    }

    let header_end = find_header_end(&data).unwrap();
    let body_start = header_end + 4;
    let headers = core::str::from_utf8(&data[..header_end]).unwrap_or("");

    // Phase 2: read body based on framing
    let content_length = pool_parse_content_length(headers);
    let is_chunked = pool_header_has(headers, "transfer-encoding", "chunked");

    if let Some(cl) = content_length {
        // Read exactly Content-Length bytes of body
        while data.len() - body_start < cl {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
            if kernel_ms() > deadline {
                break;
            }
        }
    } else if is_chunked {
        // Read until final chunk marker (0\r\n\r\n)
        while !pool_has_final_chunk(&data[body_start..]) {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
            if kernel_ms() > deadline {
                break;
            }
        }
    } else {
        // Unknown framing — read with short timeout
        let short = kernel_ms() + 3_000;
        loop {
            match tls.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
            if kernel_ms() > short {
                break;
            }
        }
    }

    Ok(data)
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn pool_parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.split("\r\n") {
        if line.len() > 15 {
            let prefix = &line[..15];
            if prefix.eq_ignore_ascii_case("content-length:") {
                return line[15..].trim().parse().ok();
            }
        }
        if line.len() > 16 {
            let prefix = &line[..16];
            if prefix.eq_ignore_ascii_case("content-length: ") {
                return line[16..].trim().parse().ok();
            }
        }
    }
    None
}

fn pool_header_has(headers: &str, name: &str, value: &str) -> bool {
    for line in headers.split("\r\n") {
        if line.len() > name.len() + 1 {
            let (key_part, rest) = line.split_at(name.len());
            if key_part.eq_ignore_ascii_case(name) && rest.starts_with(':') {
                if rest[1..].trim().eq_ignore_ascii_case(value) {
                    return true;
                }
            }
        }
    }
    false
}

fn pool_has_final_chunk(body: &[u8]) -> bool {
    if body.len() < 5 {
        return false;
    }
    body.windows(5).any(|w| w == b"0\r\n\r\n")
}
