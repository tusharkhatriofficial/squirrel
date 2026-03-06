//! HTTP/1.1 client — HTTPS POST over TLS for AI API calls.
//!
//! This is the highest layer of the network stack. It composes:
//!   NetworkStack (TCP/IP) + TlsClient (encryption) + HTTP protocol
//!
//! The primary use case is calling cloud AI inference APIs:
//!   - OpenAI: POST https://api.openai.com/v1/chat/completions
//!   - Anthropic: POST https://api.anthropic.com/v1/messages
//!   - Google: POST https://generativelanguage.googleapis.com/...
//!
//! For the MVP, we only implement POST with JSON bodies (which is what
//! every AI API uses). GET, PUT, etc. can be added in Stage 2.

use alloc::{format, string::String, vec::Vec};

use crate::stack::NetworkStack;
use crate::tls::TlsClient;

/// Reusable HTTP client (holds TLS config).
pub struct HttpClient {
    tls: TlsClient,
}

/// A parsed HTTP response.
pub struct HttpResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}

impl HttpClient {
    /// Create a new HTTP client.
    /// Initializes TLS with Mozilla's root CA certificates.
    pub fn new() -> Self {
        Self {
            tls: TlsClient::new(),
        }
    }

    /// Send an HTTPS POST request with a JSON body.
    ///
    /// `url` must start with "https://".
    /// `headers` are additional HTTP headers (e.g. Authorization, x-api-key).
    /// `body` is the raw JSON payload bytes.
    ///
    /// Returns the HTTP response with status code and body.
    ///
    /// Example for OpenAI:
    /// ```ignore
    /// let resp = client.post_json(
    ///     &mut stack,
    ///     "https://api.openai.com/v1/chat/completions",
    ///     &[("Authorization", "Bearer sk-...")],
    ///     b"{\"model\":\"gpt-4\",\"messages\":[...]}",
    /// )?;
    /// ```
    pub fn post_json(
        &self,
        stack: &mut NetworkStack,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<HttpResponse, &'static str> {
        // 1. Parse the URL into hostname, port, path
        let (hostname, port, path) = parse_https_url(url)?;

        // 2. DNS resolve: hostname → IPv4 address
        let ip = stack.resolve(hostname)?;

        // 3. TCP connect: open a socket to the server
        let tcp_handle = stack.tcp_connect(ip, port)?;

        // 4. TLS handshake: establish encrypted channel
        let mut tls = self.tls.connect(hostname, stack, tcp_handle)?;

        // 5. Build the HTTP/1.1 request
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

        // 6. Send the request headers + body
        tls.write_all(request.as_bytes(), stack)?;
        tls.write_all(body, stack)?;

        // 7. Read the response
        let response_bytes = tls.read_to_end(stack)?;
        parse_http_response(&response_bytes)
    }
}

/// Parse an HTTPS URL into (hostname, port, path).
///
/// Examples:
///   "https://api.openai.com/v1/chat" → ("api.openai.com", 443, "/v1/chat")
///   "https://localhost:8443/test"     → ("localhost", 8443, "/test")
fn parse_https_url(url: &str) -> Result<(&str, u16, &str), &'static str> {
    let url = url
        .strip_prefix("https://")
        .ok_or("Not an HTTPS URL")?;

    let (host_and_port, path) = match url.find('/') {
        Some(idx) => (&url[..idx], &url[idx..]),
        None => (url, "/"),
    };

    let (hostname, port) = match host_and_port.find(':') {
        Some(idx) => {
            let port: u16 = host_and_port[idx + 1..]
                .parse()
                .map_err(|_| "Invalid port in URL")?;
            (&host_and_port[..idx], port)
        }
        None => (host_and_port, 443u16),
    };

    Ok((hostname, port, path))
}

/// Parse a raw HTTP response into status code + body.
///
/// HTTP/1.1 responses look like:
///   HTTP/1.1 200 OK\r\n
///   Content-Type: application/json\r\n
///   \r\n
///   {"choices":[...]}
///
/// We split on the blank line (\r\n\r\n) to separate headers from body,
/// then extract the status code from the first line.
fn parse_http_response(bytes: &[u8]) -> Result<HttpResponse, &'static str> {
    // Find the header/body boundary
    let response = core::str::from_utf8(bytes).map_err(|_| "Response is not valid UTF-8")?;
    let (header_str, body_str) = response
        .split_once("\r\n\r\n")
        .ok_or("Malformed HTTP response: no header/body boundary")?;

    // Parse status code from first line: "HTTP/1.1 200 OK"
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
