use crate::{
    desktop_rich_text, session_data,
    session_launch::{
        DesktopModelChoice, DesktopSessionEvent, DesktopSessionHandle, DesktopSessionStatus,
    },
    workspace,
};
use jcode_tui_messages::DisplayMessage;
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use workspace::{KeyInput, KeyOutcome, SessionTranscriptMessage};

mod overlay_render;
pub(crate) use overlay_render::*;

mod welcome;
pub(crate) use welcome::*;

mod pickers;
pub(crate) use pickers::*;

mod markdown;
pub(crate) use markdown::*;

mod tool_lines;
pub(crate) use tool_lines::*;
mod transcript;

pub(crate) const SINGLE_SESSION_FONT_FAMILY: &str = "JetBrainsMono Nerd Font";
pub(crate) const SINGLE_SESSION_USER_FONT_FAMILY: &str = "Kalam";
pub(crate) const SINGLE_SESSION_ASSISTANT_FONT_FAMILY: &str = SINGLE_SESSION_FONT_FAMILY;
pub(crate) const SINGLE_SESSION_WELCOME_FONT_FAMILY: &str = "Homemade Apple";
pub(crate) const SINGLE_SESSION_FONT_WEIGHT: &str = "Light";
pub(crate) const SINGLE_SESSION_FONT_FALLBACKS: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrains Mono",
    "monospace",
];

pub(crate) const SINGLE_SESSION_HANDWRITING_FONT_FAMILIES: &[&str] = &[
    "Homemade Apple",
    "Kalam",
    "Shadows Into Light Two",
    "Patrick Hand",
    "Gaegu",
    "Caveat",
    "Indie Flower",
    "Gloria Hallelujah",
    "Handlee",
    "Reenie Beanie",
];

static DESKTOP_USER_FONT_INDEX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
static DESKTOP_ASSISTANT_FONT_INDEX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

pub(crate) fn single_session_user_font_family() -> &'static str {
    desktop_font_family_from_index(
        DESKTOP_USER_FONT_INDEX.load(std::sync::atomic::Ordering::Relaxed),
    )
    .or_else(|| desktop_font_family_from_env("JCODE_DESKTOP_USER_FONT"))
    .unwrap_or(SINGLE_SESSION_USER_FONT_FAMILY)
}

pub(crate) fn single_session_assistant_font_family() -> &'static str {
    desktop_font_family_from_index(
        DESKTOP_ASSISTANT_FONT_INDEX.load(std::sync::atomic::Ordering::Relaxed),
    )
    .or_else(|| desktop_font_family_from_env("JCODE_DESKTOP_AI_FONT"))
    .or_else(|| desktop_font_family_from_env("JCODE_DESKTOP_ASSISTANT_FONT"))
    .unwrap_or(SINGLE_SESSION_ASSISTANT_FONT_FAMILY)
}

pub(crate) fn set_single_session_user_font_family(value: &str) -> Option<&'static str> {
    let (index, family) = desktop_font_family_index_from_key(value)?;
    DESKTOP_USER_FONT_INDEX.store(index, std::sync::atomic::Ordering::Relaxed);
    Some(family)
}

pub(crate) fn set_single_session_assistant_font_family(value: &str) -> Option<&'static str> {
    let (index, family) = desktop_font_family_index_from_key(value)?;
    DESKTOP_ASSISTANT_FONT_INDEX.store(index, std::sync::atomic::Ordering::Relaxed);
    Some(family)
}

fn desktop_font_family_from_index(index: usize) -> Option<&'static str> {
    match index {
        0 => Some(SINGLE_SESSION_FONT_FAMILY),
        index => SINGLE_SESSION_HANDWRITING_FONT_FAMILIES
            .get(index - 1)
            .copied(),
    }
}

fn desktop_font_family_index_from_key(value: &str) -> Option<(usize, &'static str)> {
    let family = desktop_font_family_from_key(value)?;
    if family == SINGLE_SESSION_FONT_FAMILY {
        return Some((0, family));
    }
    SINGLE_SESSION_HANDWRITING_FONT_FAMILIES
        .iter()
        .position(|candidate| *candidate == family)
        .map(|index| (index + 1, family))
}

fn desktop_font_family_from_env(name: &str) -> Option<&'static str> {
    let value = std::env::var(name).ok()?;
    desktop_font_family_from_key(&value)
}

pub(crate) fn desktop_font_family_from_key(value: &str) -> Option<&'static str> {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    match normalized.as_str() {
        "jetbrains" | "jetbrainsmono" | "jetbrainsmononerdfont" | "default" => {
            Some(SINGLE_SESSION_FONT_FAMILY)
        }
        "homemadeapple" => Some("Homemade Apple"),
        "kalam" => Some("Kalam"),
        "shadowsintolighttwo" | "shadowsintolight" => Some("Shadows Into Light Two"),
        "patrickhand" => Some("Patrick Hand"),
        "gaegu" => Some("Gaegu"),
        "caveat" => Some("Caveat"),
        "indieflower" => Some("Indie Flower"),
        "gloriahallelujah" => Some("Gloria Hallelujah"),
        "handlee" => Some("Handlee"),
        "reeniebeanie" => Some("Reenie Beanie"),
        _ => None,
    }
}
pub(crate) const SINGLE_SESSION_DEFAULT_FONT_SIZE: f32 = 22.0;
pub(crate) const SINGLE_SESSION_TITLE_FONT_SIZE: f32 = SINGLE_SESSION_DEFAULT_FONT_SIZE;
pub(crate) const SINGLE_SESSION_BODY_FONT_SIZE: f32 = SINGLE_SESSION_DEFAULT_FONT_SIZE * 1.55;
pub(crate) const SINGLE_SESSION_META_FONT_SIZE: f32 = SINGLE_SESSION_DEFAULT_FONT_SIZE;
pub(crate) const SINGLE_SESSION_CODE_FONT_SIZE: f32 = SINGLE_SESSION_BODY_FONT_SIZE;
pub(crate) const SINGLE_SESSION_BODY_LINE_HEIGHT: f32 = 1.45;
pub(crate) const SINGLE_SESSION_CODE_LINE_HEIGHT: f32 = 1.35;
pub(crate) const SINGLE_SESSION_META_LINE_HEIGHT: f32 = 1.25;
pub(crate) const SINGLE_SESSION_TEXT_SCALE_STEP: f32 = 0.10;
pub(crate) const SINGLE_SESSION_MIN_TEXT_SCALE: f32 = 0.65;
pub(crate) const SINGLE_SESSION_MAX_TEXT_SCALE: f32 = 1.35;
pub(crate) const HANDWRITTEN_WELCOME_PHRASES: &[&str] = &["Hello there"];

const DESKTOP_SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "show desktop shortcuts and slash commands"),
    ("/?", "alias for /help"),
    ("/commands", "alias for /help"),
    ("/clear", "clear conversation history"),
    ("/new", "reset to a fresh desktop session"),
    ("/resume", "open the recent session switcher"),
    ("/sessions", "open the recent session switcher"),
    ("/session", "alias for /sessions"),
    ("/issues", "toggle the local GitHub issue browser"),
    ("/model [name]", "open model picker or switch to a model"),
    ("/models", "alias for /model"),
    ("/refresh-model-list", "refresh provider model catalogs"),
    ("/reload", "force reload the desktop window using handoff"),
    ("/force-reload", "alias for /reload"),
    ("/effort [level]", "show or change reasoning effort"),
    (
        "/font [user|ai] [name]",
        "show or hot-swap desktop transcript fonts",
    ),
    ("/fonts", "alias for /font"),
    ("/fast [on|off|status]", "show or toggle OpenAI fast mode"),
    ("/transport [mode]", "show or change OpenAI transport"),
    (
        "/compact [mode <mode>]",
        "compact context or set compaction mode",
    ),
    ("/rename <title|--clear>", "rename the current session"),
    ("/usage", "desktop parity notice for TUI usage overlay"),
    ("/todo", "desktop parity notice for TUI todo panel"),
    ("/todos", "alias for /todo"),
    ("/memory", "desktop parity notice for TUI memory panel"),
    (
        "/changelog",
        "desktop parity notice for TUI changelog overlay",
    ),
    ("/diff", "desktop parity notice for TUI diff viewer"),
    ("/account", "desktop parity notice for TUI account picker"),
    ("/swarm", "desktop parity notice for TUI swarm panel"),
    ("/bg", "desktop parity notice for TUI background task panel"),
    (
        "/copy [latest|code|transcript]",
        "copy latest response, latest code block, or transcript",
    ),
    (
        "/search <query>",
        "count transcript matches and jump to the first one",
    ),
    ("/commit", "make logical commits from current changes"),
    ("/stop", "interrupt the running generation"),
    ("/cancel", "alias for /stop"),
    ("/status", "show current desktop session status"),
    ("/info", "alias for /status"),
    ("/quit", "exit the desktop app"),
    ("/exit", "alias for /quit"),
];
pub(crate) const DESKTOP_SLASH_SUGGESTION_ROW_LIMIT: usize = 7;

const DESKTOP_REASONING_EFFORTS_OPENAI: &[&str] = &["none", "low", "medium", "high", "xhigh"];
const DESKTOP_REASONING_EFFORTS_ANTHROPIC_STANDARD: &[&str] = &["none", "low", "medium", "high"];
const DESKTOP_REASONING_EFFORTS_DEEPSEEK: &[&str] = &["none", "low", "medium", "high", "max"];

#[cfg_attr(test, allow(dead_code))]
const INLINE_WIDGET_REVEAL_DURATION: Duration = Duration::from_millis(180);
const INLINE_WIDGET_EXIT_DURATION: Duration = Duration::from_millis(140);
pub(crate) const MODEL_PICKER_INLINE_ROW_LIMIT: usize = 5;
pub(crate) const INLINE_WIDGET_DEFAULT_VISIBLE_LINE_LIMIT: usize = 12;

const BODY_CACHE_TEXT_EDGE_BYTES: usize = 256;
const BODY_CACHE_MESSAGE_EDGE_COUNT: usize = 12;
const BODY_CACHE_MESSAGE_MIDDLE_SAMPLE_COUNT: usize = 8;

fn desktop_commit_prompt() -> String {
    "Make interactive, logical commits for the current uncommitted work. Inspect the git state first, including unstaged and staged changes. Group related changes into small coherent commits, staging only the files or hunks that belong together. Preserve unrelated user or agent work, do not discard changes, and do not amend existing commits unless clearly necessary. For each commit, use a concise conventional-style message when possible. Validate as appropriate for the changed files before committing, and report the commits created plus any remaining uncommitted changes.".to_string()
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SingleSessionTypography {
    pub(crate) family: &'static str,
    pub(crate) weight: &'static str,
    pub(crate) fallbacks: &'static [&'static str],
    pub(crate) title_size: f32,
    pub(crate) body_size: f32,
    pub(crate) meta_size: f32,
    pub(crate) code_size: f32,
    pub(crate) body_line_height: f32,
    pub(crate) code_line_height: f32,
    pub(crate) meta_line_height: f32,
}

pub(crate) const fn single_session_typography() -> SingleSessionTypography {
    SingleSessionTypography {
        family: SINGLE_SESSION_FONT_FAMILY,
        weight: SINGLE_SESSION_FONT_WEIGHT,
        fallbacks: SINGLE_SESSION_FONT_FALLBACKS,
        title_size: SINGLE_SESSION_TITLE_FONT_SIZE,
        body_size: SINGLE_SESSION_BODY_FONT_SIZE,
        meta_size: SINGLE_SESSION_META_FONT_SIZE,
        code_size: SINGLE_SESSION_CODE_FONT_SIZE,
        body_line_height: SINGLE_SESSION_BODY_LINE_HEIGHT,
        code_line_height: SINGLE_SESSION_CODE_LINE_HEIGHT,
        meta_line_height: SINGLE_SESSION_META_LINE_HEIGHT,
    }
}

pub(crate) fn single_session_typography_for_scale(scale: f32) -> SingleSessionTypography {
    let base = single_session_typography();
    let scale = scale.clamp(SINGLE_SESSION_MIN_TEXT_SCALE, SINGLE_SESSION_MAX_TEXT_SCALE);
    SingleSessionTypography {
        title_size: base.title_size * scale,
        body_size: base.body_size * scale,
        meta_size: base.meta_size * scale,
        code_size: base.code_size * scale,
        ..base
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SingleSessionApp {
    pub(crate) session: Option<workspace::SessionCard>,
    pub(crate) draft: String,
    pub(crate) draft_cursor: usize,
    pub(crate) detail_scroll: usize,
    pub(crate) live_session_id: Option<String>,
    pub(crate) messages: Vec<SingleSessionMessage>,
    pub(crate) streaming_response: String,
    pub(crate) status: Option<String>,
    status_kind: Option<SingleSessionStatus>,
    pub(crate) error: Option<String>,
    pub(crate) is_processing: bool,
    pub(crate) body_scroll_lines: f32,
    pub(crate) show_help: bool,
    pub(crate) show_session_info: bool,
    pub(crate) pending_images: Vec<(String, String)>,
    pub(crate) model_picker: ModelPickerState,
    pub(crate) session_switcher: SessionSwitcherState,
    pub(crate) stdin_response: Option<StdinResponseState>,
    slash_suggestions: SlashSuggestionState,
    runtime_settings: SingleSessionRuntimeSettings,
    welcome: SingleSessionWelcomeState,
    composer: SingleSessionComposerState,
    selection: SingleSessionSelectionState,
    runtime: SingleSessionRuntimeState,
    tool: SingleSessionToolState,
    view: SingleSessionViewState,
    side_panel: DesktopSidePanelState,
    pending_issue_sync_request: bool,
    /// Session id whose transcript should be hydrated from disk off the UI
    /// thread. Set when resuming from the session switcher; serviced by the
    /// event loop so large transcript parses never stall key handling.
    pending_transcript_hydration: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DesktopSidePanelState {
    pub(crate) visible: bool,
    pub(crate) focus: DesktopSidePanelFocus,
    pub(crate) github_issues: GitHubIssueBrowserState,
    pub(crate) github_issue_sync: GitHubIssueSyncUiState,
}

impl Default for DesktopSidePanelState {
    fn default() -> Self {
        Self {
            visible: false,
            focus: DesktopSidePanelFocus::Chat,
            github_issues: GitHubIssueBrowserState::sample(),
            github_issue_sync: GitHubIssueSyncUiState::default(),
        }
    }
}

impl DesktopSidePanelState {
    fn focus_next(&mut self) {
        self.focus = match self.focus {
            DesktopSidePanelFocus::IssueList => DesktopSidePanelFocus::IssuePreview,
            DesktopSidePanelFocus::IssuePreview => DesktopSidePanelFocus::Chat,
            DesktopSidePanelFocus::Chat => DesktopSidePanelFocus::IssueList,
        };
    }

    fn focus_previous(&mut self) {
        self.focus = match self.focus {
            DesktopSidePanelFocus::IssueList => DesktopSidePanelFocus::Chat,
            DesktopSidePanelFocus::IssuePreview => DesktopSidePanelFocus::IssueList,
            DesktopSidePanelFocus::Chat => DesktopSidePanelFocus::IssuePreview,
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DesktopSidePanelFocus {
    IssueList,
    IssuePreview,
    Chat,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GitHubIssueSyncUiState {
    pub(crate) syncing: bool,
    pub(crate) last_message: Option<String>,
    pub(crate) last_error: Option<String>,
}

impl GitHubIssueSyncUiState {
    pub(crate) fn label(&self) -> Option<String> {
        if self.syncing {
            return Some("syncing from GitHub in the background".to_string());
        }
        if let Some(error) = &self.last_error {
            return Some(format!("sync failed · {error}"));
        }
        self.last_message.clone()
    }

    pub(crate) fn guidance(&self) -> Option<String> {
        let error = self.last_error.as_deref()?;
        Some(issue_sync_error_guidance(error).to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitHubIssueBrowserState {
    pub(crate) repo: String,
    pub(crate) filter_label: String,
    pub(crate) selected: usize,
    pub(crate) list_scroll: usize,
    pub(crate) preview_scroll: usize,
    pub(crate) issues: Vec<GitHubIssuePreview>,
}

impl GitHubIssueBrowserState {
    fn sample() -> Self {
        Self {
            repo: "1jehuang/jcode".to_string(),
            filter_label: "priority · open · local cache".to_string(),
            selected: 0,
            list_scroll: 0,
            preview_scroll: 0,
            issues: vec![
                GitHubIssuePreview {
                    number: 342,
                    priority: "P0".to_string(),
                    title: "Desktop reload can lose the active chat surface".to_string(),
                    labels: vec!["bug".to_string(), "desktop".to_string(), "regression".to_string()],
                    age: "2d".to_string(),
                    comments: 8,
                    state: GitHubIssueVisualState::Selected,
                    body_lines: vec![
                        "When the desktop process reloads while a session is streaming, the window sometimes returns to the welcome state instead of the active chat.".to_string(),
                        "Expected: reload handoff preserves the session id, transcript, draft, and scroll position.".to_string(),
                        "Observed: the app opens, paints the shell, then falls back to a fresh session.".to_string(),
                    ],
                    comment_lines: vec![
                        "maintainer: happens more often after resizing during handoff".to_string(),
                        "agent note: likely snapshot restore ordering or worker init race".to_string(),
                    ],
                    priority_reason: "explicit regression label, data-loss risk, bounded desktop repro".to_string(),
                },
                GitHubIssuePreview {
                    number: 337,
                    priority: "P1".to_string(),
                    title: "Tool-card animation does too much work offscreen".to_string(),
                    labels: vec!["performance".to_string(), "desktop".to_string()],
                    age: "5d".to_string(),
                    comments: 4,
                    state: GitHubIssueVisualState::Idle,
                    body_lines: vec![
                        "Large transcripts still spend frame time walking tool-card metadata for rows far outside the viewport.".to_string(),
                        "The UI remains correct, but long sessions can miss frame budget during streaming.".to_string(),
                    ],
                    comment_lines: vec![
                        "profiling: check viewport clipping before card motion".to_string(),
                    ],
                    priority_reason: "perf label plus objective frame-time validation path".to_string(),
                },
                GitHubIssuePreview {
                    number: 329,
                    priority: "P2".to_string(),
                    title: "Provider auth errors should link to doctor output".to_string(),
                    labels: vec!["auth".to_string(), "ux".to_string()],
                    age: "1w".to_string(),
                    comments: 2,
                    state: GitHubIssueVisualState::Idle,
                    body_lines: vec![
                        "Desktop auth failures currently show a terse provider error.".to_string(),
                        "It should offer a one-click path to the same diagnostic information as `jcode auth doctor`.".to_string(),
                    ],
                    comment_lines: vec!["nice to have after core desktop stability".to_string()],
                    priority_reason: "important UX improvement, but not blocking active work".to_string(),
                },
            ],
        }
    }

    pub(crate) fn selected_issue(&self) -> Option<&GitHubIssuePreview> {
        self.issues.get(self.selected)
    }

    fn selected_issue_mut(&mut self) -> Option<&mut GitHubIssuePreview> {
        self.issues.get_mut(self.selected)
    }

    fn select_first(&mut self) {
        self.set_selected(0);
    }

    fn select_last(&mut self) {
        self.set_selected(self.issues.len().saturating_sub(1));
    }

    fn move_selection(&mut self, delta: i32) {
        if self.issues.is_empty() {
            self.selected = 0;
            self.list_scroll = 0;
            self.preview_scroll = 0;
            return;
        }
        let selected = self.selected as i32 + delta;
        self.set_selected(selected.clamp(0, self.issues.len().saturating_sub(1) as i32) as usize);
    }

    fn set_selected(&mut self, selected: usize) {
        if self.issues.is_empty() {
            self.selected = 0;
            self.list_scroll = 0;
            self.preview_scroll = 0;
            return;
        }
        self.selected = selected.min(self.issues.len() - 1);
        self.preview_scroll = 0;
        let visible_rows = 6usize;
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if self.selected >= self.list_scroll.saturating_add(visible_rows) {
            self.list_scroll = self.selected.saturating_sub(visible_rows - 1);
        }
        self.sync_visual_selection_state();
    }

    fn sync_visual_selection_state(&mut self) {
        for (index, issue) in self.issues.iter_mut().enumerate() {
            if issue.state != GitHubIssueVisualState::Active {
                issue.state = if index == self.selected {
                    GitHubIssueVisualState::Selected
                } else {
                    GitHubIssueVisualState::Idle
                };
            }
        }
    }

    fn scroll_preview_lines(&mut self, lines: i32) {
        let max_scroll = self
            .selected_issue()
            .map(|issue| issue.body_lines.len().saturating_sub(1))
            .unwrap_or_default();
        if lines > 0 {
            self.preview_scroll = self.preview_scroll.saturating_sub(lines as usize);
        } else if lines < 0 {
            self.preview_scroll = self
                .preview_scroll
                .saturating_add(lines.unsigned_abs() as usize)
                .min(max_scroll);
        }
    }

    fn mark_selected_active(&mut self) {
        for issue in &mut self.issues {
            if issue.state == GitHubIssueVisualState::Active {
                issue.state = GitHubIssueVisualState::Idle;
            }
        }
        if let Some(issue) = self.selected_issue_mut() {
            issue.state = GitHubIssueVisualState::Active;
        }
    }

    pub(crate) fn selected_issue_context_prompt(&self) -> Option<String> {
        let issue = self.selected_issue()?;
        Some(issue_context_prompt(&self.repo, issue))
    }
}

fn issue_context_prompt(repo: &str, issue: &GitHubIssuePreview) -> String {
    let labels = if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue.labels.join(", ")
    };
    let body = issue.body_lines.join("\n");
    let comments = if issue.comment_lines.is_empty() {
        "none".to_string()
    } else {
        issue.comment_lines.join("\n")
    };
    format!(
        "GitHub issue mission\n\nRepository: {repo}\nIssue: #{}\nTitle: {}\nPriority: {}\nLabels: {labels}\nAge: {}\nComment count: {}\nPriority rationale: {}\n\nIssue body:\n{body}\n\nRecent comments:\n{comments}\n\nMission objective: investigate and, when safe, implement a fix for this issue in the local repository.\n\nOperating instructions:\n1. Start by inspecting the relevant code and reproducing or narrowing the behavior.\n2. Preserve existing user changes and avoid destructive actions.\n3. If implementing a fix, add or update targeted tests.\n4. Run the maximum reasonable validation before reporting completion.\n5. Report evidence, remaining gaps, and any follow-up work.\n6. Do not rely on the GitHub web UI unless local cache context is insufficient.",
        issue.number, issue.title, issue.priority, issue.age, issue.comments, issue.priority_reason
    )
}

fn issue_sync_error_guidance(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("not installed")
        || lower.contains("not on path")
        || lower.contains("no such file")
    {
        "Install GitHub CLI `gh`, authenticate it, then press r or Ctrl+R to sync."
    } else if lower.contains("auth")
        || lower.contains("authentication")
        || lower.contains("login")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
    {
        "Run `gh auth login` or refresh GitHub CLI auth, then press r or Ctrl+R to sync."
    } else if lower.contains("could not find a github origin") || lower.contains("origin remote") {
        "Add a GitHub origin remote for this repository, then press r or Ctrl+R to sync."
    } else {
        "Using cached GitHub issues. Press r or Ctrl+R to retry background sync."
    }
}

fn compact_issue_sync_error(error: &str) -> String {
    let mut compact = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() > 160 {
        compact.truncate(157);
        compact.push_str("...");
    }
    if compact.is_empty() {
        "unknown error".to_string()
    } else {
        compact
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitHubIssueVisualState {
    Idle,
    Selected,
    Active,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitHubIssuePreview {
    pub(crate) number: u64,
    pub(crate) priority: String,
    pub(crate) title: String,
    pub(crate) labels: Vec<String>,
    pub(crate) age: String,
    pub(crate) comments: u32,
    pub(crate) state: GitHubIssueVisualState,
    pub(crate) body_lines: Vec<String>,
    pub(crate) comment_lines: Vec<String>,
    pub(crate) priority_reason: String,
}

#[derive(Clone, Debug)]
struct SingleSessionWelcomeState {
    name: Option<String>,
    recovery_session_count: usize,
    continuation_suggestion: Option<String>,
    // True for the fresh-start chat that owns the welcome hero as visual UI.
    // The hero must stay out of `body_styled_lines()` so it never becomes part
    // of the persisted/rendered transcript text.
    timeline: bool,
    hero_phrase_index: usize,
}

impl SingleSessionWelcomeState {
    fn new(has_session: bool) -> Self {
        let name = desktop_welcome_name();
        let hero_phrase_index = welcome_phrase_index(&name);
        // The continuation suggestion is only rendered on the fresh welcome
        // screen (when there is no session). Scanning external CLI history
        // (`~/.codex`, `~/.claude`) is expensive, so skip it entirely when a
        // session is present. This keeps workspace pane construction cheap,
        // which matters because workspace rendering builds one ephemeral
        // `SingleSessionApp` per visible surface every frame.
        let continuation_suggestion = if has_session {
            None
        } else {
            latest_external_cli_continuation_suggestion()
        };
        Self {
            name,
            recovery_session_count: 0,
            continuation_suggestion,
            timeline: !has_session,
            hero_phrase_index,
        }
    }

    fn reset_fresh(&mut self) {
        *self = Self::new(false);
    }
}

#[derive(Clone, Debug, Default)]
struct SingleSessionComposerState {
    queued_drafts: Vec<(String, Vec<(String, String)>)>,
    input_undo_stack: Vec<(String, usize)>,
}

#[derive(Clone, Debug, Default)]
struct SlashSuggestionState {
    selected: usize,
    query: String,
    dismissed_for_draft: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SingleSessionSelectionState {
    anchor: Option<SelectionPoint>,
    focus: Option<SelectionPoint>,
    draft_anchor: Option<SelectionPoint>,
    draft_focus: Option<SelectionPoint>,
}

#[derive(Clone, Debug)]
struct SingleSessionRuntimeState {
    session_handle: Option<DesktopSessionHandle>,
    reload_phase: ReloadPhase,
}

impl Default for SingleSessionRuntimeState {
    fn default() -> Self {
        Self {
            session_handle: None,
            reload_phase: ReloadPhase::Stable,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SingleSessionRuntimeSettings {
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    transport: Option<String>,
    compaction_mode: Option<String>,
    connection_type: Option<String>,
    status_detail: Option<String>,
    upstream_provider: Option<String>,
    token_usage: Option<SingleSessionTokenUsage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SingleSessionTokenUsage {
    input: u64,
    output: u64,
    cache_read_input: Option<u64>,
    cache_creation_input: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct SingleSessionToolState {
    active_message_index: Option<usize>,
    active_call_id: Option<String>,
    input_buffer: String,
    event_sequence: u64,
    runs: Vec<SingleSessionToolRun>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SingleSessionToolRun {
    pub(crate) call_id: String,
    pub(crate) message_index: usize,
    pub(crate) name: String,
    pub(crate) state: SingleSessionToolVisualState,
    pub(crate) summary: Option<String>,
    pub(crate) input_raw: String,
    pub(crate) input_preview: Option<String>,
    pub(crate) stdin_prompt: Option<String>,
    pub(crate) started_sequence: u64,
    pub(crate) updated_sequence: u64,
    pub(crate) completed_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SingleSessionToolVisualState {
    Preparing,
    Running,
    Succeeded,
    Failed,
    Unknown,
    Group,
}

impl SingleSessionToolVisualState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Succeeded => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Group => "tools",
        }
    }

    pub(crate) fn from_tool_state_text(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "preparing" | "pending" | "queued" | "waiting" => Self::Preparing,
            "running" | "executing" | "active" => Self::Running,
            "done" | "success" | "succeeded" | "passed" => Self::Succeeded,
            "failed" | "failure" | "error" | "errored" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Preparing | Self::Running)
    }
}

#[derive(Clone, Debug)]
struct SingleSessionViewState {
    inline_widget_opened_at: Option<Instant>,
    closing_inline_widget: Option<ClosingInlineWidgetState>,
    text_scale: f32,
}

#[derive(Clone, Debug)]
struct ClosingInlineWidgetState {
    kind: InlineWidgetKind,
    lines: Vec<SingleSessionStyledLine>,
    started_at: Instant,
}

impl Default for SingleSessionViewState {
    fn default() -> Self {
        Self {
            inline_widget_opened_at: None,
            closing_inline_widget: None,
            text_scale: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReloadPhase {
    Stable,
    AwaitingReconnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectionPoint {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectionLineSegment {
    pub(crate) line: usize,
    pub(crate) start_column: usize,
    pub(crate) end_column: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SingleSessionStyledLine {
    pub(crate) text: String,
    pub(crate) style: SingleSessionLineStyle,
    pub(crate) inline_spans: Vec<SingleSessionInlineSpan>,
    pub(crate) tool: Option<SingleSessionToolLineMetadata>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SingleSessionToolLineMetadata {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) state: SingleSessionToolVisualState,
    pub(crate) kind: SingleSessionToolLineKind,
    pub(crate) active: bool,
    pub(crate) expanded: bool,
    pub(crate) stdin_prompt: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SingleSessionToolLineKind {
    Header,
    Detail,
    GroupSummary,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SingleSessionInlineSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: SingleSessionInlineSpanKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SingleSessionInlineSpanKind {
    Code,
    Math,
    Strong,
    Emphasis,
    Strike,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadOnlyInlineWidget {
    pub(crate) title: String,
    pub(crate) lines: Vec<SingleSessionStyledLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InlineWidgetMode {
    ReadOnly,
    Interactive,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum InlineWidgetKind {
    HotkeyHelp,
    SessionInfo,
    ModelPicker,
    SessionSwitcher,
    SlashSuggestions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SingleSessionOverlay {
    None,
    StdinResponse,
    Inline {
        kind: InlineWidgetKind,
        mode: InlineWidgetMode,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReasoningEffortCycleOutcome {
    Set(String),
    AlreadyAtLimit { effort: String, limit: &'static str },
    Unavailable,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SingleSessionStatus {
    LoadingModels,
    LoadingRecentSessions,
    Receiving,
    Connected,
    SendingInteractiveInput,
    Cancelling,
    ServerReloading,
    ServerReconnected,
    InteractiveInputRequested,
    InteractiveInputPending,
    Ready,
    Sending,
    Error,
    ModelsLoaded,
    ModelPickerError,
    ModelSwitchFailed,
    ModelSelected(String),
    ToolPreparing(String),
    ToolUsing(String),
    ToolFinished { name: String, is_error: bool },
    AttachedImages(usize),
    Info(String),
    Backend(DesktopSessionStatus),
}

impl SingleSessionStatus {
    fn label(&self) -> String {
        match self {
            Self::LoadingModels => "loading models".to_string(),
            Self::LoadingRecentSessions => "loading recent sessions".to_string(),
            Self::Receiving => "receiving".to_string(),
            Self::Connected => "connected".to_string(),
            Self::SendingInteractiveInput => "sending interactive input".to_string(),
            Self::Cancelling => "cancelling".to_string(),
            Self::ServerReloading => "server reloading, reconnecting".to_string(),
            Self::ServerReconnected => "server reconnected".to_string(),
            Self::InteractiveInputRequested => "interactive input requested".to_string(),
            Self::InteractiveInputPending => {
                "interactive input pending · Esc to cancel".to_string()
            }
            Self::Ready => "ready".to_string(),
            Self::Sending => "sending".to_string(),
            Self::Error => "error".to_string(),
            Self::ModelsLoaded => "models loaded".to_string(),
            Self::ModelPickerError => "model picker error".to_string(),
            Self::ModelSwitchFailed => "model switch failed".to_string(),
            Self::ModelSelected(label) => format!("model: {label}"),
            Self::ToolPreparing(name) => format!("preparing tool {name}"),
            Self::ToolUsing(name) => format!("using tool {name}"),
            Self::ToolFinished { name, is_error } => {
                format!("tool {name} {}", if *is_error { "failed" } else { "done" })
            }
            Self::AttachedImages(count) => format!("attached {count} image(s)"),
            Self::Info(label) => label.clone(),
            Self::Backend(status) => status.label(),
        }
    }

    fn is_in_flight(&self) -> bool {
        match self {
            Self::LoadingModels
            | Self::LoadingRecentSessions
            | Self::Receiving
            | Self::Connected
            | Self::SendingInteractiveInput
            | Self::Cancelling
            | Self::Sending
            | Self::ToolPreparing(_)
            | Self::ToolUsing(_)
            | Self::AttachedImages(_) => true,
            Self::Backend(status) => status.is_in_flight(),
            Self::ServerReloading
            | Self::ServerReconnected
            | Self::InteractiveInputRequested
            | Self::InteractiveInputPending
            | Self::Ready
            | Self::Error
            | Self::ModelsLoaded
            | Self::ModelPickerError
            | Self::ModelSwitchFailed
            | Self::ModelSelected(_)
            | Self::ToolFinished { .. }
            | Self::Info(_) => false,
        }
    }
}

impl SingleSessionOverlay {
    pub(crate) fn blocks_composer_caret(self) -> bool {
        match self {
            Self::None => false,
            Self::StdinResponse => true,
            Self::Inline {
                kind: InlineWidgetKind::ModelPicker,
                mode: InlineWidgetMode::ReadOnly,
            } => false,
            Self::Inline {
                kind: InlineWidgetKind::SessionSwitcher,
                mode: InlineWidgetMode::ReadOnly,
            } => false,
            Self::Inline {
                kind: InlineWidgetKind::SlashSuggestions,
                mode: InlineWidgetMode::ReadOnly,
            } => false,
            Self::Inline { .. } => true,
        }
    }
}

impl InlineWidgetKind {
    pub(crate) fn mode(self, app: &SingleSessionApp) -> InlineWidgetMode {
        match self {
            Self::HotkeyHelp | Self::SessionInfo | Self::SlashSuggestions => {
                InlineWidgetMode::ReadOnly
            }
            Self::ModelPicker if app.model_picker.preview => InlineWidgetMode::ReadOnly,
            Self::ModelPicker => InlineWidgetMode::Interactive,
            Self::SessionSwitcher if app.session_switcher.preview => InlineWidgetMode::ReadOnly,
            Self::SessionSwitcher => InlineWidgetMode::Interactive,
        }
    }

    pub(crate) fn visible_line_limit(self) -> usize {
        match self {
            Self::HotkeyHelp => 18,
            // Compact type fits the whole panel including the closing rail
            // corner; truncating mid-panel leaves the box-drawing rail open.
            Self::SessionInfo => 18,
            Self::ModelPicker => usize::MAX,
            Self::SessionSwitcher => 24,
            Self::SlashSuggestions => DESKTOP_SLASH_SUGGESTION_ROW_LIMIT + 1,
        }
    }
}

impl ReadOnlyInlineWidget {
    fn new(title: impl Into<String>, lines: Vec<SingleSessionStyledLine>) -> Self {
        Self {
            title: title.into(),
            lines,
        }
    }

    fn styled_lines(self) -> Vec<SingleSessionStyledLine> {
        let mut styled = Vec::with_capacity(self.lines.len().saturating_add(2));
        styled.push(styled_line(
            self.title,
            SingleSessionLineStyle::OverlayTitle,
        ));
        if !self.lines.is_empty() {
            styled.push(blank_styled_line());
            styled.extend(self.lines);
        }
        styled
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum SingleSessionLineStyle {
    #[default]
    Assistant,
    AssistantHeading,
    AssistantQuote,
    AssistantTable,
    AssistantLink,
    AssistantMedia,
    CodeHeader,
    Code,
    User,
    UserContinuation,
    Tool,
    Meta,
    Status,
    Error,
    OverlayTitle,
    Overlay,
    OverlaySelection,
    Blank,
}

impl SingleSessionStyledLine {
    pub(crate) fn new(text: impl Into<String>, style: SingleSessionLineStyle) -> Self {
        Self {
            text: text.into(),
            style,
            inline_spans: Vec::new(),
            tool: None,
        }
    }

    pub(crate) fn with_inline_spans(
        text: impl Into<String>,
        style: SingleSessionLineStyle,
        inline_spans: Vec<SingleSessionInlineSpan>,
    ) -> Self {
        Self {
            text: text.into(),
            style,
            inline_spans,
            tool: None,
        }
    }

    pub(crate) fn with_tool_metadata(mut self, tool: SingleSessionToolLineMetadata) -> Self {
        self.tool = Some(tool);
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SingleSessionMessage {
    display: DisplayMessage,
    rich_attachments: Vec<desktop_rich_text::RichAttachment>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[allow(dead_code)]
pub(crate) enum SingleSessionRole {
    User,
    Assistant,
    Tool,
    System,
    Meta,
}

impl SingleSessionRole {
    pub(crate) fn is_user(self) -> bool {
        matches!(self, Self::User)
    }
}

fn rich_role_from_single_session_role(
    role: SingleSessionRole,
) -> desktop_rich_text::TranscriptRole {
    match role {
        SingleSessionRole::User => desktop_rich_text::TranscriptRole::User,
        SingleSessionRole::Assistant => desktop_rich_text::TranscriptRole::Assistant,
        SingleSessionRole::Tool => desktop_rich_text::TranscriptRole::Tool,
        SingleSessionRole::System => desktop_rich_text::TranscriptRole::System,
        SingleSessionRole::Meta => desktop_rich_text::TranscriptRole::Meta,
    }
}

impl SingleSessionMessage {
    pub(crate) fn user(content: impl Into<String>) -> Self {
        Self::from_display_message(DisplayMessage::user(content))
    }

    pub(crate) fn assistant(content: impl Into<String>) -> Self {
        Self::from_display_message(DisplayMessage::assistant(content))
    }

    pub(crate) fn tool(content: impl Into<String>) -> Self {
        Self::from_display_message(DisplayMessage::tool_text(content))
    }

    #[allow(dead_code)]
    pub(crate) fn system(content: impl Into<String>) -> Self {
        Self::from_display_message(DisplayMessage::system(content))
    }

    #[allow(dead_code)]
    pub(crate) fn meta(content: impl Into<String>) -> Self {
        Self::from_display_message(DisplayMessage::meta(content))
    }

    pub(crate) fn from_display_message(display: DisplayMessage) -> Self {
        Self {
            display,
            rich_attachments: Vec::new(),
        }
    }

    pub(crate) fn from_session_transcript(message: SessionTranscriptMessage) -> Self {
        match message.role.as_str() {
            "user" => Self::user(message.content),
            "assistant" => Self::assistant(message.content),
            "tool" => Self::tool(message.content),
            "system" | "background_task" => Self::system(message.content),
            _ => Self::meta(message.content),
        }
    }

    fn with_rich_attachments(
        mut self,
        attachments: Vec<desktop_rich_text::RichAttachment>,
    ) -> Self {
        self.rich_attachments = attachments;
        self
    }

    fn role(&self) -> SingleSessionRole {
        match self.display.role.as_str() {
            "user" => SingleSessionRole::User,
            "assistant" => SingleSessionRole::Assistant,
            "tool" => SingleSessionRole::Tool,
            "system" | "background_task" => SingleSessionRole::System,
            _ => SingleSessionRole::Meta,
        }
    }

    fn content(&self) -> &str {
        &self.display.content
    }

    fn set_content(&mut self, content: impl Into<String>) {
        self.display.content = content.into();
    }

    fn content_mut(&mut self) -> &mut String {
        &mut self.display.content
    }

    fn rich_attachments(&self) -> &[desktop_rich_text::RichAttachment] {
        &self.rich_attachments
    }
}

impl PartialEq for SingleSessionMessage {
    fn eq(&self, other: &Self) -> bool {
        self.display.role == other.display.role
            && self.display.content == other.display.content
            && self.rich_attachments == other.rich_attachments
    }
}

impl Eq for SingleSessionMessage {}

fn hash_messages_cache_fingerprint<H: Hasher>(messages: &[SingleSessionMessage], hasher: &mut H) {
    messages.len().hash(hasher);
    if messages.len() <= BODY_CACHE_MESSAGE_EDGE_COUNT * 2 + BODY_CACHE_MESSAGE_MIDDLE_SAMPLE_COUNT
    {
        for message in messages {
            hash_message_cache_fingerprint(message, hasher);
        }
        return;
    }

    for message in &messages[..BODY_CACHE_MESSAGE_EDGE_COUNT] {
        hash_message_cache_fingerprint(message, hasher);
    }
    let middle_start = BODY_CACHE_MESSAGE_EDGE_COUNT;
    let middle_len = messages
        .len()
        .saturating_sub(BODY_CACHE_MESSAGE_EDGE_COUNT * 2);
    for sample in 1..=BODY_CACHE_MESSAGE_MIDDLE_SAMPLE_COUNT {
        let index =
            middle_start + sample * middle_len / (BODY_CACHE_MESSAGE_MIDDLE_SAMPLE_COUNT + 1);
        index.hash(hasher);
        hash_message_cache_fingerprint(&messages[index], hasher);
    }
    for message in &messages[messages.len() - BODY_CACHE_MESSAGE_EDGE_COUNT..] {
        hash_message_cache_fingerprint(message, hasher);
    }
}

fn hash_message_cache_fingerprint<H: Hasher>(message: &SingleSessionMessage, hasher: &mut H) {
    message.role().hash(hasher);
    hash_text_cache_fingerprint(message.content(), hasher);
    message.rich_attachments.hash(hasher);
}

fn hash_text_cache_fingerprint<H: Hasher>(text: &str, hasher: &mut H) {
    let bytes = text.as_bytes();
    bytes.len().hash(hasher);
    if bytes.len() <= BODY_CACHE_TEXT_EDGE_BYTES * 2 {
        bytes.hash(hasher);
        return;
    }

    bytes[..BODY_CACHE_TEXT_EDGE_BYTES].hash(hasher);
    bytes[bytes.len() - BODY_CACHE_TEXT_EDGE_BYTES..].hash(hasher);
}

fn hash_tool_cache_fingerprint<H: Hasher>(tool: &SingleSessionToolState, hasher: &mut H) {
    tool.active_message_index.hash(hasher);
    tool.active_call_id.hash(hasher);
    visible_active_tool_input_preview(tool).hash(hasher);
    for run in &tool.runs {
        run.call_id.hash(hasher);
        run.message_index.hash(hasher);
        run.name.hash(hasher);
        run.state.hash(hasher);
        run.summary.hash(hasher);
        run.input_preview.hash(hasher);
        run.stdin_prompt.hash(hasher);
    }
}

fn visible_active_tool_input_preview(tool: &SingleSessionToolState) -> Option<String> {
    if tool.input_buffer.is_empty() {
        return None;
    }
    let tool_name = tool
        .active_call_id
        .as_ref()
        .and_then(|call_id| tool.runs.iter().find(|run| &run.call_id == call_id))
        .or_else(|| tool.runs.last())
        .map(|run| run.name.as_str())
        .unwrap_or("tool");
    compact_tool_metadata(&formatted_tool_input_lines(tool_name, &tool.input_buffer))
}

fn hash_session_switcher_cache_state<H: Hasher>(switcher: &SessionSwitcherState, hasher: &mut H) {
    switcher.open.hash(hasher);
    switcher.loading.hash(hasher);
    switcher.preview.hash(hasher);
    switcher.filter.hash(hasher);
    switcher.selected.hash(hasher);
    switcher.preview_scroll.hash(hasher);
    switcher.focus.hash(hasher);
    switcher
        .sessions
        .iter()
        .map(|session| {
            (
                session.session_id.as_str(),
                session.title.as_str(),
                session.subtitle.as_str(),
                session.detail.as_str(),
                session.preview_lines.as_slice(),
                session.detail_lines.as_slice(),
            )
        })
        .collect::<Vec<_>>()
        .hash(hasher);
}

impl SingleSessionApp {
    pub(crate) fn new(session: Option<workspace::SessionCard>) -> Self {
        let welcome = SingleSessionWelcomeState::new(session.is_some());
        let messages = session
            .as_ref()
            .filter(|session| !session.transcript_messages.is_empty())
            .map(|session| {
                session
                    .transcript_messages
                    .iter()
                    .cloned()
                    .map(SingleSessionMessage::from_session_transcript)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            session,
            draft: String::new(),
            draft_cursor: 0,
            detail_scroll: 0,
            live_session_id: None,
            messages,
            streaming_response: String::new(),
            status: None,
            status_kind: None,
            error: None,
            is_processing: false,
            body_scroll_lines: 0.0,
            show_help: false,
            show_session_info: false,
            pending_images: Vec::new(),
            model_picker: ModelPickerState::default(),
            session_switcher: SessionSwitcherState::default(),
            stdin_response: None,
            slash_suggestions: SlashSuggestionState::default(),
            runtime_settings: SingleSessionRuntimeSettings::default(),
            welcome,
            composer: SingleSessionComposerState::default(),
            selection: SingleSessionSelectionState::default(),
            runtime: SingleSessionRuntimeState::default(),
            tool: SingleSessionToolState::default(),
            view: SingleSessionViewState::default(),
            side_panel: DesktopSidePanelState::default(),
            pending_issue_sync_request: false,
            pending_transcript_hydration: None,
        }
    }

    pub(crate) fn replace_session(&mut self, session: Option<workspace::SessionCard>) {
        let replacing_with_session = session.is_some();
        self.session = session;
        if let Some(session) = &self.session {
            self.live_session_id = Some(session.session_id.clone());
        }
        if replacing_with_session
            && self.messages.is_empty()
            && self.streaming_response.is_empty()
            && self.error.is_none()
        {
            self.welcome.timeline = false;
        } else if !replacing_with_session {
            self.welcome.timeline = true;
        }
        self.detail_scroll = 0;
    }

    #[cfg(test)]
    pub(crate) fn reasoning_effort(&self) -> Option<&str> {
        self.runtime_settings.reasoning_effort.as_deref()
    }

    pub(crate) fn preview_reasoning_effort_set(&mut self, effort: &str) -> Option<String> {
        let normalized = self.normalize_reasoning_effort_for_current_context(effort)?;
        self.runtime_settings.reasoning_effort = Some(normalized.clone());
        self.set_status_label(format!("thinking level: {normalized}"));
        Some(normalized)
    }

    pub(crate) fn preview_reasoning_effort_cycle(
        &mut self,
        direction: i8,
    ) -> ReasoningEffortCycleOutcome {
        let efforts = self.available_reasoning_efforts_for_current_context();
        if efforts.is_empty() {
            self.set_status_label("thinking level is not available for this model");
            return ReasoningEffortCycleOutcome::Unavailable;
        }

        let current = self.runtime_settings.reasoning_effort.as_deref();
        let current_index = current
            .and_then(|effort| efforts.iter().position(|candidate| *candidate == effort))
            .unwrap_or(efforts.len() - 1);
        let next_index = if direction > 0 {
            (current_index + 1).min(efforts.len() - 1)
        } else {
            current_index.saturating_sub(1)
        };
        let next_effort = efforts[next_index];
        if next_index == current_index {
            let limit = if direction > 0 { "max" } else { "min" };
            self.set_status_label(format!(
                "thinking level: {next_effort} (already at {limit})"
            ));
            return ReasoningEffortCycleOutcome::AlreadyAtLimit {
                effort: next_effort.to_string(),
                limit,
            };
        }

        self.runtime_settings.reasoning_effort = Some(next_effort.to_string());
        self.set_status_label(format!("thinking level: {next_effort}"));
        ReasoningEffortCycleOutcome::Set(next_effort.to_string())
    }

    fn normalize_reasoning_effort_for_current_context(&self, raw: &str) -> Option<String> {
        let requested = raw.trim().to_ascii_lowercase();
        if requested.is_empty() {
            return None;
        }
        let efforts = self.available_reasoning_efforts_for_current_context();
        if efforts.is_empty() {
            return None;
        }
        if efforts.contains(&requested.as_str()) {
            return Some(requested);
        }
        if requested == "max" && efforts.contains(&"xhigh") {
            return Some("xhigh".to_string());
        }
        if requested == "xhigh" && efforts.contains(&"max") {
            return Some("max".to_string());
        }
        efforts.last().map(|effort| (*effort).to_string())
    }

    fn available_reasoning_efforts_for_current_context(&self) -> &'static [&'static str] {
        inferred_desktop_reasoning_efforts(
            self.model_picker.provider_name.as_deref(),
            self.model_picker.current_model.as_deref(),
            self.runtime_settings.reasoning_effort.as_deref(),
        )
    }

    pub(crate) fn initialize_resumed_session(&mut self, session_id: &str) {
        self.live_session_id = Some(session_id.to_string());
        self.detail_scroll = 0;
        self.messages.clear();
        self.streaming_response.clear();
        self.status = None;
        self.status_kind = None;
        self.error = None;
        self.stdin_response = None;
        self.body_scroll_lines = 0.0;
        self.show_help = false;
        self.show_session_info = false;
        self.is_processing = false;
        self.tool.active_message_index = None;
        self.tool.input_buffer.clear();
        self.runtime.reload_phase = ReloadPhase::Stable;
        self.view.inline_widget_opened_at = None;
        self.view.closing_inline_widget = None;
        self.welcome.timeline = false;
    }

    pub(crate) fn hydrate_resumed_session_from_disk(&mut self, session_id: &str) {
        match session_data::load_session_transcript_by_id(session_id) {
            Ok(Some(messages)) if !messages.is_empty() => {
                self.apply_resumed_session_transcript(messages);
            }
            Ok(_) => {}
            Err(error) => {
                crate::desktop_log::warn(format_args!(
                    "jcode-desktop: failed to hydrate resumed transcript for {session_id}: {error:#}"
                ));
                self.error = Some(format!("failed to load transcript: {error:#}"));
            }
        }
    }

    pub(crate) fn apply_resumed_session_transcript(
        &mut self,
        messages: Vec<SessionTranscriptMessage>,
    ) {
        self.messages = messages
            .into_iter()
            .map(SingleSessionMessage::from_session_transcript)
            .collect();
        self.streaming_response.clear();
        self.tool.active_message_index = None;
        self.tool.input_buffer.clear();
        self.welcome.timeline = false;
    }

    pub(crate) fn set_recovery_session_count(&mut self, count: usize) {
        self.welcome.recovery_session_count = count;
    }

    pub(crate) fn reset_fresh_session(&mut self) {
        self.session = None;
        self.draft.clear();
        self.draft_cursor = 0;
        self.detail_scroll = 0;
        self.live_session_id = None;
        self.messages.clear();
        self.streaming_response.clear();
        self.status = None;
        self.status_kind = None;
        self.error = None;
        self.is_processing = false;
        self.body_scroll_lines = 0.0;
        self.show_help = false;
        self.show_session_info = false;
        self.pending_images.clear();
        self.model_picker = ModelPickerState::default();
        self.session_switcher = SessionSwitcherState::default();
        self.stdin_response = None;
        self.welcome.reset_fresh();
        self.composer = SingleSessionComposerState::default();
        self.selection = SingleSessionSelectionState::default();
        self.runtime = SingleSessionRuntimeState::default();
        self.runtime_settings = SingleSessionRuntimeSettings::default();
        self.tool = SingleSessionToolState::default();
        self.view.inline_widget_opened_at = None;
        self.view.closing_inline_widget = None;
        self.side_panel = DesktopSidePanelState::default();
        self.pending_issue_sync_request = false;
    }

    pub(crate) fn side_panel(&self) -> &DesktopSidePanelState {
        &self.side_panel
    }

    pub(crate) fn take_github_issue_sync_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_issue_sync_request)
    }

    pub(crate) fn note_github_issue_sync_already_running(&mut self) {
        self.side_panel.github_issue_sync.syncing = true;
        self.side_panel.github_issue_sync.last_error = None;
        self.side_panel.github_issue_sync.last_message =
            Some("GitHub issue sync already running; cached issues remain interactive".to_string());
    }

    pub(crate) fn apply_github_issue_sync_result(
        &mut self,
        result: std::result::Result<crate::desktop_issue_cache::GitHubIssueSyncSummary, String>,
    ) {
        self.pending_issue_sync_request = false;
        self.side_panel.github_issue_sync.syncing = false;
        match result {
            Ok(summary) => {
                let warning_label = if summary.comment_fetch_errors == 0 {
                    String::new()
                } else {
                    format!(
                        " · {} comment refresh warning(s)",
                        summary.comment_fetch_errors
                    )
                };
                let message = format!(
                    "synced {} GitHub issues for {} in {}ms · cache {}{}",
                    summary.issue_count,
                    summary.repo,
                    summary.elapsed.as_millis(),
                    summary.cache_path.display(),
                    warning_label
                );
                self.side_panel.github_issues = summary.browser;
                self.side_panel.github_issue_sync.last_error = None;
                self.side_panel.github_issue_sync.last_message = Some(message.clone());
                self.set_status(SingleSessionStatus::Info(message));
            }
            Err(error) => {
                let compact_error = compact_issue_sync_error(&error);
                self.side_panel.github_issue_sync.last_error = Some(compact_error.clone());
                self.side_panel.github_issue_sync.last_message =
                    Some(issue_sync_error_guidance(&error).to_string());
                self.set_status(SingleSessionStatus::Info(format!(
                    "GitHub issue sync failed · {compact_error}"
                )));
            }
        }
    }

    pub(crate) fn issue_browser_visible(&self) -> bool {
        self.side_panel.visible
    }

    fn request_issue_browser_sync(&mut self) {
        self.pending_issue_sync_request = true;
        self.side_panel.github_issue_sync.syncing = true;
        self.side_panel.github_issue_sync.last_error = None;
        self.side_panel.github_issue_sync.last_message =
            Some("syncing from GitHub via gh; cached issues remain interactive".to_string());
    }

    fn toggle_issue_browser(&mut self, visible: Option<bool>) -> KeyOutcome {
        let visible = visible.unwrap_or(!self.side_panel.visible);
        self.side_panel.visible = visible;
        self.side_panel.focus = if visible {
            DesktopSidePanelFocus::IssueList
        } else {
            DesktopSidePanelFocus::Chat
        };
        let cache_status = visible
            .then(|| self.refresh_issue_browser_from_cache())
            .flatten();
        if visible {
            self.request_issue_browser_sync();
        }
        self.draft.clear();
        self.draft_cursor = 0;
        self.composer.input_undo_stack.clear();
        self.set_status(SingleSessionStatus::Info(cache_status.unwrap_or_else(
            || {
                if visible {
                    "showing local GitHub issue browser".to_string()
                } else {
                    "hid local GitHub issue browser".to_string()
                }
            },
        )));
        KeyOutcome::Redraw
    }

    #[cfg(not(test))]
    fn refresh_issue_browser_from_cache(&mut self) -> Option<String> {
        match crate::desktop_issue_cache::load_current_repo_issue_browser() {
            Ok(Some(browser)) => {
                let repo = browser.repo.clone();
                let count = browser.issues.len();
                self.side_panel.github_issues = browser;
                Some(format!("showing {count} cached GitHub issues for {repo}"))
            }
            Ok(None) => None,
            Err(error) => Some(format!(
                "showing sample issues; cache unavailable: {error:#}"
            )),
        }
    }

    #[cfg(test)]
    fn refresh_issue_browser_from_cache(&mut self) -> Option<String> {
        None
    }

    fn handle_issue_browser_key(&mut self, key: &KeyInput) -> Option<KeyOutcome> {
        if !self.side_panel.visible {
            return None;
        }

        if matches!(key, KeyInput::Autocomplete) && self.draft.is_empty() {
            self.side_panel.focus_next();
            return Some(KeyOutcome::Redraw);
        }

        if let KeyInput::Character(text) = key
            && text.starts_with('/')
        {
            self.side_panel.focus = DesktopSidePanelFocus::Chat;
            return None;
        }

        if matches!(key, KeyInput::RefreshSessions) {
            self.request_issue_browser_sync();
            return Some(KeyOutcome::Redraw);
        }

        match self.side_panel.focus {
            DesktopSidePanelFocus::Chat => None,
            DesktopSidePanelFocus::IssueList => Some(self.handle_issue_list_key(key)),
            DesktopSidePanelFocus::IssuePreview => Some(self.handle_issue_preview_key(key)),
        }
    }

    fn handle_issue_list_key(&mut self, key: &KeyInput) -> KeyOutcome {
        match key {
            KeyInput::Escape => {
                self.side_panel.focus = DesktopSidePanelFocus::Chat;
                KeyOutcome::Redraw
            }
            KeyInput::SubmitDraft => self.investigate_selected_issue(),
            KeyInput::Character(text) if text.eq_ignore_ascii_case("r") => {
                self.request_issue_browser_sync();
                KeyOutcome::Redraw
            }
            KeyInput::ModelPickerMove(delta) => {
                self.side_panel.github_issues.move_selection(*delta);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.side_panel.github_issues.move_selection(-pages * 5);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "j" => {
                self.side_panel.github_issues.move_selection(1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "k" => {
                self.side_panel.github_issues.move_selection(-1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "g" => {
                self.side_panel.github_issues.select_first();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "G" => {
                self.side_panel.github_issues.select_last();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "l" => {
                self.side_panel.focus = DesktopSidePanelFocus::IssuePreview;
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "h" => {
                self.side_panel.focus_previous();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text.eq_ignore_ascii_case("q") => {
                self.toggle_issue_browser(Some(false))
            }
            _ => KeyOutcome::None,
        }
    }

    fn handle_issue_preview_key(&mut self, key: &KeyInput) -> KeyOutcome {
        match key {
            KeyInput::Escape => {
                self.side_panel.focus = DesktopSidePanelFocus::Chat;
                KeyOutcome::Redraw
            }
            KeyInput::SubmitDraft => self.investigate_selected_issue(),
            KeyInput::Character(text) if text.eq_ignore_ascii_case("r") => {
                self.request_issue_browser_sync();
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyLines(lines) => {
                self.side_panel.github_issues.scroll_preview_lines(*lines);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.side_panel
                    .github_issues
                    .scroll_preview_lines(*pages * 6);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyToTop => {
                self.side_panel.github_issues.preview_scroll = 0;
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyToBottom => {
                self.side_panel
                    .github_issues
                    .scroll_preview_lines(i32::MIN + 1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "j" => {
                self.side_panel.github_issues.scroll_preview_lines(-1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "k" => {
                self.side_panel.github_issues.scroll_preview_lines(1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "h" => {
                self.side_panel.focus = DesktopSidePanelFocus::IssueList;
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "l" => {
                self.side_panel.focus = DesktopSidePanelFocus::Chat;
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text.eq_ignore_ascii_case("q") => {
                self.toggle_issue_browser(Some(false))
            }
            KeyInput::ModelPickerMove(delta) => {
                self.side_panel.github_issues.move_selection(*delta);
                KeyOutcome::Redraw
            }
            _ => KeyOutcome::None,
        }
    }

    fn investigate_selected_issue(&mut self) -> KeyOutcome {
        let Some(message) = self
            .side_panel
            .github_issues
            .selected_issue_context_prompt()
        else {
            return KeyOutcome::None;
        };
        self.side_panel.github_issues.mark_selected_active();
        self.side_panel.focus = DesktopSidePanelFocus::Chat;
        self.record_user_submit(&message, &[]);
        if let Some(session) = &self.session {
            KeyOutcome::SendDraft {
                session_id: session.session_id.clone(),
                title: session.title.clone(),
                message,
                images: Vec::new(),
            }
        } else {
            KeyOutcome::StartFreshSession {
                message,
                images: Vec::new(),
            }
        }
    }

    pub(crate) fn status_title(&self) -> String {
        format!("Jcode · {}", self.title())
    }

    pub(crate) fn title(&self) -> String {
        if let Some(session) = &self.session {
            session.title.clone()
        } else if let Some(session_id) = &self.live_session_id {
            format!("session {}", short_session_id(session_id))
        } else {
            "fresh session".to_string()
        }
    }

    pub(crate) fn header_title(&self) -> String {
        if self.should_show_session_title_header() {
            return self.title();
        }
        String::new()
    }

    pub(crate) fn should_show_session_title_header(&self) -> bool {
        self.messages.is_empty()
            && self.streaming_response.is_empty()
            && self.error.is_none()
            && !self.model_picker.open
            && !self.session_switcher.open
            && self.stdin_response.is_none()
            && !self.show_help
            && !self.show_session_info
            && self.session.is_some()
    }

    pub(crate) fn has_background_work(&self) -> bool {
        self.has_activity_indicator()
    }

    pub(crate) fn has_frame_animation(&self) -> bool {
        self.has_activity_indicator()
            || self.inline_widget_reveal_in_progress()
            || self.inline_widget_exit_in_progress()
    }

    fn mark_inline_widget_opened(&mut self) {
        self.view.inline_widget_opened_at = Some(Instant::now());
        self.view.closing_inline_widget = None;
    }

    fn capture_inline_widget_exit(&mut self) {
        if crate::animation::desktop_reduced_motion_enabled() {
            self.view.closing_inline_widget = None;
            return;
        }
        let Some(kind) = self.active_inline_widget() else {
            return;
        };
        let lines = self.inline_widget_styled_lines();
        self.capture_inline_widget_exit_snapshot(kind, lines);
    }

    fn capture_inline_widget_exit_snapshot(
        &mut self,
        kind: InlineWidgetKind,
        lines: Vec<SingleSessionStyledLine>,
    ) {
        if crate::animation::desktop_reduced_motion_enabled() {
            self.view.closing_inline_widget = None;
            return;
        }
        if lines.is_empty() {
            self.view.closing_inline_widget = None;
            return;
        }
        self.view.closing_inline_widget = Some(ClosingInlineWidgetState {
            kind,
            lines,
            started_at: Instant::now(),
        });
    }

    fn close_inline_widgets(&mut self) {
        self.capture_inline_widget_exit();
        self.show_help = false;
        self.show_session_info = false;
        self.model_picker.close();
        self.session_switcher.close();
        self.view.inline_widget_opened_at = None;
    }

    fn open_read_only_inline_widget(&mut self, kind: InlineWidgetKind) {
        self.close_inline_widgets();
        match kind {
            InlineWidgetKind::HotkeyHelp => self.show_help = true,
            InlineWidgetKind::SessionInfo => self.show_session_info = true,
            InlineWidgetKind::ModelPicker
            | InlineWidgetKind::SessionSwitcher
            | InlineWidgetKind::SlashSuggestions => {}
        }
        self.mark_inline_widget_opened();
    }

    fn toggle_read_only_inline_widget(&mut self, kind: InlineWidgetKind) -> KeyOutcome {
        let was_active = self.active_inline_widget() == Some(kind);
        self.close_inline_widgets();
        if !was_active {
            self.open_read_only_inline_widget(kind);
        }
        self.scroll_body_to_bottom();
        KeyOutcome::Redraw
    }

    fn inline_widget_reveal_in_progress(&self) -> bool {
        self.active_inline_widget().is_some() && self.inline_widget_reveal_progress() < 1.0
    }

    fn inline_widget_exit_in_progress(&self) -> bool {
        self.active_inline_widget().is_none() && self.render_inline_widget_reveal_progress() > 0.001
    }

    pub(crate) fn inline_widget_reveal_progress(&self) -> f32 {
        if self.active_inline_widget().is_none() {
            return 0.0;
        }
        if crate::animation::desktop_reduced_motion_enabled() {
            return 1.0;
        }

        #[cfg(test)]
        {
            1.0
        }

        #[cfg(not(test))]
        {
            let Some(opened_at) = self.view.inline_widget_opened_at else {
                return 1.0;
            };
            let raw = (opened_at.elapsed().as_secs_f32()
                / INLINE_WIDGET_REVEAL_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            1.0 - (1.0 - raw).powi(3)
        }
    }

    pub(crate) fn render_inline_widget_kind(&self) -> Option<InlineWidgetKind> {
        self.active_inline_widget().or_else(|| {
            (self.render_inline_widget_reveal_progress() > 0.001)
                .then(|| {
                    self.view
                        .closing_inline_widget
                        .as_ref()
                        .map(|closing| closing.kind)
                })
                .flatten()
        })
    }

    pub(crate) fn render_inline_widget_styled_lines(&self) -> Vec<SingleSessionStyledLine> {
        if self.active_inline_widget().is_some() {
            return self.inline_widget_styled_lines();
        }
        if self.render_inline_widget_reveal_progress() <= 0.001 {
            return Vec::new();
        }
        self.view
            .closing_inline_widget
            .as_ref()
            .map(|closing| closing.lines.clone())
            .unwrap_or_default()
    }

    pub(crate) fn render_inline_widget_line_count(&self) -> usize {
        if let Some(kind) = self.active_inline_widget() {
            return self.active_inline_widget_line_count(kind);
        }
        if self.render_inline_widget_reveal_progress() <= 0.001 {
            return 0;
        }
        self.view
            .closing_inline_widget
            .as_ref()
            .map(|closing| closing.lines.len())
            .unwrap_or(0)
    }

    fn active_inline_widget_line_count(&self, kind: InlineWidgetKind) -> usize {
        match kind {
            InlineWidgetKind::HotkeyHelp => hotkey_help_inline_line_count(),
            InlineWidgetKind::ModelPicker => model_picker_inline_line_count(&self.model_picker),
            InlineWidgetKind::SessionSwitcher => {
                session_switcher_line_count(&self.session_switcher, self.current_session_id())
            }
            InlineWidgetKind::SessionInfo => session_info_inline_line_count(self),
            InlineWidgetKind::SlashSuggestions => self.slash_suggestion_line_count(),
        }
    }

    pub(crate) fn render_inline_widget_visible_line_count(&self) -> usize {
        let line_count = self.render_inline_widget_line_count();
        let limit = self
            .render_inline_widget_kind()
            .map(InlineWidgetKind::visible_line_limit)
            .unwrap_or(INLINE_WIDGET_DEFAULT_VISIBLE_LINE_LIMIT);
        line_count.min(limit)
    }

    pub(crate) fn render_inline_widget_reveal_progress(&self) -> f32 {
        if self.active_inline_widget().is_some() {
            return self.inline_widget_reveal_progress();
        }
        if crate::animation::desktop_reduced_motion_enabled() {
            return 0.0;
        }
        let Some(closing) = &self.view.closing_inline_widget else {
            return 0.0;
        };
        let raw = (closing.started_at.elapsed().as_secs_f32()
            / INLINE_WIDGET_EXIT_DURATION.as_secs_f32())
        .clamp(0.0, 1.0);
        1.0 - raw.powi(3)
    }

    fn current_session_id(&self) -> Option<&str> {
        self.live_session_id.as_deref().or_else(|| {
            self.session
                .as_ref()
                .map(|session| session.session_id.as_str())
        })
    }

    pub(crate) fn user_turn_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| message.role().is_user())
            .count()
    }

    pub(crate) fn next_prompt_number(&self) -> usize {
        self.user_turn_count() + 1
    }

    pub(crate) fn composer_prompt(&self) -> String {
        format!("{}› ", self.next_prompt_number())
    }

    pub(crate) fn composer_text(&self) -> String {
        format!("{}{}", self.composer_prompt(), self.draft)
    }

    #[cfg(test)]
    pub(crate) fn queued_draft_count(&self) -> usize {
        self.composer.queued_drafts.len()
    }

    #[cfg(test)]
    pub(crate) fn queued_draft_messages(&self) -> Vec<String> {
        self.composer
            .queued_drafts
            .iter()
            .map(|(message, _)| message.clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn finish_inline_widget_exit_animation_for_test(&mut self) {
        if let Some(closing) = &mut self.view.closing_inline_widget {
            closing.started_at = Instant::now() - INLINE_WIDGET_EXIT_DURATION * 2;
        }
    }

    /// Fast-forward entry/exit animations so captures render the settled
    /// frame instead of a mid-reveal state. Used by the headless gallery
    /// screenshot tool.
    pub(crate) fn settle_animations_for_capture(&mut self) {
        if let Some(opened_at) = &mut self.view.inline_widget_opened_at {
            *opened_at = Instant::now() - INLINE_WIDGET_REVEAL_DURATION * 2;
        }
        if let Some(closing) = &mut self.view.closing_inline_widget {
            closing.started_at = Instant::now() - INLINE_WIDGET_EXIT_DURATION * 2;
        }
    }

    #[cfg(test)]
    pub(crate) fn activity_indicator_active(&self) -> bool {
        self.has_activity_indicator()
    }

    pub(crate) fn has_activity_indicator(&self) -> bool {
        self.is_processing
            || self.model_picker.loading
            || self.session_switcher.loading
            || self
                .status_kind
                .as_ref()
                .is_some_and(SingleSessionStatus::is_in_flight)
    }

    /// The standalone activity pill only shows while waiting for the first
    /// streamed token. Once text flows, the streaming tail cursor takes over
    /// as the "alive" cue at the end of the revealed text.
    pub(crate) fn streaming_activity_pill_visible(&self) -> bool {
        self.has_activity_indicator() && self.streaming_response.is_empty()
    }

    fn set_status(&mut self, status: SingleSessionStatus) {
        self.status = Some(status.label());
        self.status_kind = Some(status);
    }

    pub(crate) fn set_status_label(&mut self, label: impl Into<String>) {
        self.set_status(SingleSessionStatus::Info(label.into()));
    }

    fn set_backend_status(&mut self, status: DesktopSessionStatus) {
        match &status {
            DesktopSessionStatus::ReasoningEffort(effort) => {
                self.runtime_settings.reasoning_effort = Some(effort.clone());
                self.messages.push(SingleSessionMessage::meta(format!(
                    "thinking level set to {effort}"
                )));
            }
            DesktopSessionStatus::ServiceTier(service_tier) => {
                self.runtime_settings.service_tier = Some(service_tier.clone());
                self.messages.push(SingleSessionMessage::meta(format!(
                    "fast mode set to {service_tier}"
                )));
            }
            DesktopSessionStatus::Transport(transport) => {
                self.runtime_settings.transport = Some(transport.clone());
                self.messages.push(SingleSessionMessage::meta(format!(
                    "transport set to {transport}"
                )));
            }
            DesktopSessionStatus::CompactionMode(mode) => {
                self.runtime_settings.compaction_mode = Some(mode.clone());
                self.messages.push(SingleSessionMessage::meta(format!(
                    "compaction mode set to {mode}"
                )));
            }
            DesktopSessionStatus::ReasoningEffortFailed(error)
            | DesktopSessionStatus::ServiceTierFailed(error)
            | DesktopSessionStatus::TransportFailed(error)
            | DesktopSessionStatus::CompactionModeFailed(error) => {
                self.messages.push(SingleSessionMessage::meta(format!(
                    "slash command failed: {error}"
                )));
            }
            DesktopSessionStatus::CompactResult { message, .. } => {
                self.messages
                    .push(SingleSessionMessage::meta(message.clone()));
            }
            _ => {}
        }
        self.set_status(SingleSessionStatus::Backend(status));
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn status_kind(&self) -> Option<&SingleSessionStatus> {
        self.status_kind.as_ref()
    }

    pub(crate) fn handle_key(&mut self, key: KeyInput) -> KeyOutcome {
        if key == KeyInput::ExitApp {
            return KeyOutcome::Exit;
        }

        if self.stdin_response.is_some() {
            return self.handle_stdin_response_key(key);
        }

        if self.session_switcher.open
            && self.session_switcher.preview
            && let Some(outcome) = self.handle_session_switcher_preview_key(&key)
        {
            return outcome;
        }

        if self.session_switcher.open {
            return self.handle_session_switcher_key(key);
        }

        if matches!(
            self.active_inline_widget_mode(),
            Some(InlineWidgetMode::Interactive)
        ) && self.model_picker.open
        {
            return self.handle_model_picker_key(key);
        }

        if self.model_picker.open
            && self.model_picker.preview
            && let Some(outcome) = self.handle_model_picker_preview_key(&key)
        {
            return outcome;
        }

        if self.active_inline_widget() == Some(InlineWidgetKind::SlashSuggestions)
            && let Some(outcome) = self.handle_slash_suggestion_key(&key)
        {
            return outcome;
        }

        if let Some(outcome) = self.handle_issue_browser_key(&key) {
            return outcome;
        }

        match key {
            KeyInput::SpawnPanel => KeyOutcome::SpawnSession,
            KeyInput::SpawnSelfDevSession => KeyOutcome::SpawnSelfDevSession,
            KeyInput::SpawnHomeSession => KeyOutcome::SpawnHomeSession,
            KeyInput::OpenSessionSwitcher => self.open_session_switcher(),
            KeyInput::OpenModelPicker => self.open_model_picker(),
            KeyInput::HotkeyHelp => {
                self.toggle_read_only_inline_widget(InlineWidgetKind::HotkeyHelp)
            }
            KeyInput::ToggleSessionInfo => {
                self.toggle_read_only_inline_widget(InlineWidgetKind::SessionInfo)
            }
            KeyInput::RefreshSessions if self.welcome.recovery_session_count > 0 => {
                KeyOutcome::RestoreCrashedSessions
            }
            KeyInput::RefreshSessions => KeyOutcome::Redraw,
            KeyInput::ExitApp => KeyOutcome::Exit,
            KeyInput::AdjustTextScale(direction) => {
                self.adjust_text_scale(direction);
                KeyOutcome::Redraw
            }
            KeyInput::ResetTextScale => {
                self.view.text_scale = 1.0;
                KeyOutcome::Redraw
            }
            KeyInput::CancelGeneration => {
                if self.is_processing {
                    KeyOutcome::CancelGeneration
                } else {
                    KeyOutcome::None
                }
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.scroll_body_lines((pages * 12) as f32);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyLines(lines) => {
                self.scroll_body_lines(lines as f32);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyToTop => {
                self.scroll_body_to_top();
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyToBottom => {
                self.scroll_body_to_bottom();
                KeyOutcome::Redraw
            }
            KeyInput::JumpPrompt(direction) => {
                self.jump_prompt(direction);
                KeyOutcome::Redraw
            }
            KeyInput::CopyLatestResponse => self
                .latest_assistant_response()
                .map(KeyOutcome::CopyLatestResponse)
                .unwrap_or(KeyOutcome::None),
            KeyInput::CopyLatestCodeBlock => self.copy_latest_code_block(),
            KeyInput::CopyTranscript => self.copy_transcript(),
            KeyInput::ModelPickerMove(_) => KeyOutcome::None,
            KeyInput::CycleModel(direction) => KeyOutcome::CycleModel(direction),
            KeyInput::CycleReasoningEffort(direction) => {
                KeyOutcome::CycleReasoningEffort(direction)
            }
            KeyInput::AttachClipboardImage => KeyOutcome::AttachClipboardImage,
            KeyInput::ClearAttachedImages => {
                if self.clear_attached_images() {
                    KeyOutcome::Redraw
                } else {
                    KeyOutcome::None
                }
            }
            KeyInput::PasteText => KeyOutcome::PasteText,
            KeyInput::QueueDraft if self.is_processing => self.queue_draft(),
            KeyInput::RetrieveQueuedDraft => self.retrieve_queued_draft_for_edit(),
            KeyInput::QueueDraft => self.submit_draft(),
            KeyInput::SubmitDraft => self.submit_draft(),
            KeyInput::Escape if self.show_help => {
                self.close_inline_widgets();
                KeyOutcome::Redraw
            }
            KeyInput::Escape if self.show_session_info => {
                self.close_inline_widgets();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text)
                if (self.show_help || self.show_session_info) && text.eq_ignore_ascii_case("q") =>
            {
                self.close_inline_widgets();
                KeyOutcome::Redraw
            }
            KeyInput::Escape => {
                if self.is_processing {
                    KeyOutcome::CancelGeneration
                } else {
                    self.clear_draft_for_escape()
                }
            }
            KeyInput::Enter => {
                self.insert_draft_text("\n");
                KeyOutcome::Redraw
            }
            KeyInput::Backspace => {
                self.delete_previous_char();
                self.sync_inline_previews_from_draft()
                    .unwrap_or(KeyOutcome::Redraw)
            }
            KeyInput::DeletePreviousWord => {
                self.delete_previous_word();
                self.sync_inline_previews_from_draft()
                    .unwrap_or(KeyOutcome::Redraw)
            }
            KeyInput::DeleteNextWord => {
                self.delete_next_word();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::DeleteNextChar => {
                self.delete_next_char();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorWordLeft => {
                self.move_cursor_word_left();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorWordRight => {
                self.move_cursor_word_right();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorLeft => {
                self.move_cursor_left();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorRight => {
                self.move_cursor_right();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineStart => {
                self.move_to_line_start();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineEnd => {
                self.move_to_line_end();
                self.sync_slash_suggestions_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::DeleteToLineStart => {
                self.delete_to_line_start();
                self.sync_inline_previews_from_draft()
                    .unwrap_or(KeyOutcome::Redraw)
            }
            KeyInput::DeleteToLineEnd => {
                self.delete_to_line_end();
                self.sync_inline_previews_from_draft()
                    .unwrap_or(KeyOutcome::Redraw)
            }
            KeyInput::CutInputLine => self.cut_input_line(),
            KeyInput::UndoInput => {
                self.undo_input_change();
                self.sync_inline_previews_from_draft();
                KeyOutcome::Redraw
            }
            KeyInput::Autocomplete => self.autocomplete_draft(),
            KeyInput::Character(text) => {
                self.insert_draft_text(&text);
                self.sync_inline_previews_from_draft()
                    .unwrap_or(KeyOutcome::Redraw)
            }
            _ => KeyOutcome::None,
        }
    }

    pub(crate) fn text_scale(&self) -> f32 {
        self.view.text_scale
    }

    pub(crate) fn has_active_selection(&self) -> bool {
        self.selection.anchor.is_some()
            || self.selection.focus.is_some()
            || self.selection.draft_anchor.is_some()
            || self.selection.draft_focus.is_some()
    }

    fn adjust_text_scale(&mut self, direction: i8) {
        let delta = direction as f32 * SINGLE_SESSION_TEXT_SCALE_STEP;
        self.view.text_scale = (self.view.text_scale + delta)
            .clamp(SINGLE_SESSION_MIN_TEXT_SCALE, SINGLE_SESSION_MAX_TEXT_SCALE);
    }

    fn open_model_picker(&mut self) -> KeyOutcome {
        let was_open = self.model_picker.open;
        self.close_inline_widgets();
        self.model_picker.open_loading();
        if !was_open {
            self.mark_inline_widget_opened();
        }
        self.set_status(SingleSessionStatus::LoadingModels);
        self.scroll_body_to_bottom();
        KeyOutcome::LoadModelCatalog
    }

    fn open_model_picker_preview(&mut self, filter: String) -> KeyOutcome {
        let was_open = self.model_picker.open;
        self.close_inline_widgets();
        self.model_picker.open_preview_loading(filter);
        if !was_open {
            self.mark_inline_widget_opened();
        }
        self.set_status(SingleSessionStatus::LoadingModels);
        self.scroll_body_to_bottom();
        KeyOutcome::LoadModelCatalog
    }

    fn open_session_switcher_preview(&mut self, filter: String) -> KeyOutcome {
        let was_open = self.session_switcher.open;
        self.close_inline_widgets();
        let current_session_id = self.current_session_id().map(str::to_string);
        self.session_switcher
            .open_preview_loading(current_session_id.as_deref(), filter);
        if !was_open {
            self.mark_inline_widget_opened();
        }
        self.set_status(SingleSessionStatus::LoadingRecentSessions);
        self.scroll_body_to_bottom();
        KeyOutcome::LoadSessionSwitcher
    }

    fn sync_model_picker_preview_from_draft(&mut self) -> Option<KeyOutcome> {
        let Some(filter) = model_picker_preview_filter(&self.draft) else {
            if self.model_picker.open && self.model_picker.preview {
                self.capture_inline_widget_exit();
                self.model_picker.close();
                return Some(KeyOutcome::Redraw);
            }
            return None;
        };

        if self.model_picker.open && self.model_picker.preview {
            self.model_picker.set_filter(filter);
            Some(KeyOutcome::Redraw)
        } else {
            Some(self.open_model_picker_preview(filter))
        }
    }

    fn sync_session_switcher_preview_from_draft(&mut self) -> Option<KeyOutcome> {
        if !self.pending_images.is_empty() {
            if self.session_switcher.open && self.session_switcher.preview {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                return Some(KeyOutcome::Redraw);
            }
            return None;
        }

        let Some(filter) = session_switcher_preview_filter(&self.draft) else {
            if self.session_switcher.open && self.session_switcher.preview {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                return Some(KeyOutcome::Redraw);
            }
            return None;
        };

        if self.session_switcher.open && self.session_switcher.preview {
            self.session_switcher.set_filter(filter);
            Some(KeyOutcome::Redraw)
        } else {
            Some(self.open_session_switcher_preview(filter))
        }
    }

    fn sync_inline_previews_from_draft(&mut self) -> Option<KeyOutcome> {
        self.sync_slash_suggestions_from_draft();
        self.sync_model_picker_preview_from_draft()
            .or_else(|| self.sync_session_switcher_preview_from_draft())
    }

    fn sync_slash_suggestions_from_draft(&mut self) {
        let was_visible = self.slash_suggestions_visible();
        let Some(query) = slash_suggestion_query(&self.draft, self.draft_cursor) else {
            if was_visible
                && self.active_inline_widget() == Some(InlineWidgetKind::SlashSuggestions)
            {
                self.capture_inline_widget_exit();
            }
            self.slash_suggestions.query.clear();
            self.slash_suggestions.selected = 0;
            return;
        };

        if self
            .slash_suggestions
            .dismissed_for_draft
            .as_deref()
            .is_some_and(|dismissed| dismissed != self.draft)
        {
            self.slash_suggestions.dismissed_for_draft = None;
        }

        let previous_slash_lines = (was_visible
            && self.active_inline_widget() == Some(InlineWidgetKind::SlashSuggestions))
        .then(|| self.inline_widget_styled_lines());
        if self.slash_suggestions.query != query {
            self.slash_suggestions.query = query;
            self.slash_suggestions.selected = 0;
        }
        let candidate_count = self.slash_suggestion_candidates().len();
        if candidate_count == 0 {
            if let Some(lines) = previous_slash_lines {
                self.capture_inline_widget_exit_snapshot(InlineWidgetKind::SlashSuggestions, lines);
            }
            self.slash_suggestions.selected = 0;
            return;
        }
        self.slash_suggestions.selected = self.slash_suggestions.selected.min(candidate_count - 1);
        if !was_visible {
            self.mark_inline_widget_opened();
            self.scroll_body_to_bottom();
        }
    }

    fn handle_slash_suggestion_key(&mut self, key: &KeyInput) -> Option<KeyOutcome> {
        match key {
            KeyInput::Escape => {
                self.capture_inline_widget_exit();
                self.slash_suggestions.dismissed_for_draft = Some(self.draft.clone());
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ModelPickerMove(delta) => {
                self.move_slash_suggestion_selection(*delta);
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.move_slash_suggestion_selection(if *pages > 0 { -5 } else { 5 });
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Autocomplete => self.complete_selected_slash_suggestion(),
            KeyInput::SubmitDraft => {
                self.capture_inline_widget_exit();
                self.complete_selected_slash_suggestion();
                Some(self.submit_draft())
            }
            _ => None,
        }
    }

    fn move_slash_suggestion_selection(&mut self, delta: i32) {
        let count = self.slash_suggestion_candidates().len();
        if count == 0 {
            self.slash_suggestions.selected = 0;
            return;
        }
        let selected = self.slash_suggestions.selected as i32 + delta;
        self.slash_suggestions.selected =
            selected.clamp(0, count.saturating_sub(1) as i32) as usize;
    }

    fn complete_selected_slash_suggestion(&mut self) -> Option<KeyOutcome> {
        let candidates = self.slash_suggestion_candidates();
        let selected = self
            .slash_suggestions
            .selected
            .min(candidates.len().saturating_sub(1));
        let (usage, _) = candidates.get(selected).copied()?;
        let command = usage.split_whitespace().next().unwrap_or(usage);
        let (start, end) = slash_suggestion_prefix_bounds(&self.draft, self.draft_cursor)?;
        if self.draft.get(start..end) == Some(command) {
            return None;
        }
        self.remember_input_undo_state();
        self.draft.replace_range(start..end, command);
        self.draft_cursor = start + command.len();
        self.clear_draft_selection();
        self.slash_suggestions.dismissed_for_draft = None;
        self.slash_suggestions.query = command.to_string();
        self.slash_suggestions.selected = selected;
        Some(KeyOutcome::Redraw)
    }

    fn handle_model_picker_preview_key(&mut self, key: &KeyInput) -> Option<KeyOutcome> {
        match key {
            KeyInput::Character(text) if text == "j" => {
                self.model_picker.move_selection(1);
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "k" => {
                self.model_picker.move_selection(-1);
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "g" => {
                self.model_picker.select_first();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "G" => {
                self.model_picker.select_last();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "q" => {
                self.capture_inline_widget_exit();
                self.model_picker.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Escape => {
                self.capture_inline_widget_exit();
                self.model_picker.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ModelPickerMove(delta) => {
                self.model_picker.move_selection(*delta);
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.model_picker
                    .move_selection(if *pages > 0 { -5 } else { 5 });
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveToLineStart => {
                self.model_picker.select_first();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveToLineEnd => {
                self.model_picker.select_last();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::SubmitDraft => {
                let Some(model) = self.model_picker.selected_model() else {
                    self.capture_inline_widget_exit();
                    self.model_picker.close();
                    self.draft.clear();
                    self.draft_cursor = 0;
                    self.composer.input_undo_stack.clear();
                    return Some(KeyOutcome::Redraw);
                };
                self.capture_inline_widget_exit();
                self.model_picker.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::SetModel(model))
            }
            KeyInput::RefreshSessions => {
                let filter = self.model_picker.filter.clone();
                self.model_picker.open_preview_loading(filter);
                self.set_status(SingleSessionStatus::LoadingModels);
                Some(KeyOutcome::LoadModelCatalog)
            }
            _ => None,
        }
    }

    fn handle_session_switcher_preview_key(&mut self, key: &KeyInput) -> Option<KeyOutcome> {
        match key {
            KeyInput::Character(text) if text == "j" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(1);
                } else {
                    self.session_switcher.move_selection(1);
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "k" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(-1);
                } else {
                    self.session_switcher.move_selection(-1);
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "h" => {
                self.session_switcher.focus_sessions();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "l" => {
                self.session_switcher.focus_preview();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "g" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll = 0;
                } else {
                    self.session_switcher.select_first();
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "G" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll =
                        self.session_switcher.preview_line_count().saturating_sub(1);
                } else {
                    self.session_switcher.select_last();
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Character(text) if text == "q" => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Escape => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ModelPickerMove(delta) => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(*delta);
                } else {
                    self.session_switcher.move_selection(*delta);
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::ScrollBodyPages(pages) => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher
                        .scroll_preview(if *pages > 0 { -8 } else { 8 });
                } else {
                    self.session_switcher
                        .move_selection(if *pages > 0 { -5 } else { 5 });
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::Autocomplete => {
                self.session_switcher.toggle_focus();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveCursorLeft => {
                self.session_switcher.focus_sessions();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveCursorRight => {
                self.session_switcher.focus_preview();
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveToLineStart => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll = 0;
                } else {
                    self.session_switcher.select_first();
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::MoveToLineEnd => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll =
                        self.session_switcher.preview_line_count().saturating_sub(1);
                } else {
                    self.session_switcher.select_last();
                }
                Some(KeyOutcome::Redraw)
            }
            KeyInput::SubmitDraft => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(self.resume_selected_switcher_session())
            }
            KeyInput::QueueDraft => {
                let Some(session) = self.session_switcher.selected_session() else {
                    return Some(KeyOutcome::None);
                };
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                Some(KeyOutcome::OpenSession {
                    session_id: session.session_id,
                    title: session.title,
                })
            }
            KeyInput::RefreshSessions => {
                let current_session_id = self.current_session_id().map(str::to_string);
                let filter = self.session_switcher.filter.clone();
                self.session_switcher
                    .open_preview_loading(current_session_id.as_deref(), filter);
                self.set_status(SingleSessionStatus::LoadingRecentSessions);
                Some(KeyOutcome::LoadSessionSwitcher)
            }
            _ => None,
        }
    }

    fn open_session_switcher(&mut self) -> KeyOutcome {
        self.close_inline_widgets();
        let current_session_id = self.current_session_id().map(str::to_string);
        self.session_switcher
            .open_loading(current_session_id.as_deref());
        self.set_status(SingleSessionStatus::LoadingRecentSessions);
        self.scroll_body_to_bottom();
        self.mark_inline_widget_opened();
        KeyOutcome::LoadSessionSwitcher
    }

    fn handle_model_picker_key(&mut self, key: KeyInput) -> KeyOutcome {
        match key {
            KeyInput::Character(text) if text == "j" => {
                self.model_picker.move_selection(1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "k" => {
                self.model_picker.move_selection(-1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "h" => {
                self.model_picker.column = self.model_picker.column.saturating_sub(1);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "l" => {
                self.model_picker.column = (self.model_picker.column + 1).min(2);
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "g" => {
                self.model_picker.select_first();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "G" => {
                self.model_picker.select_last();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "q" => {
                self.capture_inline_widget_exit();
                self.model_picker.close();
                KeyOutcome::Redraw
            }
            KeyInput::Escape if !self.model_picker.filter.is_empty() => {
                self.model_picker.set_filter(String::new());
                KeyOutcome::Redraw
            }
            KeyInput::Escape | KeyInput::OpenModelPicker => {
                self.capture_inline_widget_exit();
                self.model_picker.close();
                KeyOutcome::Redraw
            }
            KeyInput::OpenSessionSwitcher => self.open_session_switcher(),
            KeyInput::RefreshSessions => {
                self.model_picker.open_loading();
                self.set_status(SingleSessionStatus::LoadingModels);
                KeyOutcome::LoadModelCatalog
            }
            KeyInput::ModelPickerMove(delta) => {
                self.model_picker.move_selection(delta);
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyPages(pages) => {
                self.model_picker
                    .move_selection(if pages > 0 { -5 } else { 5 });
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineStart => {
                self.model_picker.select_first();
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineEnd => {
                self.model_picker.select_last();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorRight => {
                self.model_picker.column = (self.model_picker.column + 1).min(2);
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorLeft => {
                self.model_picker.column = self.model_picker.column.saturating_sub(1);
                KeyOutcome::Redraw
            }
            KeyInput::CycleModel(direction) => KeyOutcome::CycleModel(direction),
            KeyInput::SubmitDraft => {
                let Some(model) = self.model_picker.selected_model() else {
                    return KeyOutcome::None;
                };
                self.capture_inline_widget_exit();
                self.model_picker.close();
                KeyOutcome::SetModel(model)
            }
            KeyInput::Backspace => {
                self.model_picker.pop_filter_char();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) => {
                self.model_picker.push_filter_text(&text);
                KeyOutcome::Redraw
            }
            KeyInput::HotkeyHelp => {
                self.open_read_only_inline_widget(InlineWidgetKind::HotkeyHelp);
                KeyOutcome::Redraw
            }
            _ => KeyOutcome::None,
        }
    }

    fn handle_session_switcher_key(&mut self, key: KeyInput) -> KeyOutcome {
        match key {
            KeyInput::Character(text) if text == "j" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(1);
                } else {
                    self.session_switcher.move_selection(1);
                }
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "k" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(-1);
                } else {
                    self.session_switcher.move_selection(-1);
                }
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "h" => {
                self.session_switcher.focus_sessions();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "l" => {
                self.session_switcher.focus_preview();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "g" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll = 0;
                } else {
                    self.session_switcher.select_first();
                }
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "G" => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll =
                        self.session_switcher.preview_line_count().saturating_sub(1);
                } else {
                    self.session_switcher.select_last();
                }
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) if text == "q" => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::Redraw
            }
            KeyInput::Escape | KeyInput::OpenSessionSwitcher => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::Redraw
            }
            KeyInput::Autocomplete => {
                self.session_switcher.toggle_focus();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorLeft => {
                self.session_switcher.focus_sessions();
                KeyOutcome::Redraw
            }
            KeyInput::MoveCursorRight => {
                self.session_switcher.focus_preview();
                KeyOutcome::Redraw
            }
            KeyInput::RefreshSessions => {
                let current_session_id = self.current_session_id().map(str::to_string);
                self.session_switcher
                    .refresh_loading(current_session_id.as_deref());
                self.set_status(SingleSessionStatus::LoadingRecentSessions);
                self.mark_inline_widget_opened();
                KeyOutcome::LoadSessionSwitcher
            }
            KeyInput::ModelPickerMove(delta) => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.scroll_preview(delta);
                } else {
                    self.session_switcher.move_selection(delta);
                }
                KeyOutcome::Redraw
            }
            KeyInput::ScrollBodyPages(pages) => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher
                        .scroll_preview(if pages > 0 { -8 } else { 8 });
                } else {
                    self.session_switcher
                        .move_selection(if pages > 0 { -5 } else { 5 });
                }
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineStart => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll = 0;
                } else {
                    self.session_switcher.select_first();
                }
                KeyOutcome::Redraw
            }
            KeyInput::MoveToLineEnd => {
                if self.session_switcher.focus == SessionSwitcherPane::Preview {
                    self.session_switcher.preview_scroll =
                        self.session_switcher.preview_line_count().saturating_sub(1);
                } else {
                    self.session_switcher.select_last();
                }
                KeyOutcome::Redraw
            }
            KeyInput::QueueDraft => {
                let Some(session) = self.session_switcher.selected_session() else {
                    return KeyOutcome::None;
                };
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::OpenSession {
                    session_id: session.session_id,
                    title: session.title,
                }
            }
            KeyInput::SubmitDraft => self.resume_selected_switcher_session(),
            KeyInput::Backspace => {
                self.session_switcher.pop_filter_char();
                KeyOutcome::Redraw
            }
            KeyInput::Character(text) => {
                self.session_switcher.push_filter_text(&text);
                KeyOutcome::Redraw
            }
            KeyInput::HotkeyHelp => {
                self.open_read_only_inline_widget(InlineWidgetKind::HotkeyHelp);
                KeyOutcome::Redraw
            }
            KeyInput::OpenModelPicker => self.open_model_picker(),
            KeyInput::SpawnPanel => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::SpawnSession
            }
            KeyInput::SpawnSelfDevSession => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::SpawnSelfDevSession
            }
            KeyInput::SpawnHomeSession => {
                self.capture_inline_widget_exit();
                self.session_switcher.close();
                KeyOutcome::SpawnHomeSession
            }
            _ => KeyOutcome::None,
        }
    }

    pub(crate) fn apply_session_switcher_cards(&mut self, cards: Vec<workspace::SessionCard>) {
        let current_session_id = self.current_session_id().map(str::to_string);
        self.session_switcher
            .apply_sessions(cards, current_session_id.as_deref());
        if self.session_switcher.open {
            self.set_status(SingleSessionStatus::Info(format!(
                "{} recent session(s)",
                self.session_switcher.sessions.len()
            )));
        }
    }

    fn resume_selected_switcher_session(&mut self) -> KeyOutcome {
        if self.is_processing {
            self.set_status(SingleSessionStatus::Info(
                "finish or Esc interrupt the running generation before switching sessions"
                    .to_string(),
            ));
            return KeyOutcome::Redraw;
        }

        let Some(session) = self.session_switcher.selected_session() else {
            return KeyOutcome::None;
        };
        let title = session.title.clone();
        let session_id = session.session_id.clone();
        self.session = Some(session);
        self.live_session_id = self
            .session
            .as_ref()
            .map(|session| session.session_id.clone());
        self.detail_scroll = 0;
        self.messages.clear();
        self.streaming_response.clear();
        self.error = None;
        self.stdin_response = None;
        self.body_scroll_lines = 0.0;
        self.show_help = false;
        self.welcome.timeline = false;
        self.session_switcher.close();
        // Card previews (if any) are applied synchronously above via
        // replace-session state; the full transcript can be large, so defer
        // the disk parse to the event loop instead of blocking this key.
        self.pending_transcript_hydration = Some(session_id.clone());
        self.set_status(SingleSessionStatus::Info(format!("resumed {title}")));
        KeyOutcome::Redraw
    }

    /// Take the session id queued for off-thread transcript hydration.
    pub(crate) fn take_pending_transcript_hydration(&mut self) -> Option<String> {
        self.pending_transcript_hydration.take()
    }

    /// Queue a transcript hydration to be serviced off the UI thread.
    pub(crate) fn request_transcript_hydration(&mut self, session_id: &str) {
        self.pending_transcript_hydration = Some(session_id.to_string());
    }

    /// Apply a transcript loaded off the UI thread, if it still matches the
    /// live session. Returns true when the transcript was applied.
    pub(crate) fn apply_hydrated_transcript(
        &mut self,
        session_id: &str,
        result: Result<Option<Vec<SessionTranscriptMessage>>, String>,
    ) -> bool {
        if self.live_session_id.as_deref() != Some(session_id) {
            return false;
        }
        match result {
            Ok(Some(messages)) if !messages.is_empty() => {
                self.apply_resumed_session_transcript(messages);
                true
            }
            Ok(_) => false,
            Err(error) => {
                crate::desktop_log::warn(format_args!(
                    "jcode-desktop: failed to hydrate resumed transcript for {session_id}: {error}"
                ));
                self.error = Some(format!("failed to load transcript: {error}"));
                false
            }
        }
    }

    fn handle_stdin_response_key(&mut self, key: KeyInput) -> KeyOutcome {
        match key {
            KeyInput::SubmitDraft | KeyInput::QueueDraft => {
                let Some(state) = self.stdin_response.take() else {
                    return KeyOutcome::None;
                };
                self.set_status(SingleSessionStatus::SendingInteractiveInput);
                KeyOutcome::SendStdinResponse {
                    request_id: state.request_id,
                    input: state.input,
                }
            }
            KeyInput::Enter => {
                if let Some(state) = &mut self.stdin_response {
                    state.input.push('\n');
                }
                KeyOutcome::Redraw
            }
            KeyInput::Backspace => {
                if let Some(state) = &mut self.stdin_response {
                    state.input.pop();
                }
                KeyOutcome::Redraw
            }
            KeyInput::DeleteToLineStart => {
                if let Some(state) = &mut self.stdin_response {
                    state.input.clear();
                }
                KeyOutcome::Redraw
            }
            KeyInput::PasteText => KeyOutcome::PasteText,
            KeyInput::Character(text) => {
                if let Some(state) = &mut self.stdin_response {
                    state.input.push_str(&text);
                }
                KeyOutcome::Redraw
            }
            KeyInput::CancelGeneration => KeyOutcome::CancelGeneration,
            KeyInput::Escape => {
                self.set_status(SingleSessionStatus::InteractiveInputPending);
                KeyOutcome::Redraw
            }
            _ => KeyOutcome::None,
        }
    }

    pub(crate) fn inline_widget_styled_lines(&self) -> Vec<SingleSessionStyledLine> {
        match self.active_inline_widget() {
            Some(InlineWidgetKind::HotkeyHelp) => hotkey_help_inline_widget().styled_lines(),
            Some(InlineWidgetKind::ModelPicker) => {
                model_picker_inline_styled_lines(&self.model_picker)
            }
            Some(InlineWidgetKind::SessionSwitcher) => {
                session_switcher_styled_lines(&self.session_switcher, self.current_session_id())
            }
            Some(InlineWidgetKind::SessionInfo) => session_info_inline_styled_lines(self),
            Some(InlineWidgetKind::SlashSuggestions) => self.slash_suggestion_styled_lines(),
            None => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn inline_widget_line_count(&self) -> usize {
        self.inline_widget_styled_lines().len()
    }

    #[cfg(test)]
    pub(crate) fn inline_widget_visible_line_count(&self) -> usize {
        let line_count = self.inline_widget_line_count();
        let limit = self
            .active_inline_widget()
            .map(InlineWidgetKind::visible_line_limit)
            .unwrap_or(INLINE_WIDGET_DEFAULT_VISIBLE_LINE_LIMIT);
        line_count.min(limit)
    }

    fn slash_suggestions_visible(&self) -> bool {
        !self.slash_suggestion_candidates().is_empty()
    }

    fn slash_suggestion_styled_lines(&self) -> Vec<SingleSessionStyledLine> {
        let candidates = self.slash_suggestion_candidates();
        if candidates.is_empty() {
            return Vec::new();
        }

        let mut lines = vec![styled_line(
            "slash command suggestions",
            SingleSessionLineStyle::OverlayTitle,
        )];
        let selected = self
            .slash_suggestions
            .selected
            .min(candidates.len().saturating_sub(1));
        let usage_width = candidates
            .iter()
            .map(|(usage, _)| usage.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(10, 20);
        lines.extend(
            candidates
                .into_iter()
                .enumerate()
                .map(|(index, (usage, description))| {
                    let style = if index == selected {
                        SingleSessionLineStyle::OverlaySelection
                    } else {
                        SingleSessionLineStyle::Overlay
                    };
                    styled_line(
                        format!(" {:<width$}  {}", usage, description, width = usage_width),
                        style,
                    )
                }),
        );
        lines
    }

    fn slash_suggestion_line_count(&self) -> usize {
        let candidate_count = self.slash_suggestion_candidate_count();
        if candidate_count == 0 {
            0
        } else {
            1 + candidate_count
        }
    }

    fn slash_suggestion_candidate_count(&self) -> usize {
        self.slash_suggestion_candidates().len()
    }

    fn slash_suggestion_candidates(&self) -> Vec<(&'static str, &'static str)> {
        if self
            .slash_suggestions
            .dismissed_for_draft
            .as_deref()
            .is_some_and(|draft| draft == self.draft)
        {
            return Vec::new();
        }
        let cursor = self.draft_cursor.min(self.draft.len());
        if !self.draft.is_char_boundary(cursor) {
            return Vec::new();
        }
        let prefix = self.draft[..cursor].trim_start();
        if !prefix.starts_with('/') || prefix.contains(char::is_whitespace) {
            return Vec::new();
        }
        let prefix = if self.slash_suggestions.query.is_empty() {
            prefix
        } else {
            self.slash_suggestions.query.as_str()
        };
        let prefix = prefix.to_ascii_lowercase();

        let mut prefix_matches = Vec::new();
        let mut fuzzy_matches: Vec<(usize, usize, &'static str, &'static str)> = Vec::new();
        for (usage, description) in DESKTOP_SLASH_COMMANDS.iter().copied() {
            let command = usage.split_whitespace().next().unwrap_or(usage);
            let command_lower = command.to_ascii_lowercase();
            if command_lower.starts_with(&prefix) {
                prefix_matches.push((usage, description));
            } else if let Some(score) = desktop_slash_fuzzy_score(&prefix, &command_lower) {
                fuzzy_matches.push((score, command.len(), usage, description));
            }
        }

        fuzzy_matches.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(b.2))
        });

        prefix_matches
            .into_iter()
            .chain(
                fuzzy_matches
                    .into_iter()
                    .map(|(_, _, usage, description)| (usage, description)),
            )
            .take(DESKTOP_SLASH_SUGGESTION_ROW_LIMIT)
            .collect()
    }

    pub(crate) fn active_inline_widget(&self) -> Option<InlineWidgetKind> {
        match self.active_overlay_state() {
            SingleSessionOverlay::Inline { kind, .. } => Some(kind),
            SingleSessionOverlay::None | SingleSessionOverlay::StdinResponse => None,
        }
    }

    pub(crate) fn active_inline_widget_mode(&self) -> Option<InlineWidgetMode> {
        match self.active_overlay_state() {
            SingleSessionOverlay::Inline { mode, .. } => Some(mode),
            SingleSessionOverlay::None | SingleSessionOverlay::StdinResponse => None,
        }
    }

    pub(crate) fn active_overlay_state(&self) -> SingleSessionOverlay {
        if self.stdin_response.is_some() {
            return SingleSessionOverlay::StdinResponse;
        }
        if self.session_switcher.open {
            return SingleSessionOverlay::Inline {
                kind: InlineWidgetKind::SessionSwitcher,
                mode: InlineWidgetKind::SessionSwitcher.mode(self),
            };
        }
        if self.model_picker.open {
            return SingleSessionOverlay::Inline {
                kind: InlineWidgetKind::ModelPicker,
                mode: InlineWidgetKind::ModelPicker.mode(self),
            };
        }
        if self.show_help {
            return SingleSessionOverlay::Inline {
                kind: InlineWidgetKind::HotkeyHelp,
                mode: InlineWidgetMode::ReadOnly,
            };
        }
        if self.show_session_info {
            return SingleSessionOverlay::Inline {
                kind: InlineWidgetKind::SessionInfo,
                mode: InlineWidgetMode::ReadOnly,
            };
        }
        if self.slash_suggestions_visible() {
            return SingleSessionOverlay::Inline {
                kind: InlineWidgetKind::SlashSuggestions,
                mode: InlineWidgetMode::ReadOnly,
            };
        }
        SingleSessionOverlay::None
    }

    #[cfg(test)]
    pub(crate) fn active_inline_widget_uses_card_chrome(&self) -> bool {
        self.active_inline_widget().is_some()
    }

    pub(crate) fn should_draw_composer_caret(&self) -> bool {
        !self.active_overlay_state().blocks_composer_caret()
    }

    pub(crate) fn welcome_hero_text(&self) -> String {
        handwritten_welcome_phrase(self.welcome.hero_phrase_index).to_string()
    }

    pub(crate) fn welcome_continuation_suggestion(&self) -> Option<&str> {
        self.welcome.continuation_suggestion.as_deref()
    }

    pub(crate) fn is_welcome_timeline_visible(&self) -> bool {
        self.welcome.timeline
            && !self.show_help
            && !self.show_session_info
            && !self.session_switcher.open
            && self.stdin_response.is_none()
    }

    pub(crate) fn has_welcome_timeline_transcript(&self) -> bool {
        !self.messages.is_empty() || !self.streaming_response.is_empty() || self.error.is_some()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_fresh_welcome_visible(&self) -> bool {
        self.session.is_none()
            && self.live_session_id.is_none()
            && self.messages.is_empty()
            && self.streaming_response.is_empty()
            && self.status.is_none()
            && self.error.is_none()
            && self.pending_images.is_empty()
            && !self.show_help
            && !self.model_picker.open
            && !self.session_switcher.open
            && self.stdin_response.is_none()
    }

    pub(crate) fn draft_cursor_line_col(&self) -> (usize, usize) {
        let before_cursor = &self.draft[..self.draft_cursor.min(self.draft.len())];
        let line = before_cursor.chars().filter(|ch| *ch == '\n').count();
        let column = before_cursor
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count();
        (line, column)
    }

    pub(crate) fn draft_cursor_line_byte_index(&self) -> (usize, usize) {
        let cursor = self.draft_cursor.min(self.draft.len());
        let line = self.draft[..cursor]
            .chars()
            .filter(|ch| *ch == '\n')
            .count();
        let line_start = line_start(&self.draft, cursor);
        (line, cursor - line_start)
    }

    pub(crate) fn composer_cursor_line_byte_index(&self) -> (usize, usize) {
        let (line, index) = self.draft_cursor_line_byte_index();
        if line == 0 {
            (line, self.composer_prompt().len() + index)
        } else {
            (line, index)
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_draft_cursor_line_col(&mut self, target_line: usize, target_col: usize) {
        self.draft_cursor = self.draft_byte_index_for_line_col(target_line, target_col);
        self.clamp_draft_cursor();
        self.clear_selection();
        self.clear_draft_selection();
    }

    fn draft_byte_index_for_line_col(&self, target_line: usize, target_col: usize) -> usize {
        let mut line = 0usize;
        let mut line_start = 0usize;
        for (index, ch) in self.draft.char_indices() {
            if line == target_line {
                break;
            }
            if ch == '\n' {
                line += 1;
                line_start = index + ch.len_utf8();
            }
        }

        if line < target_line {
            return self.draft.len();
        }

        let line_end = line_end(&self.draft, line_start);
        self.draft[line_start..line_end]
            .char_indices()
            .map(|(offset, _)| line_start + offset)
            .chain(std::iter::once(line_end))
            .nth(target_col)
            .unwrap_or(line_end)
    }

    fn submit_draft(&mut self) -> KeyOutcome {
        let message = self.draft.trim().to_string();
        if message.is_empty() && self.pending_images.is_empty() {
            return KeyOutcome::None;
        }
        if self.pending_images.is_empty()
            && let Some(outcome) = self.handle_slash_command(&message)
        {
            return outcome;
        }
        let images = std::mem::take(&mut self.pending_images);
        self.record_user_submit(&message, &images);
        let Some(session) = &self.session else {
            return KeyOutcome::StartFreshSession { message, images };
        };
        let session_id = session.session_id.clone();
        let title = session.title.clone();
        KeyOutcome::SendDraft {
            session_id,
            title,
            message,
            images,
        }
    }

    fn handle_slash_command(&mut self, message: &str) -> Option<KeyOutcome> {
        if !message.starts_with('/') {
            return None;
        }

        let mut parts = message.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default();
        let args = parts.next().unwrap_or_default().trim();

        if self.active_inline_widget() == Some(InlineWidgetKind::SlashSuggestions) {
            self.capture_inline_widget_exit();
        }

        let outcome = match command {
            "/help" | "/?" | "/commands" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.show_help = true;
                self.model_picker.close();
                self.session_switcher.close();
                self.mark_inline_widget_opened();
                self.set_status(SingleSessionStatus::Info(
                    "showing desktop slash commands".to_string(),
                ));
                self.scroll_body_to_bottom();
                KeyOutcome::Redraw
            }
            "/clear" => {
                self.messages.clear();
                self.streaming_response.clear();
                self.error = None;
                self.is_processing = false;
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info("session cleared".to_string()));
                self.scroll_body_to_bottom();
                if self.session.is_some() || self.live_session_id.is_some() {
                    KeyOutcome::ClearServerSession
                } else {
                    KeyOutcome::Redraw
                }
            }
            "/new" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                KeyOutcome::SpawnSession
            }
            "/issues" => {
                if matches!(args, "refresh" | "sync") {
                    return Some(self.toggle_issue_browser(Some(true)));
                }
                if args == "preview" {
                    let outcome = self.toggle_issue_browser(Some(true));
                    self.side_panel.focus = DesktopSidePanelFocus::IssuePreview;
                    return Some(outcome);
                }
                let visible = match args {
                    "on" | "open" | "show" => Some(true),
                    "off" | "close" | "hide" => Some(false),
                    _ => None,
                };
                self.toggle_issue_browser(visible)
            }
            "/sessions" | "/session" | "/resume" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                return Some(self.open_session_switcher());
            }
            "/model" | "/models" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if args.is_empty() {
                    return Some(self.open_model_picker());
                }
                KeyOutcome::SetModel(args.to_string())
            }
            "/refresh-model-list" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.model_picker.open_loading();
                self.set_status(SingleSessionStatus::Info(
                    "refreshing model list".to_string(),
                ));
                KeyOutcome::RefreshModelCatalog
            }
            "/reload" | "/force-reload" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "force reloading desktop".to_string(),
                ));
                KeyOutcome::ForceReload
            }
            "/effort" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if args.is_empty() || args == "status" {
                    let current = self
                        .runtime_settings
                        .reasoning_effort
                        .as_deref()
                        .unwrap_or("default");
                    self.set_status(SingleSessionStatus::Info(format!(
                        "effort: {current} · use /effort <none|low|medium|high|xhigh|max>"
                    )));
                    KeyOutcome::Redraw
                } else if matches!(args, "none" | "low" | "medium" | "high" | "xhigh" | "max") {
                    KeyOutcome::SetReasoningEffort(args.to_string())
                } else {
                    self.set_status(SingleSessionStatus::Info(
                        "usage: /effort <none|low|medium|high|xhigh|max>".to_string(),
                    ));
                    KeyOutcome::Redraw
                }
            }
            "/font" | "/fonts" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                let mut args = args.split_whitespace();
                match (args.next(), args.collect::<Vec<_>>().join(" ")) {
                    (None, _) | (Some("status"), _) => {
                        let options = SINGLE_SESSION_HANDWRITING_FONT_FAMILIES.join(", ");
                        self.set_status(SingleSessionStatus::Info(format!(
                            "fonts: user={} · ai={} · options: default, {options}",
                            single_session_user_font_family(),
                            single_session_assistant_font_family()
                        )));
                        KeyOutcome::Redraw
                    }
                    (Some("user"), value) if !value.is_empty() => {
                        if let Some(family) = set_single_session_user_font_family(&value) {
                            self.set_status(SingleSessionStatus::Info(format!(
                                "user font set to {family}"
                            )));
                        } else {
                            self.set_status(SingleSessionStatus::Info(
                                "unknown font · try /font status".to_string(),
                            ));
                        }
                        KeyOutcome::Redraw
                    }
                    (Some("ai" | "assistant"), value) if !value.is_empty() => {
                        if let Some(family) = set_single_session_assistant_font_family(&value) {
                            self.set_status(SingleSessionStatus::Info(format!(
                                "AI font set to {family}"
                            )));
                        } else {
                            self.set_status(SingleSessionStatus::Info(
                                "unknown font · try /font status".to_string(),
                            ));
                        }
                        KeyOutcome::Redraw
                    }
                    _ => {
                        self.set_status(SingleSessionStatus::Info(
                            "usage: /font [status|user <name>|ai <name>]".to_string(),
                        ));
                        KeyOutcome::Redraw
                    }
                }
            }
            "/fast" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                match args {
                    "" | "status" => {
                        let current = self
                            .runtime_settings
                            .service_tier
                            .as_deref()
                            .unwrap_or("standard");
                        self.set_status(SingleSessionStatus::Info(format!(
                            "fast mode: {current} · use /fast <on|off|status>"
                        )));
                        KeyOutcome::Redraw
                    }
                    "on" => KeyOutcome::SetServiceTier("priority".to_string()),
                    "off" => KeyOutcome::SetServiceTier("off".to_string()),
                    _ => {
                        self.set_status(SingleSessionStatus::Info(
                            "usage: /fast [on|off|status]".to_string(),
                        ));
                        KeyOutcome::Redraw
                    }
                }
            }
            "/transport" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                match args {
                    "" | "status" => {
                        let current = self
                            .runtime_settings
                            .transport
                            .as_deref()
                            .unwrap_or("unknown");
                        self.set_status(SingleSessionStatus::Info(format!(
                            "transport: {current} · use /transport <auto|https|websocket>"
                        )));
                        KeyOutcome::Redraw
                    }
                    "auto" | "https" | "websocket" => KeyOutcome::SetTransport(args.to_string()),
                    _ => {
                        self.set_status(SingleSessionStatus::Info(
                            "usage: /transport <auto|https|websocket>".to_string(),
                        ));
                        KeyOutcome::Redraw
                    }
                }
            }
            "/compact" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if args.is_empty() {
                    KeyOutcome::CompactSession
                } else if args == "mode" || args == "mode status" {
                    let current = self
                        .runtime_settings
                        .compaction_mode
                        .as_deref()
                        .unwrap_or("reactive");
                    self.set_status(SingleSessionStatus::Info(format!(
                        "compaction: {current} · use /compact mode <reactive|proactive|semantic>"
                    )));
                    KeyOutcome::Redraw
                } else if let Some(mode) = args.strip_prefix("mode ") {
                    let mode = mode.trim();
                    if matches!(mode, "reactive" | "proactive" | "semantic") {
                        KeyOutcome::SetCompactionMode(mode.to_string())
                    } else {
                        self.set_status(SingleSessionStatus::Info(
                            "usage: /compact mode <reactive|proactive|semantic>".to_string(),
                        ));
                        KeyOutcome::Redraw
                    }
                } else {
                    self.set_status(SingleSessionStatus::Info(
                        "usage: /compact [mode <reactive|proactive|semantic>]".to_string(),
                    ));
                    KeyOutcome::Redraw
                }
            }
            "/commit" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                let message = desktop_commit_prompt();
                let Some(session) = &self.session else {
                    return Some(KeyOutcome::StartFreshSession {
                        message,
                        images: Vec::new(),
                    });
                };
                let session_id = session.session_id.clone();
                let title = session.title.clone();
                self.set_status(SingleSessionStatus::Info(
                    "starting logical commits".to_string(),
                ));
                return Some(KeyOutcome::SendDraft {
                    session_id,
                    title,
                    message,
                    images: Vec::new(),
                });
            }
            "/rename" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if args.is_empty() {
                    self.set_status(SingleSessionStatus::Info(
                        "usage: /rename <session name> or /rename --clear".to_string(),
                    ));
                    KeyOutcome::Redraw
                } else if args == "--clear" {
                    KeyOutcome::RenameSession(None)
                } else {
                    KeyOutcome::RenameSession(Some(args.to_string()))
                }
            }
            "/usage" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                let usage = self.runtime_settings.token_usage.as_ref();
                let message = usage
                    .map(|usage| {
                        format!(
                            "desktop /usage overlay is not implemented yet · latest tokens: input={} output={}",
                            usage.input, usage.output
                        )
                    })
                    .unwrap_or_else(|| {
                        "desktop /usage overlay is not implemented yet · no token usage received for this session".to_string()
                    });
                self.set_status(SingleSessionStatus::Info(message));
                KeyOutcome::Redraw
            }
            "/todo" | "/todos" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "desktop todo panel is not implemented yet · todo tool output is shown in transcript".to_string(),
                ));
                KeyOutcome::Redraw
            }
            "/memory" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "desktop memory panel is not implemented yet · memory server events are not surfaced".to_string(),
                ));
                KeyOutcome::Redraw
            }
            "/changelog" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "desktop changelog overlay is not implemented yet".to_string(),
                ));
                KeyOutcome::Redraw
            }
            "/diff" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "desktop diff viewer is not implemented yet".to_string(),
                ));
                KeyOutcome::Redraw
            }
            "/account" | "/auth" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(
                    "desktop account picker is not implemented yet · use the TUI for account management".to_string(),
                ));
                KeyOutcome::Redraw
            }
            "/swarm" | "/bg" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.set_status(SingleSessionStatus::Info(format!(
                    "desktop {command} panel is not implemented yet · related tool output is shown in transcript"
                )));
                KeyOutcome::Redraw
            }
            "/copy" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                return Some(match args {
                    "" | "latest" | "response" => self
                        .latest_assistant_response()
                        .map(KeyOutcome::CopyLatestResponse)
                        .unwrap_or_else(|| {
                            self.set_status(SingleSessionStatus::Info(
                                "no assistant response to copy".to_string(),
                            ));
                            KeyOutcome::Redraw
                        }),
                    "code" | "codeblock" | "code-block" => self
                        .latest_rich_code_block_text()
                        .map(|text| KeyOutcome::CopyText {
                            text,
                            success_notice: "copied latest code block",
                        })
                        .unwrap_or_else(|| {
                            self.set_status(SingleSessionStatus::Info(
                                "no code block to copy".to_string(),
                            ));
                            KeyOutcome::Redraw
                        }),
                    "transcript" | "all" => self
                        .copy_rich_transcript_text(
                            desktop_rich_text::TranscriptCopyMode::TranscriptPlainText,
                        )
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| KeyOutcome::CopyText {
                            text,
                            success_notice: "copied transcript",
                        })
                        .unwrap_or_else(|| {
                            self.set_status(SingleSessionStatus::Info(
                                "no transcript to copy".to_string(),
                            ));
                            KeyOutcome::Redraw
                        }),
                    _ => {
                        self.set_status(SingleSessionStatus::Info(
                            "usage: /copy [latest|code|transcript]".to_string(),
                        ));
                        KeyOutcome::Redraw
                    }
                });
            }
            "/search" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if args.is_empty() {
                    self.set_status(SingleSessionStatus::Info(
                        "usage: /search <query>".to_string(),
                    ));
                    KeyOutcome::Redraw
                } else {
                    let matches = self.search_rich_transcript(args);
                    if let Some(first) = matches.first() {
                        let body_len = self.body_lines().len();
                        self.body_scroll_lines =
                            body_len.saturating_sub(first.line_index + 1) as f32;
                    }
                    self.set_status(SingleSessionStatus::Info(format!(
                        "{} match(es) for \"{}\"",
                        matches.len(),
                        args
                    )));
                    KeyOutcome::Redraw
                }
            }
            "/stop" | "/cancel" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                if self.is_processing {
                    KeyOutcome::CancelGeneration
                } else {
                    self.set_status(SingleSessionStatus::Info("nothing is running".to_string()));
                    KeyOutcome::Redraw
                }
            }
            "/status" | "/info" => {
                self.draft.clear();
                self.draft_cursor = 0;
                self.composer.input_undo_stack.clear();
                self.show_help = false;
                self.show_session_info = true;
                self.model_picker.close();
                self.session_switcher.close();
                self.mark_inline_widget_opened();
                self.set_status(SingleSessionStatus::Info(
                    "showing session info".to_string(),
                ));
                self.scroll_body_to_bottom();
                KeyOutcome::Redraw
            }
            "/quit" | "/exit" => KeyOutcome::Exit,
            _ => {
                self.set_status(SingleSessionStatus::Info(format!(
                    "unknown desktop slash command: {command} · try /help"
                )));
                KeyOutcome::Redraw
            }
        };

        Some(outcome)
    }

    pub(crate) fn attach_image(&mut self, media_type: String, base64_data: String) {
        self.pending_images.push((media_type, base64_data));
        self.set_status(SingleSessionStatus::AttachedImages(
            self.pending_images.len(),
        ));
    }

    pub(crate) fn clear_attached_images(&mut self) -> bool {
        if self.pending_images.is_empty() {
            return false;
        }
        self.pending_images.clear();
        self.set_status(SingleSessionStatus::Info(
            "cleared image attachments".to_string(),
        ));
        true
    }

    pub(crate) fn accepts_clipboard_image_paste(&self) -> bool {
        self.stdin_response.is_none() && !self.model_picker.open && !self.session_switcher.open
    }

    pub(crate) fn paste_text(&mut self, text: &str) {
        if !text.is_empty() {
            if let Some(stdin_response) = &mut self.stdin_response {
                stdin_response.input.push_str(text);
                return;
            }
            self.insert_draft_text(text);
        }
    }

    pub(crate) fn send_stdin_response(
        &mut self,
        request_id: String,
        input: String,
    ) -> anyhow::Result<()> {
        let Some(handle) = &self.runtime.session_handle else {
            anyhow::bail!("no active desktop session to receive interactive input");
        };
        handle.send_stdin_response(request_id, input)?;
        self.clear_tool_stdin_prompts();
        self.set_status(SingleSessionStatus::Info(
            "interactive input sent".to_string(),
        ));
        Ok(())
    }

    pub(crate) fn set_reasoning_effort_via_active_session(
        &mut self,
        effort: String,
    ) -> anyhow::Result<()> {
        let Some(handle) = &self.runtime.session_handle else {
            anyhow::bail!("no active desktop session to receive reasoning effort change");
        };
        handle.set_reasoning_effort(effort)
    }

    fn queue_draft(&mut self) -> KeyOutcome {
        let message = self.draft.trim().to_string();
        if message.is_empty() && self.pending_images.is_empty() {
            return KeyOutcome::None;
        }
        let images = std::mem::take(&mut self.pending_images);
        self.composer.queued_drafts.push((message.clone(), images));
        self.messages.push(SingleSessionMessage::meta(format!(
            "queued prompt: {message}"
        )));
        self.draft.clear();
        self.draft_cursor = 0;
        self.composer.input_undo_stack.clear();
        self.set_status(SingleSessionStatus::Info(format!(
            "{} prompt(s) queued",
            self.composer.queued_drafts.len()
        )));
        KeyOutcome::Redraw
    }

    fn retrieve_queued_draft_for_edit(&mut self) -> KeyOutcome {
        let Some((message, images)) = self.composer.queued_drafts.pop() else {
            return KeyOutcome::None;
        };
        self.remember_input_undo_state();
        self.draft = message;
        self.draft_cursor = self.draft.len();
        self.pending_images = images;
        self.set_status(SingleSessionStatus::Info(format!(
            "{} prompt(s) queued",
            self.composer.queued_drafts.len()
        )));
        KeyOutcome::Redraw
    }

    fn cut_input_line(&mut self) -> KeyOutcome {
        if self.draft.is_empty() {
            return KeyOutcome::None;
        }
        self.remember_input_undo_state();
        let text = std::mem::take(&mut self.draft);
        self.draft_cursor = 0;
        self.set_status(SingleSessionStatus::Info("cut input line".to_string()));
        KeyOutcome::CutDraftToClipboard(text)
    }

    pub(crate) fn take_next_queued_draft(&mut self) -> Option<(String, Vec<(String, String)>)> {
        if self.is_processing || self.error.is_some() || self.composer.queued_drafts.is_empty() {
            return None;
        }
        let (message, images) = self.composer.queued_drafts.remove(0);
        self.record_user_submit(&message, &images);
        Some((message, images))
    }

    pub(crate) fn begin_selection(&mut self, point: SelectionPoint) {
        self.selection.anchor = Some(point);
        self.selection.focus = Some(point);
    }

    pub(crate) fn update_selection(&mut self, point: SelectionPoint) {
        if self.selection.anchor.is_some() {
            self.selection.focus = Some(point);
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection.anchor = None;
        self.selection.focus = None;
    }

    pub(crate) fn begin_draft_selection(&mut self, point: SelectionPoint) {
        self.clear_selection();
        self.selection.draft_anchor = Some(point);
        self.selection.draft_focus = Some(point);
        self.draft_cursor = self.draft_byte_index_for_line_col(point.line, point.column);
        self.clamp_draft_cursor();
    }

    pub(crate) fn update_draft_selection(&mut self, point: SelectionPoint) {
        if self.selection.draft_anchor.is_some() {
            self.selection.draft_focus = Some(point);
            self.draft_cursor = self.draft_byte_index_for_line_col(point.line, point.column);
            self.clamp_draft_cursor();
        }
    }

    pub(crate) fn clear_draft_selection(&mut self) {
        self.selection.draft_anchor = None;
        self.selection.draft_focus = None;
    }

    pub(crate) fn draft_selection_points(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.selection.draft_anchor?;
        let focus = self.selection.draft_focus?;
        if selection_point_cmp(anchor, focus).is_gt() {
            Some((focus, anchor))
        } else {
            Some((anchor, focus))
        }
    }

    pub(crate) fn draft_selection_segments(&self) -> Vec<SelectionLineSegment> {
        let lines: Vec<String> = self.draft.split('\n').map(ToString::to_string).collect();
        let Some((start, end)) = self.draft_selection_points() else {
            return Vec::new();
        };
        if start == end || start.line >= lines.len() {
            return Vec::new();
        }
        let end_line = end.line.min(lines.len().saturating_sub(1));
        let mut segments = Vec::new();
        for (line_index, line) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
            let line_len = line.chars().count();
            let prompt_columns = if line_index == 0 {
                self.composer_prompt().chars().count()
            } else {
                0
            };
            let start_column = if line_index == start.line {
                start.column.min(line_len)
            } else {
                0
            };
            let end_column = if line_index == end_line {
                end.column.min(line_len)
            } else {
                line_len
            };
            if start_column != end_column || (start.line != end.line && line_len == 0) {
                segments.push(SelectionLineSegment {
                    line: line_index,
                    start_column: start_column + prompt_columns,
                    end_column: end_column + prompt_columns,
                });
            }
        }
        segments
    }

    pub(crate) fn selected_draft_text(&mut self) -> Option<String> {
        let (start, end) = self.draft_selection_points()?;
        if start == end {
            self.clear_draft_selection();
            return None;
        }
        let start_index = self.draft_byte_index_for_line_col(start.line, start.column);
        let end_index = self.draft_byte_index_for_line_col(end.line, end.column);
        let (start_index, end_index) = if start_index <= end_index {
            (start_index, end_index)
        } else {
            (end_index, start_index)
        };
        let selected = self.draft.get(start_index..end_index).map(str::to_string);
        self.clear_draft_selection();
        selected.filter(|text| !text.is_empty())
    }

    fn draft_selection_range(&self) -> Option<(usize, usize)> {
        let (start, end) = self.draft_selection_points()?;
        if start == end {
            return None;
        }
        let start_index = self.draft_byte_index_for_line_col(start.line, start.column);
        let end_index = self.draft_byte_index_for_line_col(end.line, end.column);
        if start_index <= end_index {
            Some((start_index, end_index)).filter(|(start, end)| start != end)
        } else {
            Some((end_index, start_index)).filter(|(start, end)| start != end)
        }
    }

    fn replace_draft_selection_with(&mut self, text: &str) -> bool {
        let Some((start, end)) = self.draft_selection_range() else {
            return false;
        };
        self.remember_input_undo_state();
        self.draft.replace_range(start..end, text);
        self.draft_cursor = start + text.len();
        self.clear_draft_selection();
        true
    }

    fn delete_draft_selection(&mut self) -> bool {
        self.replace_draft_selection_with("")
    }

    pub(crate) fn selection_points(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.selection.anchor?;
        let focus = self.selection.focus?;
        if selection_point_cmp(anchor, focus).is_gt() {
            Some((focus, anchor))
        } else {
            Some((anchor, focus))
        }
    }

    pub(crate) fn selection_segments(&self, lines: &[String]) -> Vec<SelectionLineSegment> {
        let Some((start, end)) = self.selection_points() else {
            return Vec::new();
        };
        if start == end || start.line >= lines.len() {
            return Vec::new();
        }

        let end_line = end.line.min(lines.len().saturating_sub(1));
        let mut segments = Vec::new();
        for (line_index, line) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
            let line_len = line.chars().count();
            let start_column = if line_index == start.line {
                start.column.min(line_len)
            } else {
                0
            };
            let end_column = if line_index == end_line {
                end.column.min(line_len)
            } else {
                line_len
            };
            if start_column != end_column || (start.line != end.line && line_len == 0) {
                segments.push(SelectionLineSegment {
                    line: line_index,
                    start_column,
                    end_column,
                });
            }
        }
        segments
    }

    pub(crate) fn has_body_selection(&self) -> bool {
        self.selection.anchor.is_some() && self.selection.focus.is_some()
    }

    pub(crate) fn has_draft_selection(&self) -> bool {
        self.selection.draft_anchor.is_some() && self.selection.draft_focus.is_some()
    }

    pub(crate) fn selected_text_from_lines(&self, lines: &[String]) -> Option<String> {
        let (start, end) = self.selection_points()?;
        if start == end || start.line >= lines.len() {
            return None;
        }
        let end_line = end.line.min(lines.len().saturating_sub(1));
        let mut selected = Vec::new();
        for (line_index, line) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
            let line_len = line.chars().count();
            let start_column = if line_index == start.line {
                start.column.min(line_len)
            } else {
                0
            };
            let end_column = if line_index == end_line {
                end.column.min(line_len)
            } else {
                line_len
            };
            selected.push(slice_by_char_columns(line, start_column, end_column));
        }
        let text = selected.join("\n");
        (!text.is_empty()).then_some(text)
    }

    fn insert_draft_text(&mut self, text: &str) {
        if self.replace_draft_selection_with(text) {
            return;
        }
        if !text.is_empty() {
            self.remember_input_undo_state();
        }
        self.clamp_draft_cursor();
        self.draft.insert_str(self.draft_cursor, text);
        self.draft_cursor += text.len();
    }

    fn delete_previous_char(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        if self.draft_cursor == 0 {
            return;
        }
        self.remember_input_undo_state();
        let previous = previous_char_boundary(&self.draft, self.draft_cursor);
        self.draft.replace_range(previous..self.draft_cursor, "");
        self.draft_cursor = previous;
    }

    fn delete_next_char(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        if self.draft_cursor >= self.draft.len() {
            return;
        }
        self.remember_input_undo_state();
        let next = next_char_boundary(&self.draft, self.draft_cursor);
        self.draft.replace_range(self.draft_cursor..next, "");
    }

    fn delete_previous_word(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        let start = previous_word_start(&self.draft, self.draft_cursor);
        if start < self.draft_cursor {
            self.remember_input_undo_state();
        }
        self.draft.replace_range(start..self.draft_cursor, "");
        self.draft_cursor = start;
    }

    fn delete_next_word(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        let end = next_word_end(&self.draft, self.draft_cursor);
        if end > self.draft_cursor {
            self.remember_input_undo_state();
        }
        self.draft.replace_range(self.draft_cursor..end, "");
    }

    fn move_cursor_left(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = previous_char_boundary(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn move_cursor_right(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = next_char_boundary(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn move_cursor_word_left(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = previous_word_start(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn move_cursor_word_right(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = next_word_end(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn move_to_line_start(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = line_start(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn move_to_line_end(&mut self) {
        self.clamp_draft_cursor();
        self.draft_cursor = line_end(&self.draft, self.draft_cursor);
        self.clear_draft_selection();
    }

    fn delete_to_line_start(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        let start = line_start(&self.draft, self.draft_cursor);
        if start < self.draft_cursor {
            self.remember_input_undo_state();
        }
        self.draft.replace_range(start..self.draft_cursor, "");
        self.draft_cursor = start;
    }

    fn delete_to_line_end(&mut self) {
        if self.delete_draft_selection() {
            return;
        }
        self.clamp_draft_cursor();
        let end = line_end(&self.draft, self.draft_cursor);
        if end > self.draft_cursor {
            self.remember_input_undo_state();
        }
        self.draft.replace_range(self.draft_cursor..end, "");
    }

    fn clear_draft_for_escape(&mut self) -> KeyOutcome {
        if self.draft.is_empty() {
            return KeyOutcome::None;
        }
        self.remember_input_undo_state();
        self.draft.clear();
        self.draft_cursor = 0;
        self.clear_draft_selection();
        if self.model_picker.open && self.model_picker.preview {
            self.capture_inline_widget_exit();
            self.model_picker.close();
        }
        if self.session_switcher.open && self.session_switcher.preview {
            self.capture_inline_widget_exit();
            self.session_switcher.close();
        }
        self.set_status(SingleSessionStatus::Info(
            "Input cleared - Ctrl+Z to restore".to_string(),
        ));
        KeyOutcome::Redraw
    }

    fn autocomplete_draft(&mut self) -> KeyOutcome {
        let completions = DESKTOP_SLASH_COMMANDS
            .iter()
            .map(|(usage, _)| usage.split_whitespace().next().unwrap_or(*usage))
            .collect::<Vec<_>>();
        let Some((draft, cursor)) =
            complete_slash_command(&self.draft, self.draft_cursor, &completions)
        else {
            return KeyOutcome::None;
        };
        self.remember_input_undo_state();
        self.draft = draft;
        self.draft_cursor = cursor;
        self.clear_draft_selection();
        self.sync_model_picker_preview_from_draft()
            .unwrap_or(KeyOutcome::Redraw)
    }

    fn remember_input_undo_state(&mut self) {
        if self
            .composer
            .input_undo_stack
            .last()
            .is_some_and(|(draft, cursor)| draft == &self.draft && *cursor == self.draft_cursor)
        {
            return;
        }
        self.composer
            .input_undo_stack
            .push((self.draft.clone(), self.draft_cursor));
        const MAX_UNDO: usize = 64;
        if self.composer.input_undo_stack.len() > MAX_UNDO {
            self.composer.input_undo_stack.remove(0);
        }
    }

    fn undo_input_change(&mut self) {
        if let Some((draft, cursor)) = self.composer.input_undo_stack.pop() {
            self.draft = draft;
            self.draft_cursor = cursor.min(self.draft.len());
            self.clamp_draft_cursor();
            self.clear_draft_selection();
        }
    }

    fn clamp_draft_cursor(&mut self) {
        self.draft_cursor = self.draft_cursor.min(self.draft.len());
        while !self.draft.is_char_boundary(self.draft_cursor) {
            self.draft_cursor -= 1;
        }
    }
}

fn styled_line(text: impl Into<String>, style: SingleSessionLineStyle) -> SingleSessionStyledLine {
    SingleSessionStyledLine::new(text, style)
}

fn scroll_status_fragment(scroll_lines: f32) -> String {
    if !scroll_lines.is_finite() || scroll_lines < 0.05 {
        return String::new();
    }
    if (scroll_lines - 1.0).abs() < 0.05 {
        return " · scrolled up 1 line".to_string();
    }
    let rounded = (scroll_lines * 10.0).round() / 10.0;
    if (rounded - rounded.round()).abs() < 0.05 {
        format!(" · scrolled up {} lines", rounded.round() as usize)
    } else {
        format!(" · scrolled up {rounded:.1} lines")
    }
}

fn blank_styled_line() -> SingleSessionStyledLine {
    styled_line(String::new(), SingleSessionLineStyle::Blank)
}

fn stdin_response_styled_lines(state: &StdinResponseState) -> Vec<SingleSessionStyledLine> {
    let kind = if state.is_password {
        "interactive password input"
    } else {
        "interactive input"
    };
    let input = if state.is_password {
        "•".repeat(state.input.chars().count())
    } else if state.input.is_empty() {
        "<empty>".to_string()
    } else {
        state.input.replace(' ', "·")
    };
    vec![
        styled_line(
            format!("{kind} requested"),
            SingleSessionLineStyle::OverlayTitle,
        ),
        styled_line(
            format!("tool: {}", state.tool_call_id),
            SingleSessionLineStyle::Tool,
        ),
        styled_line(
            format!("request: {}", state.request_id),
            SingleSessionLineStyle::Meta,
        ),
        styled_line(
            format!("prompt: {}", state.prompt),
            SingleSessionLineStyle::Meta,
        ),
        blank_styled_line(),
        styled_line(
            format!("input: {input}"),
            SingleSessionLineStyle::OverlaySelection,
        ),
        blank_styled_line(),
        styled_line(
            "Enter send · Ctrl+Enter send · Shift+Enter newline · Ctrl+V paste · Ctrl+U clear · Esc cancel",
            SingleSessionLineStyle::Overlay,
        ),
    ]
}

fn selection_point_cmp(left: SelectionPoint, right: SelectionPoint) -> std::cmp::Ordering {
    left.line
        .cmp(&right.line)
        .then_with(|| left.column.cmp(&right.column))
}

fn slice_by_char_columns(line: &str, start_column: usize, end_column: usize) -> String {
    let start = byte_index_at_char_column(line, start_column);
    let end = byte_index_at_char_column(line, end_column.max(start_column));
    line.get(start..end).unwrap_or_default().to_string()
}

fn byte_index_at_char_column(line: &str, column: usize) -> usize {
    line.char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(line.len()))
        .nth(column)
        .unwrap_or(line.len())
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .char_indices()
        .last()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text[cursor..]
        .char_indices()
        .nth(1)
        .map(|(offset, _)| cursor + offset)
        .unwrap_or(text.len())
}

fn previous_word_start(text: &str, cursor: usize) -> usize {
    let mut start = cursor.min(text.len());
    while start > 0 {
        let previous = previous_char_boundary(text, start);
        let ch = text[previous..start].chars().next().unwrap_or_default();
        if !ch.is_whitespace() {
            break;
        }
        start = previous;
    }
    while start > 0 {
        let previous = previous_char_boundary(text, start);
        let ch = text[previous..start].chars().next().unwrap_or_default();
        if ch.is_whitespace() {
            break;
        }
        start = previous;
    }
    start
}

fn next_word_end(text: &str, cursor: usize) -> usize {
    let mut end = cursor.min(text.len());
    while end < text.len() {
        let next = next_char_boundary(text, end);
        let ch = text[end..next].chars().next().unwrap_or_default();
        if !ch.is_whitespace() {
            break;
        }
        end = next;
    }
    while end < text.len() {
        let next = next_char_boundary(text, end);
        let ch = text[end..next].chars().next().unwrap_or_default();
        if ch.is_whitespace() {
            break;
        }
        end = next;
    }
    end
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor.min(text.len())..]
        .find('\n')
        .map(|offset| cursor + offset)
        .unwrap_or(text.len())
}

fn slash_suggestion_query(input: &str, cursor: usize) -> Option<String> {
    let (start, end) = slash_suggestion_prefix_bounds(input, cursor)?;
    Some(input[start..end].to_string())
}

fn slash_suggestion_prefix_bounds(input: &str, cursor: usize) -> Option<(usize, usize)> {
    let cursor = cursor.min(input.len());
    if !input.is_char_boundary(cursor) {
        return None;
    }
    let prefix = &input[..cursor];
    let start = prefix.len() - prefix.trim_start().len();
    let command_prefix = &input[start..cursor];
    if !command_prefix.starts_with('/') || command_prefix.contains(char::is_whitespace) {
        return None;
    }
    Some((start, cursor))
}

fn complete_slash_command(
    input: &str,
    cursor: usize,
    completions: &[&'static str],
) -> Option<(String, usize)> {
    let cursor = cursor.min(input.len());
    if !input.is_char_boundary(cursor) || !input.starts_with('/') {
        return None;
    }
    let prefix = &input[..cursor];
    if prefix.contains(char::is_whitespace) {
        return None;
    }
    let suffix = &input[cursor..];
    let prefix_key = prefix.to_ascii_lowercase();
    let matches = completions
        .iter()
        .copied()
        .filter(|command| command.starts_with(&prefix_key))
        .collect::<Vec<_>>();
    let completion = match matches.as_slice() {
        [] => fuzzy_slash_completion(&prefix_key, completions)?,
        [only] => *only,
        _ => longest_common_prefix(&matches)?,
    };
    if completion.len() <= prefix.len() {
        return None;
    }
    let mut completed = completion.to_string();
    completed.push_str(suffix);
    Some((completed, completion.len()))
}

fn fuzzy_slash_completion(needle: &str, completions: &[&'static str]) -> Option<&'static str> {
    let mut matches = completions
        .iter()
        .copied()
        .filter_map(|command| {
            desktop_slash_fuzzy_score(needle, command).map(|score| (score, command.len(), command))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(b.2))
    });
    matches.first().map(|(_, _, command)| *command)
}

fn longest_common_prefix<'a>(values: &'a [&'a str]) -> Option<&'a str> {
    let first = *values.first()?;
    let mut end = first.len();
    for value in values.iter().skip(1) {
        while end > 0 && !value.starts_with(&first[..end]) {
            end = previous_char_boundary(first, end);
        }
    }
    (end > 0).then_some(&first[..end])
}

fn short_session_id(session_id: &str) -> &str {
    session_id
        .strip_prefix("session_")
        .and_then(|rest| rest.split('_').next())
        .filter(|name| !name.is_empty())
        .unwrap_or(session_id)
}

pub(crate) fn single_session_surface(
    session: Option<&workspace::SessionCard>,
) -> workspace::Surface {
    let lines = single_session_lines(session);
    workspace::Surface {
        id: 1,
        kind: if session.is_some() {
            workspace::SurfaceKind::Session
        } else {
            workspace::SurfaceKind::Scratch
        },
        title: session
            .map(|session| session.title.clone())
            .unwrap_or_else(|| "new jcode session".to_string()),
        body_lines: lines.clone(),
        detail_lines: lines,
        transcript_messages: Vec::new(),
        session_id: session.map(|session| session.session_id.clone()),
        lane: 0,
        column: 0,
        color_index: 0,
    }
}

pub(crate) fn single_session_lines(session: Option<&workspace::SessionCard>) -> Vec<String> {
    single_session_styled_lines(session)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

pub(crate) fn single_session_styled_lines(
    session: Option<&workspace::SessionCard>,
) -> Vec<SingleSessionStyledLine> {
    let Some(session) = session else {
        return vec![
            styled_line("single session mode", SingleSessionLineStyle::OverlayTitle),
            styled_line(
                "fresh desktop-native session draft",
                SingleSessionLineStyle::Status,
            ),
            styled_line(
                "type here without nav or insert modes",
                SingleSessionLineStyle::Overlay,
            ),
            styled_line(
                "Enter sends through the shared desktop session runtime",
                SingleSessionLineStyle::Overlay,
            ),
            styled_line(
                "ctrl+; clears this draft and starts another fresh desktop session",
                SingleSessionLineStyle::Overlay,
            ),
            styled_line(
                "run with --workspace for the niri layout wrapper",
                SingleSessionLineStyle::Overlay,
            ),
        ];
    };

    let mut lines = vec![
        styled_line("single session mode", SingleSessionLineStyle::OverlayTitle),
        styled_line(session.subtitle.clone(), SingleSessionLineStyle::Status),
        styled_line(session.detail.clone(), SingleSessionLineStyle::Meta),
    ];
    if !session.preview_lines.is_empty() {
        lines.push(styled_line(
            "recent transcript",
            SingleSessionLineStyle::OverlayTitle,
        ));
        lines.extend(
            session
                .preview_lines
                .iter()
                .cloned()
                .map(|line| styled_line(line, SingleSessionLineStyle::Assistant)),
        );
    }
    if !session.detail_lines.is_empty() {
        lines.push(styled_line(
            "expanded transcript",
            SingleSessionLineStyle::OverlayTitle,
        ));
        lines.extend(
            session
                .detail_lines
                .iter()
                .cloned()
                .map(|line| styled_line(line, SingleSessionLineStyle::Assistant)),
        );
    }
    lines
}

#[cfg(test)]
mod tests;
