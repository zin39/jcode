//! Transcript scanning with an on-disk incremental cache.
//!
//! Scanning is the expensive part: there can be ~100k JSON transcripts totaling
//! several GB. We keep a sidecar cache (`~/.jcode/cache/productivity/summaries.json`)
//! keyed by `(file_len, mtime_ns)` so a re-run only re-parses changed files.
//! Parsing of the changed set is parallelized with rayon.

use crate::model::SessionSummary;
use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Timelike};
use jcode_session_types::{SessionAgentRole, classify_legacy_session, session_is_internal_agent};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// One cached entry: the file fingerprint plus the computed summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    len: u64,
    mtime_ns: i128,
    summary: SessionSummary,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    /// Cache format version; bump to invalidate when summary semantics change.
    #[serde(default)]
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

const CACHE_VERSION: u32 = 2;

fn cache_path() -> Result<PathBuf> {
    let dir = jcode_storage::jcode_dir()?
        .join("cache")
        .join("productivity");
    std::fs::create_dir_all(&dir).ok();
    Ok(dir.join("summaries.json"))
}

fn sessions_dir() -> Result<PathBuf> {
    Ok(jcode_storage::jcode_dir()?.join("sessions"))
}

/// Result of a full scan: every session summary plus scan diagnostics.
pub struct ScanResult {
    pub summaries: Vec<SessionSummary>,
    pub scanned_files: u64,
    pub parse_errors: u64,
    pub cache_hits: u64,
    pub scan_secs: f64,
}

/// Scan all session transcripts, using and refreshing the incremental cache.
pub fn scan_all() -> Result<ScanResult> {
    let started = Instant::now();
    let dir = sessions_dir()?;

    // Load prior cache (best-effort; ignore corruption).
    let mut cache: Cache = std::fs::read(cache_path()?)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if cache.version != CACHE_VERSION {
        cache = Cache {
            version: CACHE_VERSION,
            entries: HashMap::new(),
        };
    }

    // Enumerate candidate transcript files.
    let mut files: Vec<(String, u64, i128)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let is_json = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("json"))
                .unwrap_or(false);
            if !is_json {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let len = meta.len();
            let mtime_ns = mtime_ns(&meta);
            files.push((name, len, mtime_ns));
        }
    }

    let cache_hits = AtomicU64::new(0);
    let parse_errors = AtomicU64::new(0);

    // Parse (or reuse cache) in parallel. Returns (filename, entry).
    let results: Vec<(String, CacheEntry)> = files
        .par_iter()
        .filter_map(|(name, len, mtime_ns)| {
            if let Some(prev) = cache.entries.get(name)
                && prev.len == *len
                && prev.mtime_ns == *mtime_ns
            {
                cache_hits.fetch_add(1, Ordering::Relaxed);
                return Some((name.clone(), prev.clone()));
            }
            let path = dir.join(name);
            match parse_session_file(&path) {
                Ok(summary) => Some((
                    name.clone(),
                    CacheEntry {
                        len: *len,
                        mtime_ns: *mtime_ns,
                        summary,
                    },
                )),
                Err(_) => {
                    parse_errors.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();

    // Rebuild cache from this scan (drops entries for deleted files).
    let mut new_entries: HashMap<String, CacheEntry> = HashMap::with_capacity(results.len());
    let mut summaries: Vec<SessionSummary> = Vec::with_capacity(results.len());
    for (name, entry) in results {
        summaries.push(entry.summary.clone());
        new_entries.insert(name, entry);
    }
    let new_cache = Cache {
        version: CACHE_VERSION,
        entries: new_entries,
    };
    if let Ok(bytes) = serde_json::to_vec(&new_cache)
        && let Ok(path) = cache_path()
    {
        let _ = std::fs::write(path, bytes);
    }

    Ok(ScanResult {
        scanned_files: summaries.len() as u64,
        parse_errors: parse_errors.into_inner(),
        cache_hits: cache_hits.into_inner(),
        scan_secs: started.elapsed().as_secs_f64(),
        summaries,
    })
}

#[cfg(unix)]
fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    use std::os::unix::fs::MetadataExt;
    (meta.mtime() as i128) * 1_000_000_000 + (meta.mtime_nsec() as i128)
}

#[cfg(not(unix))]
fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Minimal transcript parsing
//
// We deliberately use a tolerant, partial deserialization instead of the full
// `Session`/`StoredMessage` types so this crate stays dependency-light and keeps
// working even if the canonical schema drifts.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawSession {
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    // Classification inputs. A stored `agent_role` wins when present; the
    // rest let us apply the same legacy rule the resume picker uses, so a
    // session written before `agent_role` existed is judged identically here.
    #[serde(default)]
    agent_role: Option<SessionAgentRole>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    is_debug: bool,
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Vec<RawBlock>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    token_usage: Option<RawTokenUsage>,
    /// Set when a message is rendered as something other than plain
    /// conversation (system notices, injected context). Such a message is not
    /// something the user said.
    #[serde(default)]
    display_role: Option<String>,
}

impl RawMessage {
    /// Whether this user-role message is something the user actually typed.
    ///
    /// Mirrors the rule the session picker uses for "visible conversation", so
    /// the two surfaces agree on what a turn is. A message counts only when it
    /// renders as ordinary conversation and carries real text: tool results and
    /// injected system reminders are stored under the user role but are not the
    /// user speaking.
    fn is_user_prompt(&self) -> bool {
        if self.display_role.is_some() {
            return false;
        }
        self.content.iter().any(|block| match block {
            RawBlock::Text { text } => !text.trim_start().starts_with("<system-reminder>"),
            _ => false,
        })
    }
}

#[derive(Deserialize)]
struct RawTokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    Image {},
    #[serde(other)]
    Other,
}

fn parse_session_file(path: &Path) -> Result<SessionSummary> {
    let bytes = std::fs::read(path)?;
    let raw: RawSession = serde_json::from_slice(&bytes)?;
    Ok(summarize(raw))
}

/// Summarize one transcript's raw JSON. Exposed for tests so the classification
/// rules can be exercised against real transcript shapes without writing files.
#[cfg(test)]
pub(crate) fn summarize_json(bytes: &[u8]) -> Result<SessionSummary> {
    let raw: RawSession = serde_json::from_slice(bytes)?;
    Ok(summarize(raw))
}

fn summarize(raw: RawSession) -> SessionSummary {
    let mut s = SessionSummary {
        created_at: raw.created_at.clone(),
        updated_at: raw.updated_at.clone(),
        working_dir: raw.working_dir.clone(),
        project: raw.working_dir.as_deref().map(project_name),
        provider_key: raw.provider_key,
        model: raw.model,
        ..Default::default()
    };

    let mut active_dates = std::collections::BTreeSet::new();
    let record_time =
        |ts: &str, s: &mut SessionSummary, dates: &mut std::collections::BTreeSet<String>| {
            if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
                let local = dt.with_timezone(&Local);
                s.hour_hist[local.hour() as usize] += 1;
                s.weekday_hist[local.weekday().num_days_from_monday() as usize] += 1;
                dates.insert(local.format("%Y-%m-%d").to_string());
            }
        };

    for msg in &raw.messages {
        let role = msg.role.as_deref().unwrap_or("");
        match role {
            // Count what the user actually said, not every message stored
            // under the user role. Tool results are recorded as user-role
            // messages, and on a real transcript they outnumber genuine
            // prompts roughly 8:1, so counting them made "prompts sent"
            // meaningless and, worse, made every agent run look like a long
            // conversation to the delegation rule below.
            "user" => {
                if msg.is_user_prompt() {
                    s.user_msgs += 1;
                }
            }
            "assistant" => s.assistant_msgs += 1,
            _ => {}
        }

        if let Some(ts) = &msg.timestamp {
            if s.first_ts.is_none() {
                s.first_ts = Some(ts.clone());
            }
            s.last_ts = Some(ts.clone());
            record_time(ts, &mut s, &mut active_dates);
        }

        if let Some(tu) = &msg.token_usage {
            s.input_tokens += tu.input_tokens;
            s.output_tokens += tu.output_tokens;
            s.cache_read_tokens += tu.cache_read_input_tokens.unwrap_or(0);
            s.cache_creation_tokens += tu.cache_creation_input_tokens.unwrap_or(0);
        }

        for block in &msg.content {
            match block {
                RawBlock::Text { text } => {
                    let len = text.chars().count() as u64;
                    if role == "user" {
                        // Keep the "human typed this" proxy to real prompts:
                        // same rule as the turn count, so a system notice or a
                        // reminder blob never reads as the user's effort.
                        if msg.is_user_prompt() {
                            s.user_chars += len;
                        }
                    } else if role == "assistant" {
                        s.assistant_chars += len;
                    }
                }
                RawBlock::ToolUse { name, input } => {
                    count_tool(&mut s.tools, name, input);
                }
                RawBlock::Image {} => s.images += 1,
                RawBlock::Other => {}
            }
        }
    }

    // Fall back to header timestamps for the activity calendar when individual
    // messages lacked timestamps (common for imported transcripts).
    if active_dates.is_empty()
        && let Some(ts) = raw.updated_at.as_deref().or(raw.created_at.as_deref())
        && let Ok(dt) = DateTime::parse_from_rfc3339(ts)
    {
        let local = dt.with_timezone(&Local);
        s.hour_hist[local.hour() as usize] += 1;
        s.weekday_hist[local.weekday().num_days_from_monday() as usize] += 1;
        active_dates.insert(local.format("%Y-%m-%d").to_string());
    }

    s.active_dates = active_dates.into_iter().collect();

    // Classify only once the message loop has run, because the legacy rule
    // needs the user turn count. A stored role wins; otherwise fall back to
    // the same inference the resume picker uses, so the two surfaces never
    // disagree about whose work a session was.
    let role = raw.agent_role.or_else(|| {
        classify_legacy_session(
            raw.parent_id.as_deref(),
            raw.title.as_deref(),
            s.user_msgs as usize,
            raw.saved,
        )
    });
    s.delegated = session_is_internal_agent(role, raw.parent_id.as_deref(), raw.is_debug);

    s
}

/// Expand a single tool invocation into the histogram. `batch` is special-cased
/// so the inner tool calls get individual credit.
fn count_tool(tools: &mut BTreeMap<String, u32>, name: &str, input: &serde_json::Value) {
    let canonical = canonical_tool_name(name);
    *tools.entry(canonical.to_string()).or_insert(0) += 1;

    if canonical == "batch"
        && let Some(calls) = input.get("tool_calls").and_then(|v| v.as_array())
    {
        for call in calls {
            if let Some(inner) = call.get("tool").and_then(|v| v.as_str()) {
                let inner_canon = canonical_tool_name(inner);
                *tools.entry(inner_canon.to_string()).or_insert(0) += 1;
            }
        }
    }
}

/// Normalize legacy/alias tool names to their canonical identity.
fn canonical_tool_name(name: &str) -> &str {
    match name {
        "file_read" => "read",
        "file_write" => "write",
        "file_edit" => "edit",
        "file_grep" => "grep",
        "todowrite" => "todo",
        other => other,
    }
}

fn project_name(working_dir: &str) -> String {
    let trimmed = working_dir.trim_end_matches('/');
    Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}
