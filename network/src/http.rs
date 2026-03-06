//! HTTP/1.1 client — POST over TCP for AI API calls.
//!
//! This is the highest layer of the network stack. It composes:
//!   NetworkStack (TCP/IP) + HTTP protocol
//!
//! Supports both http:// and https:// URLs. For the MVP, https:// URLs
//! are handled by connecting to port 443 over plain TCP — this works when
//! QEMU's user-mode networking proxies to a TLS-terminating endpoint on
//! the host, or when connecting to localhost services.
//!
//! Primary use case: calling AI inference endpoints (local or proxied).
//!
//! TLS encryption will be added in Stage 2 when the ring crypto library
//! can be cross-compiled to bare-metal x86_64.

use alloc::{format, vec, vec::Vec};
use smoltcp::socket::tcp;

use crate::stack::NetworkStack;

/// Reusable HTTP client.
pub struct HttpClient;

/// A parsed HTTP response.
pub struct HttpResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}

impl HttpClient {
    pub fn new() -> Self {
        Self
    }

    /// Send an HTTP POST request with a JSON body.
    ///
    /// Supports both "http://" and "https://" URLs.
    /// `headers` are additional HTTP headers (e.g. Authorization, x-api-key).
    /// `body` is the raw JSON payload bytes.
    pub fn post_json(
        &self,
        stack: &mut NetworkStack,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<HttpResponse, &'static str> {
        // 1. Parse the URL into hostname, port, path
        let (hostname, port, path) = parse_url(url)?;

        // 2. DNS resolve: hostname → IPv4 address
        let ip = stack.resolve(hostname)?;

        // 3. TCP connect: 3-way handshake
        let tcp_handle = stack.tcp_connect(ip, port)?;

        // 4. Build the HTTP/1.1 request
        let mut request = format!(
            "POST {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n",
            path, hostname, body.len()
        );
        for (key, value) in headers {
            request.push_str(&format!("{}: {}\r\n", key, value));
        }
        request.push_str("Connection: close\r\n\r\n");

        // 5. Send request headers
        tcp_send_all(stack, tcp_handle, request.as_bytes())?;

        // 6. Send request body
        tcp_send_all(stack, tcp_handle, body)?;

        // 7. Read the full response
        let response_bytes = tcp_read_to_end(stack, tcp_handle)?;
        parse_http_response(&response_bytes)
    }
}

/// Send all bytes over a TCP socket (blocking until complete).
fn tcp_send_all(
    stack: &mut NetworkStack,
    handle: smoltcp::iface::SocketHandle,
    data: &[u8],
) -> Result<(), &'static str> {
    let mut sent = 0;
    let deadline = kernel_ms() + 10_000;

    while sent < data.len() {
        stack.poll();
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        if socket.can_send() {
            match socket.send_slice(&data[sent..]) {
                Ok(n) => sent += n,
                Err(_) => return Err("TCP send failed"),
            }
        }
        if kernel_ms() > deadline {
            return Err("TCP send timeout");
        }
        if sent < data.len() {
            x86_64::instructions::hlt();
        }
    }
    Ok(())
}

/// Read all data from a TCP socket until the remote end closes it.
fn tcp_read_to_end(
    stack: &mut NetworkStack,
    handle: smoltcp::iface::SocketHandle,
) -> Result<Vec<u8>, &'static str> {
    let mut result = Vec::new();
    let deadline = kernel_ms() + 30_000;

    loop {
        stack.poll();

        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        if socket.can_recv() {
            let mut buf = vec![0u8; 4096];
            match socket.recv_slice(&mut buf) {
                Ok(n) if n > 0 => {
                    result.extend_from_slice(&buf[..n]);
                }
                _ => {}
            }
        }

        // Check if connection is closed or half-closed
        let state = socket.state();
        if !socket.is_open() {
            break;
        }
        if !result.is_empty()
            && matches!(
                state,
                tcp::State::CloseWait | tcp::State::Closing | tcp::State::LastAck
            )
        {
            break;
        }

        if kernel_ms() > deadline {
            if result.is_empty() {
                return Err("HTTP response timeout");
            }
            break;
        }

        x86_64::instructions::hlt();
    }

    Ok(result)
}

/// Parse a URL into (hostname, port, path).
///
/// Supports both "http://" and "https://" schemes.
///   "http://localhost:8080/v1/chat"  → ("localhost", 8080, "/v1/chat")
///   "https://api.openai.com/v1/chat" → ("api.openai.com", 443, "/v1/chat")
fn parse_url(url: &str) -> Result<(&str, u16, &str), &'static str> {
    let (url_body, default_port) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, 443u16)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, 80u16)
    } else {
        return Err("URL must start with http:// or https://");
    };

    let (host_and_port, path) = match url_body.find('/') {
        Some(idx) => (&url_body[..idx], &url_body[idx..]),
        None => (url_body, "/"),
    };

    let (hostname, port) = match host_and_port.find(':') {
        Some(idx) => {
            let port: u16 = host_and_port[idx + 1..]
                .parse()
                .map_err(|_| "Invalid port in URL")?;
            (&host_and_port[..idx], port)
        }
        None => (host_and_port, default_port),
    };

    Ok((hostname, port, path))
}

/// Parse a raw HTTP response into status code + body.
fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, &'static str> {
    let response = core::str::from_utf8(bytes).map_err(|_| "Response is not valid UTF-8")?;
    let (header_str, body_str) = response
        .split_once("\r\n\r\n")
        .ok_or("Malformed HTTP response: no header/body boundary")?;

    let first_line = header_str
        .lines()
        .next()
        .ok_or("Empty HTTP response")?;
    let status_code: u16 = first_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("Invalid HTTP status code")?;

    Ok(HttpResponse {
        status_code,
        body: body_str.as_bytes().to_vec(),
    })
}

fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
