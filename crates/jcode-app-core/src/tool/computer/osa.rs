//! Centralized `osascript` / JXA execution for the `computer` tool.
//!
//! Many macOS capabilities (Accessibility actions, window/app management, system
//! state) are reachable through AppleScript / JavaScript-for-Automation without
//! extra native bindings. This module funnels all of that through one place so
//! escaping, error mapping (especially the TCC permission errors), and timeouts
//! are handled consistently.

use anyhow::{Result, bail};
use std::process::Command;

/// Run an AppleScript and return stdout (trimmed). Maps the common macOS
/// permission / automation errors to actionable messages.
pub fn run_applescript(script: &str) -> Result<String> {
    run(&["-e", script], "AppleScript")
}

/// Run a JavaScript-for-Automation (JXA) script.
pub fn run_jxa(script: &str) -> Result<String> {
    run(&["-l", "JavaScript", "-e", script], "JXA")
}

fn run(args: &[&str], lang: &str) -> Result<String> {
    let output = Command::new("/usr/bin/osascript")
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn osascript: {e}"))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string());
    }

    let err = String::from_utf8_lossy(&output.stderr);
    let trimmed = err.trim();
    let lower = trimmed.to_lowercase();

    // -1719 / "not allowed assistive access" -> Accessibility missing.
    if lower.contains("assistive")
        || lower.contains("not allowed")
        || lower.contains("-1719")
        || lower.contains("1002")
    {
        bail!(
            "Accessibility permission required. Run the `setup` action, or grant it in \
             System Settings > Privacy & Security > Accessibility for your terminal/jcode. \
             ({trimmed})"
        );
    }
    // -1743 -> Automation (Apple Events) not authorized for the target app.
    if lower.contains("-1743") || lower.contains("not authorized to send apple events") {
        bail!(
            "Automation permission required for the target app. Approve the prompt, or grant it \
             in System Settings > Privacy & Security > Automation. ({trimmed})"
        );
    }

    bail!("{lang} failed: {trimmed}");
}

/// Quote a string as an AppleScript string literal (wraps in quotes, escapes
/// backslash and double-quote). Use for interpolating untrusted text into
/// generated AppleScript.
pub fn as_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(as_quote("hi"), "\"hi\"");
        assert_eq!(as_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(as_quote("a\\b"), "\"a\\\\b\"");
    }
}
