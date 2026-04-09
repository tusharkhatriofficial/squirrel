//! Primary AI Agent — the top-level intelligence of Squirrel AIOS.
//!
//! This is the brain of the operating system. It receives natural language
//! input from the user, decides what to do (pattern match or AI inference),
//! and routes work to the appropriate capability modules.
//!
//! When the AI responds, it may include structured tool-call tags. The agent
//! parses these and executes real SVFS operations — creating files, reading
//! files, listing objects, searching by content. The AI decides what to do;
//! the agent executes it.
//!
//! State machine:
//!   WaitingForInput → user types a line
//!     ├─ pattern match → execute immediately → WaitingForInput
//!     └─ no match → send to inference engine → WaitingForInference
//!   WaitingForInference → AI response arrives
//!     ├─ contains tool call → execute against SVFS → display result
//!     └─ pure text → display it → WaitingForInput
//!   WaitingForModule → module signals completion → WaitingForInput

#![no_std]
extern crate alloc;

pub mod planner;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use sart::{Agent, AgentContext, AgentPoll, CognitivePriority};
use intent_bus::{Intent, IntentPriority, SemanticType};
use inference_engine::InferenceResponse;

use planner::{try_pattern_match, build_inference_request, parse_tool_calls, ToolCall};

/// Maximum conversation turns to keep (user + assistant pairs).
/// Each turn is ~100-300 tokens, so 6 turns ≈ 600-1800 extra tokens.
const MAX_HISTORY: usize = 6;

/// State machine for the Primary AI Agent.
#[derive(Debug, PartialEq)]
enum AgentState {
    /// Idle — waiting for user to type something.
    WaitingForInput,
    /// Sent an inference request — waiting for the AI to respond.
    WaitingForInference,
    /// Opened a module (settings, etc.) — waiting for it to close.
    WaitingForModule,
}

/// One turn in conversation history.
struct ChatTurn {
    user: String,
    assistant: String,
}

/// The Primary AI Agent — Squirrel's brain.
pub struct PrimaryAiAgent {
    state: AgentState,
    greeted: bool,
    /// Recent conversation history (ring buffer, oldest first).
    history: Vec<ChatTurn>,
    /// Stash the current user input so we can pair it with the AI response.
    pending_input: String,
}

impl PrimaryAiAgent {
    pub fn new() -> Self {
        Self {
            state: AgentState::WaitingForInput,
            greeted: false,
            history: Vec::new(),
            pending_input: String::new(),
        }
    }

    /// Record a completed turn and trim to MAX_HISTORY.
    fn push_history(&mut self, user: String, assistant: String) {
        // Truncate long responses to keep context manageable
        let trimmed_assistant: String = assistant.chars().take(300).collect();
        self.history.push(ChatTurn {
            user,
            assistant: trimmed_assistant,
        });
        while self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
    }

    /// Format conversation history for inclusion in the prompt.
    fn format_history(&self) -> String {
        if self.history.is_empty() {
            return String::new();
        }
        let mut out = String::from("\nCONVERSATION HISTORY:\n");
        for turn in &self.history {
            out += &format!("User: {}\nAssistant: {}\n", turn.user, turn.assistant);
        }
        out
    }

    /// Send raw text to the display module.
    fn print(&self, ctx: &AgentContext, text: &[u8]) {
        let intent = Intent {
            id: 0,
            reply_to: None,
            semantic_type: SemanticType::new("display.print"),
            sender: String::from("primary-agent"),
            payload: text.to_vec(),
            priority: IntentPriority::Normal,
            timestamp_ms: 0,
        };
        ctx.bus.send(intent);
    }

    /// Send a raw-payload intent (for WASM module communication).
    fn send_raw(&self, ctx: &AgentContext, intent_type: &str, payload: &[u8]) {
        let intent = Intent {
            id: 0,
            reply_to: None,
            semantic_type: SemanticType::new(intent_type),
            sender: String::from("primary-agent"),
            payload: payload.to_vec(),
            priority: IntentPriority::Normal,
            timestamp_ms: 0,
        };
        ctx.bus.send(intent);
    }

    /// Execute a tool call the AI decided to make, against real SVFS.
    fn execute_tool_call(&self, ctx: &AgentContext, tool: &ToolCall) {
        let svfs = match svfs::SVFS.get() {
            Some(s) => s,
            None => {
                self.print(ctx, b"[Error: SVFS not initialized]\n");
                return;
            }
        };

        match tool {
            ToolCall::CreateFile { ref name, ref tags, ref description, ref content } => {
                self.glass_box(ctx, "state", "svfs-write");

                // Delete existing file with same name (update = delete + create)
                let _ = svfs.delete_by_name(&name);

                // Build tag list: user tags + desc:description
                let mut tag_list: alloc::vec::Vec<&str> = alloc::vec!["user-created"];
                let tag_parts: alloc::vec::Vec<&str> = if tags.is_empty() {
                    alloc::vec![]
                } else {
                    tags.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()).collect()
                };
                for t in &tag_parts {
                    tag_list.push(t);
                }
                // Encode description as a special tag
                let desc_tag = if description.is_empty() {
                    String::new()
                } else {
                    format!("desc:{}", description)
                };
                if !desc_tag.is_empty() {
                    tag_list.push(&desc_tag);
                }

                match svfs.store(
                    content.as_bytes(),
                    svfs::ObjectType::Data,
                    Some(&name),
                    &tag_list,
                ) {
                    Ok(_hash) => {
                        let mut msg = format!(
                            "[SVFS] Stored '{}' ({} bytes)",
                            name, content.len()
                        );
                        if !tags.is_empty() {
                            msg += &format!(" [{}]", tags);
                        }
                        msg += "\n";
                        self.print(ctx, msg.as_bytes());
                    }
                    Err(_) => {
                        let msg = format!("[SVFS Error: failed to store '{}']\n", name);
                        self.print(ctx, msg.as_bytes());
                    }
                }
            }
            ToolCall::ReadFile { ref name } => {
                self.glass_box(ctx, "state", "svfs-read");
                match svfs.find_by_name(&name) {
                    Some(hash) => {
                        // Read tags/description
                        let tag_info = svfs.get_tags(&hash).unwrap_or_default();
                        let (tags_str, desc_str) = parse_stored_tags(&tag_info);

                        match svfs.retrieve(&hash) {
                            Ok(data) => {
                                let text = core::str::from_utf8(&data).unwrap_or("<binary data>");
                                let mut msg = format!("--- {} ({} bytes) ---\n", name, data.len());
                                if !desc_str.is_empty() {
                                    msg += &format!("  {}\n", desc_str);
                                }
                                if !tags_str.is_empty() {
                                    msg += &format!("  tags: {}\n", tags_str);
                                }
                                msg += &format!("---\n{}\n---\n", text);
                                self.print(ctx, msg.as_bytes());
                            }
                            Err(_) => {
                                let msg = format!("[SVFS Error: could not read '{}']\n", name);
                                self.print(ctx, msg.as_bytes());
                            }
                        }
                    }
                    None => {
                        let msg = format!("[SVFS] File '{}' not found.\n", name);
                        self.print(ctx, msg.as_bytes());
                    }
                }
            }
            ToolCall::DeleteFile { ref name } => {
                self.glass_box(ctx, "state", "svfs-delete");
                match svfs.delete_by_name(&name) {
                    Ok(true) => {
                        let msg = format!("[SVFS] Deleted '{}'\n", name);
                        self.print(ctx, msg.as_bytes());
                    }
                    Ok(false) => {
                        let msg = format!("[SVFS] File '{}' not found.\n", name);
                        self.print(ctx, msg.as_bytes());
                    }
                    Err(_) => {
                        let msg = format!("[SVFS Error: failed to delete '{}']\n", name);
                        self.print(ctx, msg.as_bytes());
                    }
                }
            }
            ToolCall::ListFiles => {
                self.glass_box(ctx, "state", "svfs-list");
                let objects = svfs.list_all();
                if objects.is_empty() {
                    self.print(ctx, b"[SVFS] No files stored.\n");
                } else {
                    let mut out = format!("Files ({}):\n", objects.len());
                    for (name, _obj_type, size) in &objects {
                        let display_name = if name.is_empty() { "<unnamed>" } else { name.as_str() };

                        // Get tags + description for this file
                        let (tags_str, desc_str) = if let Some(hash) = svfs.find_by_name(name) {
                            let tag_info = svfs.get_tags(&hash).unwrap_or_default();
                            parse_stored_tags(&tag_info)
                        } else {
                            (String::new(), String::new())
                        };

                        out += &format!("  {:14}  {} bytes", display_name, size);
                        if !desc_str.is_empty() {
                            out += &format!("  -- {}", desc_str);
                        }
                        if !tags_str.is_empty() {
                            out += &format!("  [{}]", tags_str);
                        }
                        out += "\n";
                    }
                    self.print(ctx, out.as_bytes());
                }
            }
            ToolCall::SearchFiles { ref query } => {
                self.glass_box(ctx, "state", "svfs-search");
                let objects = svfs.list_all();
                if objects.is_empty() {
                    self.print(ctx, b"[SVFS] No files stored.\n");
                    return;
                }

                // Split query into keywords for OR matching
                // "poem story" → matches files containing "poem" OR "story"
                let keywords: alloc::vec::Vec<String> = query
                    .split(|c: char| c == ' ' || c == ',')
                    .map(|w| w.trim())
                    .filter(|w| !w.is_empty())
                    .map(|w| {
                        let s: String = w.chars().map(|c| c.to_ascii_lowercase()).collect();
                        s
                    })
                    .collect();
                let mut found = false;
                let mut out = format!("Search results for '{}':\n", query);

                for (name, _obj_type, size) in &objects {
                    let name_lower: String = name.chars().map(|c| c.to_ascii_lowercase()).collect();

                    let (tags_str, desc_str) = if let Some(hash) = svfs.find_by_name(name) {
                        let tag_info = svfs.get_tags(&hash).unwrap_or_default();
                        parse_stored_tags(&tag_info)
                    } else {
                        (String::new(), String::new())
                    };

                    let tags_lower: String = tags_str.chars().map(|c| c.to_ascii_lowercase()).collect();
                    let desc_lower: String = desc_str.chars().map(|c| c.to_ascii_lowercase()).collect();

                    // Check each keyword against name, tags, description, content (OR logic)
                    let mut match_on: Option<&str> = None;
                    for kw in &keywords {
                        if name_lower.contains(kw.as_str()) {
                            match_on = Some("name"); break;
                        }
                        if tags_lower.contains(kw.as_str()) {
                            match_on = Some("tags"); break;
                        }
                        if desc_lower.contains(kw.as_str()) {
                            match_on = Some("description"); break;
                        }
                    }
                    // Check content only if no metadata hit
                    if match_on.is_none() {
                        if let Some(hash) = svfs.find_by_name(name) {
                            if let Ok(data) = svfs.retrieve(&hash) {
                                let text = core::str::from_utf8(&data).unwrap_or("");
                                let text_lower: String = text.chars().map(|c| c.to_ascii_lowercase()).collect();
                                for kw in &keywords {
                                    if text_lower.contains(kw.as_str()) {
                                        match_on = Some("content"); break;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(matched) = match_on {

                        let display_name = if name.is_empty() { "<unnamed>" } else { name.as_str() };
                        out += &format!("  {:14}  {} bytes  (matched: {})", display_name, size, matched);
                        if !desc_str.is_empty() {
                            out += &format!("\n    {}", desc_str);
                        }
                        if !tags_str.is_empty() {
                            out += &format!("\n    tags: {}", tags_str);
                        }
                        out += "\n";
                        found = true;
                    }
                }

                if found {
                    self.print(ctx, out.as_bytes());
                } else {
                    let msg = format!("[SVFS] No files matching '{}' found.\n", query);
                    self.print(ctx, msg.as_bytes());
                }
            }
        }
    }

    /// Update the Glass Box state display.
    fn glass_box(&self, ctx: &AgentContext, key: &str, value: &str) {
        let update = Intent::request(
            "glass-box.update",
            "primary-agent",
            &glass_box::GlassBoxUpdate {
                module: String::from("primary-agent"),
                key: String::from(key),
                value: String::from(value),
            },
        );
        ctx.bus.send(update);
    }
}

impl Agent for PrimaryAiAgent {
    fn name(&self) -> &str {
        "primary-agent"
    }

    fn priority(&self) -> CognitivePriority {
        // Highest priority — the AI's reasoning loop should run first.
        CognitivePriority::Reasoning
    }

    fn on_start(&mut self, ctx: &AgentContext) {
        if !self.greeted {
            self.print(ctx, concat!(
                "\n",
                "  +==================================+\n",
                "  |   Squirrel AIOS                  |\n",
                "  |   AI Sovereign Operating System  |\n",
                "  +==================================+\n",
                "\n",
                "  Type anything. Talk naturally.\n",
                "  I can create, read, search, and list files.\n",
                "\n",
                "> ",
            ).as_bytes());
            self.greeted = true;
        }

        self.glass_box(ctx, "state", "waiting");
    }

    fn poll(&mut self, ctx: &AgentContext) -> AgentPoll {
        // Process all pending intents in the inbox
        while let Some(intent) = ctx.bus.try_recv() {
            let intent_type = intent.semantic_type.as_str();

            match (&self.state, intent_type) {
                // ── User typed a line ──────────────────────────────
                (AgentState::WaitingForInput, "input.line") => {
                    // Input module sends raw UTF-8 bytes (not postcard)
                    let user_input = match core::str::from_utf8(&intent.payload) {
                        Ok(s) => s.trim(),
                        Err(_) => return AgentPoll::Yield,
                    };

                    if user_input.is_empty() {
                        self.print(ctx, b"> ");
                        return AgentPoll::Yield;
                    }

                    // Update Glass Box with the user's input
                    let display_input: String = user_input.chars().take(40).collect();
                    self.glass_box(ctx, "last_input", &display_input);

                    // Try UI pattern matching first (instant, no AI needed)
                    if let Some(plan) = try_pattern_match(user_input) {
                        for step in plan.steps {
                            self.send_raw(ctx, &step.intent_type, &step.payload);
                        }
                        if user_input.trim().eq_ignore_ascii_case("settings")
                            || user_input.contains("configure")
                            || user_input.contains("api key")
                        {
                            self.state = AgentState::WaitingForModule;
                            self.glass_box(ctx, "state", "module-active");
                        } else {
                            self.print(ctx, b"> ");
                        }
                        return AgentPoll::Yield;
                    }

                    // Everything else → AI decides what to do
                    self.pending_input = String::from(user_input);
                    let history = self.format_history();
                    let request = build_inference_request(user_input, &history);
                    let infer_intent = Intent::request(
                        "inference.generate",
                        "primary-agent",
                        &request,
                    );
                    ctx.bus.send(infer_intent);

                    self.state = AgentState::WaitingForInference;
                    self.glass_box(ctx, "state", "thinking");
                    self.print(ctx, b"[thinking...]\n");

                    return AgentPoll::Yield;
                }

                // ── AI inference response ─────────────────────────
                (AgentState::WaitingForInference, t) if t.starts_with("inference.generate") => {
                    if let Ok(response) = intent.decode::<InferenceResponse>() {
                        // Parse the AI's response for tool calls (may be multiple)
                        let (tool_calls, speech) = parse_tool_calls(&response.text);

                        // Print the AI's natural language part (if any)
                        if !speech.is_empty() {
                            let msg = format!("{}\n", speech);
                            self.print(ctx, msg.as_bytes());
                        }

                        // Execute all tool calls
                        for tool in &tool_calls {
                            self.execute_tool_call(ctx, tool);
                        }

                        // Record conversation turn for history
                        let pending = core::mem::replace(&mut self.pending_input, String::new());
                        self.push_history(pending, speech);

                        // Show stats + prompt
                        let stats = format!(
                            "[{} in {}ms]\n> ",
                            response.backend_used, response.latency_ms
                        );
                        self.print(ctx, stats.as_bytes());
                    } else if let Ok(error_text) = intent.decode::<String>() {
                        let msg = format!("[Error: {}]\n> ", error_text);
                        self.print(ctx, msg.as_bytes());
                    } else {
                        self.print(ctx, b"[Error: could not decode response]\n> ");
                    }

                    self.state = AgentState::WaitingForInput;
                    self.glass_box(ctx, "state", "waiting");
                    return AgentPoll::Yield;
                }

                // ── Module closed (settings, etc.) ────────────────
                (AgentState::WaitingForModule, "settings.closed")
                | (AgentState::WaitingForModule, "display.clear.done") => {
                    self.print(ctx, b"\n> ");
                    self.state = AgentState::WaitingForInput;
                    self.glass_box(ctx, "state", "waiting");
                    return AgentPoll::Yield;
                }

                // ── System status request ─────────────────────────
                (_, "system.status") => {
                    let backend = settings::current_backend();
                    let status = format!(
                        "Squirrel AIOS Status:\n  Backend: {}\n  Tick: {}\n> ",
                        backend, ctx.tick
                    );
                    self.print(ctx, status.as_bytes());
                    return AgentPoll::Yield;
                }

                // Ignore everything else
                _ => {}
            }
        }

        AgentPoll::Pending
    }
}

/// Parse the raw tag string from SVFS into (tags, description).
///
/// Tags are stored as comma-separated values. A special tag starting with
/// "desc:" holds the file description. This function separates them.
///
/// Example: "user-created,poem,love,desc:A romantic poem about roses"
///   → tags: "poem,love"  description: "A romantic poem about roses"
fn parse_stored_tags(raw: &str) -> (String, String) {
    let mut tags = alloc::vec::Vec::new();
    let mut description = String::new();

    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(desc) = trimmed.strip_prefix("desc:") {
            description = String::from(desc.trim());
        } else if trimmed != "user-created" {
            // Skip "user-created" as it's internal, show only meaningful tags
            tags.push(trimmed);
        }
    }

    let tags_str: String = tags.join(",");
    (tags_str, description)
}
