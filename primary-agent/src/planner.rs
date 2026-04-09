//! Planner — decides how to handle user input.
//!
//! Before sending anything to the AI inference engine (which costs time and
//! possibly money), the planner checks if the input matches a known UI pattern.
//! Simple commands like "help", "settings", "clear", "status" are handled
//! instantly without touching the AI at all.
//!
//! Everything else goes to the AI inference engine. The AI is told about
//! available tools (SVFS file operations) in the system prompt. When the AI
//! wants to use a tool, it includes a structured tag in its response which
//! the primary agent parses and executes.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use inference_engine::InferenceRequest;

// ---------------------------------------------------------------------------
// Tool calls — parsed from AI responses, executed against SVFS
// ---------------------------------------------------------------------------

/// A tool call the AI decided to make, parsed from its response text.
pub enum ToolCall {
    /// Create/write a file with semantic metadata
    CreateFile {
        name: String,
        tags: String,       // comma-separated: "poem,love,roses"
        description: String, // short summary: "A romantic poem about roses"
        content: String,
    },
    /// Read a file by name and display its content
    ReadFile { name: String },
    /// List all stored files
    ListFiles,
    /// Search files — AI gives a keyword, we search names, tags, description, and content
    SearchFiles { query: String },
    /// Delete a file by name
    DeleteFile { name: String },
}

/// Parse ALL tool call tags from the AI's response.
///
/// Returns a list of tool calls and the remaining "speech" text
/// (everything outside the tags). Supports multiple tool calls per response
/// so the AI can e.g. create 3 files in one go.
pub fn parse_tool_calls(response: &str) -> (Vec<ToolCall>, String) {
    let mut tools = Vec::new();
    let mut speech = String::from(response);

    // Parse CREATE_FILE calls (may be multiple)
    // Each CREATE_FILE uses rfind(']') within its own scope to handle ']' in content
    let mut search_from = 0;
    loop {
        let haystack = &speech[search_from..];
        let offset = match haystack.find("[CREATE_FILE:") {
            Some(pos) => search_from + pos,
            None => break,
        };
        let after = &speech[offset + 13..];
        // For CREATE_FILE, find the next [CREATE_FILE: or [READ_FILE: etc to bound the search
        // Otherwise use rfind which would grab too much with multiple CREATE_FILEs
        let end_bound = find_next_tag_start(after).unwrap_or(after.len());
        let bounded = &after[..end_bound];
        if let Some(end) = bounded.rfind(']') {
            let inner = &after[..end];
            if let Some(colon) = inner.find(':') {
                let meta = inner[..colon].trim();
                let content = inner[colon + 1..].trim();
                if !meta.is_empty() && !content.is_empty() {
                    let parts: Vec<&str> = meta.splitn(3, '|').collect();
                    let name = parts[0].trim();
                    let tags = if parts.len() > 1 { parts[1].trim() } else { "" };
                    let desc = if parts.len() > 2 { parts[2].trim() } else { "" };
                    if !name.is_empty() {
                        tools.push(ToolCall::CreateFile {
                            name: String::from(name),
                            tags: String::from(tags),
                            description: String::from(desc),
                            content: String::from(content),
                        });
                    }
                }
            }
            let tag_end = offset + 13 + end + 1;
            speech = strip_tag_inplace(speech, offset, tag_end);
            // Don't advance search_from — string shifted
        } else {
            search_from = offset + 13;
        }
    }

    // Parse simple tags (may appear multiple times)
    parse_simple_tags(&mut tools, &mut speech, "[READ_FILE:", |inner| {
        Some(ToolCall::ReadFile { name: String::from(inner.trim()) })
    });
    parse_simple_tags(&mut tools, &mut speech, "[DELETE_FILE:", |inner| {
        Some(ToolCall::DeleteFile { name: String::from(inner.trim()) })
    });
    parse_simple_tags(&mut tools, &mut speech, "[SEARCH_FILES:", |inner| {
        Some(ToolCall::SearchFiles { query: String::from(inner.trim()) })
    });

    // LIST_FILES (no argument)
    while let Some(start) = speech.find("[LIST_FILES]") {
        tools.push(ToolCall::ListFiles);
        speech = strip_tag_inplace(speech, start, start + 12);
    }

    let trimmed = String::from(speech.trim());
    (tools, trimmed)
}

/// Parse all instances of a simple [TAG:value] pattern.
fn parse_simple_tags<F>(tools: &mut Vec<ToolCall>, speech: &mut String, prefix: &str, make: F)
where
    F: Fn(&str) -> Option<ToolCall>,
{
    loop {
        let start = match speech.find(prefix) {
            Some(s) => s,
            None => break,
        };
        let after = &speech[start + prefix.len()..];
        if let Some(end) = after.find(']') {
            let inner = &after[..end];
            if !inner.trim().is_empty() {
                if let Some(tool) = make(inner) {
                    tools.push(tool);
                }
            }
            *speech = strip_tag_inplace(core::mem::take(speech), start, start + prefix.len() + end + 1);
        } else {
            break;
        }
    }
}

/// Find the start of the next tool tag after the current position.
/// Used to bound CREATE_FILE's rfind(']') when multiple tags exist.
fn find_next_tag_start(s: &str) -> Option<usize> {
    let tags = ["[CREATE_FILE:", "[READ_FILE:", "[DELETE_FILE:", "[LIST_FILES]", "[SEARCH_FILES:"];
    let mut earliest: Option<usize> = None;
    for tag in &tags {
        if let Some(pos) = s.find(tag) {
            earliest = Some(match earliest {
                Some(e) => e.min(pos),
                None => pos,
            });
        }
    }
    earliest
}

/// Remove a tag span from a string and return the result.
fn strip_tag_inplace(s: String, tag_start: usize, tag_end: usize) -> String {
    let before = s[..tag_start].trim_end();
    let after = s[tag_end..].trim_start();
    let mut result = String::from(before);
    if !result.is_empty() && !after.is_empty() {
        result.push('\n');
    }
    result.push_str(after);
    result
}

// ---------------------------------------------------------------------------
// UI commands — handled instantly without AI
// ---------------------------------------------------------------------------

/// A plan: one or more steps to execute in response to user input.
/// Each step becomes an intent sent through the bus.
pub struct WorkflowPlan {
    pub steps: Vec<WorkflowStep>,
}

/// A single step in a workflow plan.
pub struct WorkflowStep {
    /// The intent type to send (e.g., "display.print", "settings.open")
    pub intent_type: String,
    /// Raw payload bytes for the intent
    pub payload: Vec<u8>,
}

impl WorkflowPlan {
    /// Create a plan with a single step.
    pub fn single(intent_type: &str, payload: &[u8]) -> Self {
        Self {
            steps: vec![WorkflowStep {
                intent_type: String::from(intent_type),
                payload: payload.to_vec(),
            }],
        }
    }
}

/// Try to match user input against known UI patterns.
///
/// Returns Some(plan) if the input is a recognized UI command,
/// None if it needs to be sent to the AI inference engine.
pub fn try_pattern_match(input: &str) -> Option<WorkflowPlan> {
    let lower = input.trim();

    // Settings
    if eq_ignore_case(lower, "settings")
        || contains_ignore_case(lower, "configure")
        || contains_ignore_case(lower, "change model")
        || contains_ignore_case(lower, "api key")
        || contains_ignore_case(lower, "switch to openai")
        || contains_ignore_case(lower, "switch to anthropic")
        || contains_ignore_case(lower, "switch to gemini")
        || contains_ignore_case(lower, "use local model")
    {
        return Some(WorkflowPlan::single("settings.open", b"{}"));
    }

    // Help
    if eq_ignore_case(lower, "help") || lower == "?" {
        let help = b"Squirrel AIOS \xe2\x80\x94 Commands:\n\
                      \x20 settings    \xe2\x80\x94 configure AI backend\n\
                      \x20 help        \xe2\x80\x94 show this help\n\
                      \x20 clear       \xe2\x80\x94 clear the screen\n\
                      \x20 status      \xe2\x80\x94 show system status\n\
                      \x20 Or just talk naturally \xe2\x80\x94 ask me to create, read, or list files.\n";
        return Some(WorkflowPlan::single("display.print", help));
    }

    // Clear screen
    if eq_ignore_case(lower, "clear") || eq_ignore_case(lower, "cls") {
        return Some(WorkflowPlan::single("display.clear", b""));
    }

    // Status
    if eq_ignore_case(lower, "status") || eq_ignore_case(lower, "ps") || eq_ignore_case(lower, "agents") {
        return Some(WorkflowPlan::single("system.status", b""));
    }

    None
}

/// Build an InferenceRequest from user input (when pattern matching fails).
/// Includes the current SVFS file listing and conversation history so the AI
/// knows what files exist and what was said previously.
pub fn build_inference_request(user_input: &str, history: &str) -> InferenceRequest {
    let files_ctx = build_files_context();
    InferenceRequest {
        prompt: alloc::format!("{}\n{}{}\nUser: {}\nAssistant:", SYSTEM_PROMPT, files_ctx, history, user_input),
        max_tokens: 400,
        temperature: 0.7,
        stop_sequences: vec![String::from("User:"), String::from("\n\nUser")],
    }
}

/// Build a compact listing of current SVFS files for the AI's context.
fn build_files_context() -> String {
    let svfs = match svfs::SVFS.get() {
        Some(s) => s,
        None => return String::from("\nFILES IN SVFS: (not initialized)"),
    };
    let objects = svfs.list_all();
    if objects.is_empty() {
        return String::from("\nFILES IN SVFS: (empty)");
    }
    let mut out = String::from("\nFILES IN SVFS (use exact names in tool tags):\n");
    for (name, _obj_type, size) in &objects {
        let display = if name.is_empty() { "<unnamed>" } else { name.as_str() };
        // Get tags for context
        if let Some(hash) = svfs.find_by_name(name) {
            let tag_info = svfs.get_tags(&hash).unwrap_or_default();
            out += &alloc::format!("  {} ({} bytes) [{}]\n", display, size, tag_info);
        } else {
            out += &alloc::format!("  {} ({} bytes)\n", display, size);
        }
    }
    out
}

/// The system prompt — tells the AI who it is and what tools it has.
/// Kept concise to reduce API latency (every token costs time on bare-metal TLS).
pub const SYSTEM_PROMPT: &str = "\
You are Squirrel, AI brain of Squirrel AIOS — a bare-metal OS written in Rust (no Linux). \
You run on real x86_64 hardware via SART (agent runtime). The OS has: Intent Bus (semantic IPC), \
SVFS (content-addressed filesystem, blake3, RAM-backed), WASM sandbox, Glass Box (live state), \
network stack (TCP/IP, TLS 1.3), 8 agents + 5 WASM modules. Be concise — this is a terminal.\n\
\n\
TOOLS — include one or more tags when needed (you can use multiple in one response):\n\
  [CREATE_FILE:name|tags|description:content]\n\
    name: max 14 chars, no spaces, no extensions\n\
    tags: comma-separated keywords for semantic search (e.g. poem,love,nature)\n\
    description: one-line summary of what the file is about\n\
    content: the actual file content\n\
    Example: [CREATE_FILE:sunset|poem,nature,sky|A poem about a golden sunset:The sun dips low...]\n\
  [READ_FILE:name]            — read and display file\n\
  [DELETE_FILE:name]          — delete file\n\
  [LIST_FILES]                — list all files with tags and descriptions\n\
  [SEARCH_FILES:keyword1 keyword2]  — search by name/tags/description/content (multiple words = OR)\n\
\n\
ALWAYS include tags and description when creating files — this is how SVFS enables semantic search. \
Pick descriptive tags. The description should say what the file is about in one line. \
Generate content yourself when asked. These are REAL SVFS operations, not simulated. \
CRITICAL: NEVER list or echo the tool tag syntax in your text. The tags are parsed and executed — \
if you write them as examples, they will run. Only use a tag when you intend to perform that action. \
When greeting or explaining yourself, describe capabilities in plain English, never with the bracket syntax. \
Limitations: RAM-backed (lost on reboot), no USB/WiFi/GUI.";
// Case-insensitive helpers (no allocator needed for comparison)
fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    for i in 0..=(h.len() - n.len()) {
        let mut matched = true;
        for j in 0..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j].to_ascii_lowercase() {
                matched = false;
                break;
            }
        }
        if matched {
            return true;
        }
    }
    false
}
