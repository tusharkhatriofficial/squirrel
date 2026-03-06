//! Cloud API inference backend — sends prompts to OpenAI, Anthropic, or Gemini.
//!
//! This backend makes HTTP POST requests to cloud AI APIs using the network
//! crate's HttpClient. It builds provider-specific URLs, headers, and JSON
//! request bodies, then parses provider-specific response formats.
//!
//! For the MVP, requests go over plain HTTP (not HTTPS) because TLS requires
//! the ring crypto library which can't compile on bare metal x86_64 yet.
//! This works because QEMU's user-mode networking can proxy to a
//! TLS-terminating endpoint on the host machine.
//!
//! Each provider has a different API format:
//!   - OpenAI:    POST /v1/chat/completions with Bearer token
//!   - Anthropic: POST /v1/messages with x-api-key header
//!   - Gemini:    POST /v1beta/models/{model}:generateContent with key in URL
//!   - Custom:    OpenAI-compatible format with user-specified base URL

use alloc::{format, string::String, vec::Vec};

use crate::backend::{
    FinishReason, InferenceBackend, InferenceError, InferenceRequest, InferenceResponse,
};

/// Which cloud AI provider to use.
#[derive(Debug, Clone, Copy)]
pub enum ApiProvider {
    /// OpenAI (GPT-4, GPT-4o, etc.)
    OpenAi,
    /// Anthropic (Claude models)
    Anthropic,
    /// Google Gemini
    Gemini,
    /// Any OpenAI-compatible API (e.g. local proxy, Together AI)
    Custom,
}

/// Cloud API inference backend.
///
/// Holds the provider type, API key, model ID, and base URL.
/// Created fresh for each inference request by the InferenceRouter,
/// so settings changes take effect immediately.
pub struct ApiInferenceBackend {
    provider: ApiProvider,
    api_key: String,
    model_id: String,
    base_url: String,
}

impl ApiInferenceBackend {
    /// Create a new API backend with the given configuration.
    pub fn new(
        provider: ApiProvider,
        api_key: String,
        model_id: String,
        base_url: String,
    ) -> Self {
        Self {
            provider,
            api_key,
            model_id,
            base_url,
        }
    }

    /// Build the full API endpoint URL for this provider.
    fn build_url(&self) -> String {
        match self.provider {
            ApiProvider::OpenAi => {
                "http://api.openai.com/v1/chat/completions".into()
            }
            ApiProvider::Anthropic => {
                "http://api.anthropic.com/v1/messages".into()
            }
            ApiProvider::Gemini => format!(
                "http://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                self.model_id, self.api_key
            ),
            ApiProvider::Custom => self.base_url.clone(),
        }
    }

    /// Build the HTTP headers required by this provider.
    ///
    /// Each provider authenticates differently:
    ///   - OpenAI/Custom: Bearer token in Authorization header
    ///   - Anthropic: x-api-key header + anthropic-version header
    ///   - Gemini: API key is in the URL, no auth headers needed
    fn build_headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        match self.provider {
            ApiProvider::OpenAi | ApiProvider::Custom => {
                headers.push((
                    "Authorization".into(),
                    format!("Bearer {}", self.api_key),
                ));
            }
            ApiProvider::Anthropic => {
                headers.push(("x-api-key".into(), self.api_key.clone()));
                headers.push(("anthropic-version".into(), "2023-06-01".into()));
            }
            ApiProvider::Gemini => {} // Key is in the URL
        }
        headers
    }

    /// Build the JSON request body for this provider.
    ///
    /// Each provider expects a different JSON format:
    ///   - OpenAI: {"model": "...", "messages": [{"role": "user", "content": "..."}], "max_tokens": N}
    ///   - Anthropic: {"model": "...", "max_tokens": N, "messages": [{"role": "user", "content": "..."}]}
    ///   - Gemini: {"contents": [{"parts": [{"text": "..."}]}]}
    ///
    /// We build JSON manually (no serde_json on no_std) using format!().
    /// Special characters in the prompt are escaped to keep the JSON valid.
    fn build_body(&self, request: &InferenceRequest) -> Vec<u8> {
        // Escape special JSON characters in the prompt
        let escaped = request
            .prompt
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");

        let body = match self.provider {
            ApiProvider::OpenAi | ApiProvider::Custom => format!(
                r#"{{"model":"{}","messages":[{{"role":"user","content":"{}"}}],"max_tokens":{},"temperature":{}}}"#,
                self.model_id, escaped, request.max_tokens, request.temperature
            ),
            ApiProvider::Anthropic => format!(
                r#"{{"model":"{}","max_tokens":{},"messages":[{{"role":"user","content":"{}"}}]}}"#,
                self.model_id, request.max_tokens, escaped
            ),
            ApiProvider::Gemini => format!(
                r#"{{"contents":[{{"parts":[{{"text":"{}"}}]}}]}}"#,
                escaped
            ),
        };
        body.into_bytes()
    }

    /// Parse the AI-generated text from the provider's JSON response.
    ///
    /// Each provider returns text in a different JSON field:
    ///   - OpenAI: choices[0].message.content
    ///   - Anthropic: content[0].text
    ///   - Gemini: candidates[0].content.parts[0].text
    ///
    /// We use naive string searching (find the key, extract the value)
    /// rather than a full JSON parser. This is fragile but works for
    /// well-formed API responses and avoids pulling in a JSON parser dep.
    fn parse_response_text(&self, body: &[u8]) -> Result<String, InferenceError> {
        let json_str = core::str::from_utf8(body)
            .map_err(|_| InferenceError::ApiError("Invalid UTF-8 response".into()))?;

        let text = match self.provider {
            ApiProvider::OpenAi | ApiProvider::Custom => {
                // OpenAI puts content in: "content": "..."
                extract_json_string(json_str, "\"content\":")
            }
            ApiProvider::Anthropic | ApiProvider::Gemini => {
                // Anthropic and Gemini both use: "text": "..."
                extract_json_string(json_str, "\"text\":")
            }
        };

        text.ok_or_else(|| {
            // Include a truncated version of the response for debugging
            let preview = &json_str[..json_str.len().min(200)];
            InferenceError::ApiError(format!("Could not parse response: {}", preview))
        })
    }

    /// Human-readable provider name (for logging and Glass Box display).
    fn provider_name(&self) -> &str {
        match self.provider {
            ApiProvider::OpenAi => "openai",
            ApiProvider::Anthropic => "anthropic",
            ApiProvider::Gemini => "gemini",
            ApiProvider::Custom => "custom",
        }
    }
}

/// Extract the first string value after a JSON key.
///
/// This is a naive JSON extractor that finds a key like `"content":` and
/// extracts the string value that follows it. It handles basic escape
/// sequences (\n, \", \\) but won't handle deeply nested or complex JSON.
///
/// Example: extract_json_string(r#"{"content": "hello"}"#, "\"content\":")
///          → Some("hello")
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pos = json.find(key)?;
    let after_key = &json[pos + key.len()..];
    let after_ws = after_key.trim_start();

    // The value should start with a quote
    let content = after_ws.strip_prefix('"')?;

    // Find the closing quote (handling escape sequences)
    let mut result = String::new();
    let mut chars = content.chars();
    loop {
        match chars.next()? {
            '\\' => {
                // Escaped character
                match chars.next()? {
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    '/' => result.push('/'),
                    other => {
                        result.push('\\');
                        result.push(other);
                    }
                }
            }
            '"' => return Some(result), // End of string
            c => result.push(c),
        }
    }
}

impl InferenceBackend for ApiInferenceBackend {
    fn generate(
        &mut self,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        // Check that the network stack is ready
        if !network::is_ready() {
            return Err(InferenceError::NetworkUnavailable);
        }

        let start_ms = kernel_ms();

        crate::println!("[Inference] Calling {} API (model: {})...",
            self.provider_name(), self.model_id);

        let url = self.build_url();
        let headers = self.build_headers();
        let body = self.build_body(request);

        // Convert headers to (&str, &str) slices for HttpClient
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // Get the global network stack and make the HTTP POST request
        let net_stack = network::NETWORK_STACK
            .get()
            .ok_or(InferenceError::NetworkUnavailable)?;
        let mut stack = net_stack.lock();

        let http_client = network::http::HttpClient::new();
        let response = http_client
            .post_json(&mut stack, &url, &header_refs, &body)
            .map_err(|e| InferenceError::ApiError(format!("HTTP error: {}", e)))?;

        // Check for HTTP errors
        if response.status_code != 200 {
            let err_body = core::str::from_utf8(&response.body).unwrap_or("(binary)");
            let preview = &err_body[..err_body.len().min(200)];
            return Err(InferenceError::ApiError(format!(
                "HTTP {} — {}",
                response.status_code, preview
            )));
        }

        // Parse the AI-generated text from the response
        let text = self.parse_response_text(&response.body)?;

        let latency = kernel_ms() - start_ms;
        crate::println!(
            "[Inference] {} responded in {}ms ({} chars)",
            self.provider_name(),
            latency,
            text.len()
        );

        Ok(InferenceResponse {
            text,
            tokens_generated: 0, // Cloud APIs don't always report this
            finish_reason: FinishReason::EndOfSequence,
            backend_used: format!("{} ({})", self.provider_name(), self.model_id),
            latency_ms: latency,
        })
    }

    fn name(&self) -> &str {
        &self.model_id
    }

    fn is_available(&self) -> bool {
        network::is_ready() && !self.api_key.is_empty()
    }
}

/// Get the current kernel time in milliseconds.
fn kernel_ms() -> u64 {
    extern "Rust" {
        fn kernel_milliseconds() -> u64;
    }
    unsafe { kernel_milliseconds() }
}
