//! TLS 1.2/1.3 client — rustls over smoltcp TCP sockets.
//!
//! In a normal OS, rustls reads/writes through std::io Read/Write traits
//! on a TCP stream. We don't have std::io in a bare-metal kernel. Instead,
//! we manually shuttle encrypted bytes between rustls and the smoltcp TCP
//! socket using rustls's lower-level read_tls/write_tls API.
//!
//! The flow for sending data:
//!   1. Write plaintext into rustls via writer()
//!   2. rustls encrypts it internally
//!   3. We pull encrypted bytes out via write_tls() into a temp buffer
//!   4. We push that buffer into the smoltcp TCP socket
//!
//! The flow for receiving data:
//!   1. Pull encrypted bytes from the smoltcp TCP socket
//!   2. Feed them to rustls via read_tls()
//!   3. rustls decrypts internally
//!   4. We read plaintext out via reader()

use alloc::{string::String, sync::Arc, vec, vec::Vec};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;

use crate::stack::NetworkStack;

/// Reusable TLS client configuration.
///
/// Holds the root CA certificate store and rustls config. Creating this
/// once and reusing it avoids re-parsing the ~150 Mozilla root CAs for
/// every connection.
pub struct TlsClient {
    config: Arc<ClientConfig>,
}

impl TlsClient {
    /// Create a new TLS client with Mozilla's root CA certificates.
    ///
    /// These certificates are embedded in the kernel binary via the
    /// webpki-roots crate. They allow us to verify the TLS certificates
    /// of AI API servers (api.openai.com, api.anthropic.com, etc.)
    pub fn new() -> Self {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self {
            config: Arc::new(config),
        }
    }

    /// Start a TLS session over an existing TCP connection.
    ///
    /// `hostname` is used for SNI (Server Name Indication) — the server
    /// needs to know which certificate to present. The TCP socket must
    /// already be in the Established state.
    pub fn connect(
        &self,
        hostname: &str,
        stack: &mut NetworkStack,
        tcp_handle: SocketHandle,
    ) -> Result<TlsSession, &'static str> {
        let server_name = ServerName::try_from(String::from(hostname))
            .map_err(|_| "Invalid TLS hostname")?;
        let conn = ClientConnection::new(self.config.clone(), server_name)
            .map_err(|_| "TLS client init failed")?;

        let mut session = TlsSession {
            conn,
            tcp_handle,
        };

        // Drive the TLS handshake to completion.
        // rustls generates ClientHello → we send it → server responds
        // with ServerHello + Certificate → we verify → handshake done.
        session.handshake(stack)?;

        Ok(session)
    }
}

/// An active TLS session over a smoltcp TCP socket.
///
/// Provides write_all() and read_to_end() for the HTTP client to use.
/// All I/O is synchronous and blocking (with HLT-based waiting).
pub struct TlsSession {
    conn: ClientConnection,
    tcp_handle: SocketHandle,
}

impl TlsSession {
    /// Complete the TLS handshake (blocking, 10s timeout).
    fn handshake(&mut self, stack: &mut NetworkStack) -> Result<(), &'static str> {
        let deadline = kernel_ms() + 10_000;

        loop {
            // Send any pending TLS data (ClientHello, etc.)
            self.flush_to_tcp(stack)?;

            // If handshake is complete, we're done
            if !self.conn.is_handshaking() {
                return Ok(());
            }

            // Read incoming TLS data from TCP socket
            self.read_from_tcp(stack)?;

            // Process the TLS state machine
            if self.conn.process_new_packets().is_err() {
                return Err("TLS handshake error");
            }

            if kernel_ms() > deadline {
                return Err("TLS handshake timeout");
            }

            stack.poll();
            x86_64::instructions::hlt();
        }
    }

    /// Write all bytes over TLS (blocking until complete).
    pub fn write_all(
        &mut self,
        data: &[u8],
        stack: &mut NetworkStack,
    ) -> Result<(), &'static str> {
        // Feed plaintext to rustls
        self.conn.writer().write_all(data).map_err(|_| "TLS write error")?;
        // Flush encrypted output to TCP
        self.flush_to_tcp(stack)
    }

    /// Read all available response data (blocking until connection closes).
    ///
    /// Returns when the remote end closes the connection or sends all data
    /// and enters CloseWait state. Times out after 30 seconds.
    pub fn read_to_end(
        &mut self,
        stack: &mut NetworkStack,
    ) -> Result<Vec<u8>, &'static str> {
        let mut result = Vec::new();
        let deadline = kernel_ms() + 30_000;

        loop {
            stack.poll();

            // Pull encrypted data from TCP → feed to rustls
            self.read_from_tcp(stack)?;

            // Let rustls decrypt
            if self.conn.process_new_packets().is_err() {
                return Err("TLS decryption error");
            }

            // Read decrypted plaintext
            let mut buf = vec![0u8; 8192];
            loop {
                match self.conn.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => result.extend_from_slice(&buf[..n]),
                    Err(ref e) if e.kind() == core::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }

            // Send any TLS alerts or close-notify
            let _ = self.flush_to_tcp(stack);

            // Check if we're done
            let socket = stack.sockets.get::<tcp::Socket>(self.tcp_handle);
            if !socket.is_open() {
                break;
            }
            if !result.is_empty() && socket.state() == tcp::State::CloseWait {
                break;
            }
            if kernel_ms() > deadline {
                return Err("TLS read timeout");
            }

            x86_64::instructions::hlt();
        }

        Ok(result)
    }

    /// Pull encrypted bytes from rustls and push them into the TCP socket.
    fn flush_to_tcp(&mut self, stack: &mut NetworkStack) -> Result<(), &'static str> {
        while self.conn.wants_write() {
            let mut buf = vec![0u8; 8192];
            let mut cursor = &mut buf[..] as &mut [u8];
            let n = self.conn.write_tls(&mut cursor).map_err(|_| "TLS write_tls error")?;
            if n == 0 {
                break;
            }

            // Push to TCP socket (with backpressure handling)
            let mut sent = 0;
            let deadline = kernel_ms() + 5_000;
            while sent < n {
                stack.poll();
                let socket = stack.sockets.get_mut::<tcp::Socket>(self.tcp_handle);
                if socket.can_send() {
                    match socket.send_slice(&buf[sent..n]) {
                        Ok(bytes_sent) => sent += bytes_sent,
                        Err(_) => return Err("TCP send failed"),
                    }
                }
                if kernel_ms() > deadline {
                    return Err("TCP send timeout");
                }
                if sent < n {
                    x86_64::instructions::hlt();
                }
            }
        }
        Ok(())
    }

    /// Pull encrypted bytes from the TCP socket and feed them to rustls.
    fn read_from_tcp(&mut self, stack: &mut NetworkStack) -> Result<(), &'static str> {
        let socket = stack.sockets.get_mut::<tcp::Socket>(self.tcp_handle);
        if !socket.can_recv() {
            return Ok(());
        }

        socket
            .recv(|data| {
                if !data.is_empty() {
                    // Feed encrypted bytes to rustls
                    let mut slice = data;
                    let n = self.conn.read_tls(&mut slice).unwrap_or(0);
                    (n, ())
                } else {
                    (0, ())
                }
            })
            .map_err(|_| "TCP recv failed")?;

        Ok(())
    }
}

/// Get current kernel time in milliseconds.
fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
