//! Network Agent — a SART agent that handles network.http.post intents.
//!
//! This agent is the bridge between Squirrel's Intent Bus and the network
//! stack. Other agents (like the AI inference engine) don't talk to TCP/IP
//! directly — they send a "network.http.post" intent with a URL, headers,
//! and body, and the NetworkAgent handles the entire request lifecycle.
//!
//! This is the Squirrel way: everything goes through the Intent Bus, making
//! all network activity visible in the Glass Box audit log.

use alloc::{string::String, vec::Vec};
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use serde::{Deserialize, Serialize};

/// Payload for a "network.http.post" intent.
///
/// Sent by any agent that needs to make an HTTPS POST request.
/// The NetworkAgent receives this, makes the request, and sends
/// back a NetworkResponse via an intent response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkRequest {
    /// Full HTTPS URL (e.g. "https://api.openai.com/v1/chat/completions")
    pub url: String,
    /// HTTP headers as key-value pairs (e.g. [("Authorization", "Bearer sk-...")])
    pub headers: Vec<(String, String)>,
    /// Raw request body (typically JSON)
    pub body: Vec<u8>,
}

/// Payload for a "network.http.post.response" intent.
///
/// Sent by the NetworkAgent back to the requester after the HTTP
/// request completes (or fails).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkResponse {
    /// HTTP status code (200, 401, 500, etc.) — 0 if request failed
    pub status_code: u16,
    /// Response body bytes
    pub body: Vec<u8>,
    /// Error message if the request failed (None on success)
    pub error: Option<String>,
}

/// The Network Agent — handles all outbound HTTP requests.
///
/// Registered with SART at boot time. Subscribes to "network.http.post"
/// intents. On each poll, it also drives the network stack to process
/// incoming/outgoing packets.
pub struct NetworkAgent {
    /// Whether we've logged our startup message
    started: bool,
}

impl NetworkAgent {
    pub fn new() -> Self {
        Self { started: false }
    }
}

impl Agent for NetworkAgent {
    fn name(&self) -> &str {
        "network-agent"
    }

    fn priority(&self) -> CognitivePriority {
        // Active priority — network I/O should be processed promptly
        CognitivePriority::Active
    }

    fn on_start(&mut self, _ctx: &AgentContext) {
        self.started = true;
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // Always drive the network stack to process packets.
        // This is critical — smoltcp needs frequent polling to handle
        // TCP retransmissions, keepalives, DHCP renewals, etc.
        {
            if let Some(stack) = crate::NETWORK_STACK.get() {
                stack.lock().poll();
            }
        }

        // Check for pending HTTP request intents
        if let Some(intent) = ctx.bus.try_recv() {
            if intent.semantic_type.matches("network.http.post") {
                if let Ok(req) = intent.decode::<NetworkRequest>() {
                    // Convert header pairs to borrowed slices for the HTTP client
                    let header_refs: Vec<(&str, &str)> = req
                        .headers
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect();

                    let response = if let Some(stack) = crate::NETWORK_STACK.get() {
                        let mut stack = stack.lock();
                        let http = crate::http::HttpClient::new();
                        match http.post_json(&mut stack, &req.url, &header_refs, &req.body) {
                            Ok(resp) => NetworkResponse {
                                status_code: resp.status_code,
                                body: resp.body,
                                error: None,
                            },
                            Err(e) => NetworkResponse {
                                status_code: 0,
                                body: Vec::new(),
                                error: Some(String::from(e)),
                            },
                        }
                    } else {
                        NetworkResponse {
                            status_code: 0,
                            body: Vec::new(),
                            error: Some(String::from("Network stack not initialized")),
                        }
                    };

                    // Send the response back via Intent Bus
                    let reply =
                        intent_bus::Intent::response(&intent, "network-agent", &response);
                    ctx.bus.send(reply);

                    return AgentPoll::Yield;
                }
            }
        }

        AgentPoll::Pending
    }
}
