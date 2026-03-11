//! HTTP/1.1 client — POST over TCP (plain) or TLS 1.3 (encrypted).
//!
//! This is the highest layer of the network stack. It composes:
//!   NetworkStack (TCP/IP) + optional TLS 1.3 + HTTP protocol
//!
//! For http:// URLs: sends over plain TCP.
//! For https:// URLs: establishes TLS 1.3 via embedded-tls, then sends
//! the HTTP request over the encrypted channel.
//!
//! Primary use case: calling AI inference endpoints (OpenAI, Anthropic, etc).

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
    /// For https://, a TLS 1.3 handshake is performed before sending data.
    /// `headers` are additional HTTP headers (e.g. Authorization, x-api-key).
    /// `body` is the raw JSON payload bytes.
    pub fn post_json(
        &self,
        stack: &mut NetworkStack,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<HttpResponse, &'static str> {
        // 1. Parse the URL into hostname, port, path, and whether it's HTTPS
        let (hostname, port, path, is_https) = parse_url(url)?;

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

        // Combine headers and body into a single buffer for TLS path
        let mut full_request = request.into_bytes();
        full_request.extend_from_slice(body);

        let result = if is_https {
            // 5a. HTTPS: perform TLS handshake and send/receive encrypted
            let response_bytes = crate::tls::tls_post(
                stack,
                tcp_handle,
                hostname,
                &full_request,
            )?;
            parse_http_response(&response_bytes)
        } else {
            // 5b. HTTP: send over plain TCP
            tcp_send_all(stack, tcp_handle, &full_request)?;

            // 6. Read the full response
            let response_bytes = tcp_read_to_end(stack, tcp_handle)?;
            parse_http_response(&response_bytes)
        };

        // Clean up the TCP socket so it doesn't linger in the socket set
        stack.tcp_close(tcp_handle);

        result
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

/// Parse a URL into (hostname, port, path, is_https).
///
/// Supports both "http://" and "https://" schemes.
///   "http://localhost:8080/v1/chat"  → ("localhost", 8080, "/v1/chat", false)
///   "https://api.openai.com/v1/chat" → ("api.openai.com", 443, "/v1/chat", true)
fn parse_url(url: &str) -> Result<(&str, u16, &str, bool), &'static str> {
    let (url_body, default_port, is_https) =
        if let Some(rest) = url.strip_prefix("https://") {
            (rest, 443u16, true)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (rest, 80u16, false)
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

    Ok((hostname, port, path, is_https))
}

/// Parse a raw HTTP response into status code + body.
///
/// Handles chunked transfer encoding by reassembling chunks into a
/// contiguous body. Each chunk is: `<hex-size>\r\n<data>\r\n`, ending
/// with a zero-length chunk `0\r\n\r\n`.
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

    // Check if response uses chunked transfer encoding
    let is_chunked = header_str
        .lines()
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("transfer-encoding") && lower.contains("chunked")
        });

    let body = if is_chunked {
        decode_chunked(body_str)
    } else {
        body_str.as_bytes().to_vec()
    };

    Ok(HttpResponse {
        status_code,
        body,
    })
}

/// Decode a chunked HTTP response body.
///
/// Format: `<hex-size>\r\n<data>\r\n<hex-size>\r\n<data>\r\n0\r\n\r\n`
fn decode_chunked(body: &str) -> Vec<u8> {
    let mut result = Vec::new();
    let mut remaining = body;

    loop {
        // Find the chunk size line
        let size_end = match remaining.find("\r\n") {
            Some(pos) => pos,
            None => break,
        };
        let size_str = remaining[..size_end].trim();
        // Chunk size might have extensions after ';', ignore them
        let size_hex = size_str.split(';').next().unwrap_or("0").trim();
        let chunk_size = match usize::from_str_radix(size_hex, 16) {
            Ok(s) => s,
            Err(_) => break,
        };

        if chunk_size == 0 {
            break; // Final chunk
        }

        let data_start = size_end + 2; // skip \r\n after size
        let data_end = data_start + chunk_size;
        if data_end > remaining.len() {
            // Partial chunk — take what we have
            result.extend_from_slice(remaining[data_start..].as_bytes());
            break;
        }

        result.extend_from_slice(remaining[data_start..data_end].as_bytes());
        // Skip past chunk data + trailing \r\n
        remaining = if data_end + 2 <= remaining.len() {
            &remaining[data_end + 2..]
        } else {
            break;
        };
    }

    result
}

fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
