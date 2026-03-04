//! Glass Box display renderer — turns module state into a formatted ASCII overlay.
//!
//! The Glass Box needs to be VISIBLE — you should be able to look at the
//! framebuffer at any moment and see what every agent is doing. This module
//! takes a list of ModuleSnapshots and renders them as a nice ASCII box.
//!
//! Example output:
//! ```text
//! ┌─── Glass Box ─────────────────────────────────────┐
//! │ ● kernel                          0ms ago         │
//! │   status           = booting                      │
//! │ ● heartbeat                       0ms ago         │
//! │   beat_count       = 5                            │
//! └────────────────────────────────────────────────────┘
//! ```
//!
//! The "●" means the module is active (running). "○" means inactive (stopped
//! but we're still showing its last state). The "Xms ago" shows how recently
//! the module last updated any of its state.
//!
//! Each module shows up to 4 key-value pairs. If a module has more than 4,
//! only the first 4 (alphabetically, since BTreeMap is sorted) are shown.
//! If there are more modules than fit in max_lines, a "... (more modules)"
//! line is shown.

use alloc::string::String;
use alloc::format;
use crate::state::ModuleSnapshot;

/// Width of the Glass Box display in characters.
/// This is the inner width — the "│" borders add 2 more.
const BOX_INNER_WIDTH: usize = 52;

/// Render the Glass Box as a formatted ASCII string.
///
/// Takes a slice of ModuleSnapshots (from GlassBoxStore::snapshot()) and
/// a maximum number of output lines. Returns a String that can be printed
/// directly to the framebuffer.
///
/// How it works:
/// 1. Print the top border: ┌─── Glass Box ───...─┐
/// 2. For each module:
///    a. Print a header line: │ ● module_name    Xms ago │
///    b. Print up to 4 key-value lines: │   key = value │
///    c. Stop if we've hit max_lines (show "... more modules")
/// 3. Print the bottom border: └────...────┘
pub fn render_to_string(snapshots: &[ModuleSnapshot], max_lines: usize) -> String {
    let mut output = String::new();

    // Top border
    output.push_str("\u{250c}\u{2500}\u{2500}\u{2500} Glass Box \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n");

    let mut lines = 1usize; // count the top border as line 1

    for snap in snapshots {
        // If we're about to exceed max_lines, show a truncation message
        if lines >= max_lines.saturating_sub(1) {
            output.push_str(&pad_line(
                &format!("\u{2502} ... ({} more modules) ...", snapshots.len()),
                BOX_INNER_WIDTH,
            ));
            output.push('\n');
            break;
        }

        // Module header line: │ ● module_name    Xms ago │
        // "●" = active (running), "○" = inactive (stopped)
        let status_dot = if snap.is_active { "\u{25cf}" } else { "\u{25cb}" };
        let ms_ago = current_ms().saturating_sub(snap.last_update_ms);
        let header = format!(
            "\u{2502} {} {:20} {:>8}ms ago",
            status_dot,
            truncate(&snap.name, 20),
            ms_ago
        );
        output.push_str(&pad_line(&header, BOX_INNER_WIDTH));
        output.push('\n');
        lines += 1;

        // Key-value lines: │   key = value │
        // Show at most 4 entries per module to keep the display compact.
        // BTreeMap iterates in sorted order, so keys appear alphabetically.
        for (key, val) in snap.state.iter().take(4) {
            if lines >= max_lines.saturating_sub(1) {
                break;
            }
            let entry = format!(
                "\u{2502}   {:16} = {}",
                truncate(key, 16),
                truncate(&val.value, 24)
            );
            output.push_str(&pad_line(&entry, BOX_INNER_WIDTH));
            output.push('\n');
            lines += 1;
        }
    }

    // Bottom border
    output.push_str("\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}\n");
    output
}

/// Truncate a string to at most `max` characters.
///
/// Works correctly with multi-byte UTF-8 characters — we count characters
/// (code points), not bytes. This prevents splitting a character in half.
fn truncate(s: &str, max: usize) -> &str {
    let end = s
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// Pad a line to exactly `target_len` characters, then add " │" at the end.
///
/// If the line is shorter than target_len, spaces are appended.
/// If the line is longer, it's used as-is (the border may be misaligned,
/// but we don't truncate content — that's done by the caller).
fn pad_line(s: &str, target_len: usize) -> String {
    let char_count = s.chars().count();
    let padding = if target_len > char_count {
        target_len - char_count
    } else {
        0
    };
    format!("{}{} \u{2502}", s, " ".repeat(padding))
}

/// Get current milliseconds since boot (from the Intent Bus time source).
fn current_ms() -> u64 {
    intent_bus::bus::current_ms()
}
