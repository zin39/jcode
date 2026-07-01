mod animation;
mod desktop_app_driver;
mod desktop_benchmark;
mod desktop_config;
mod desktop_gallery;
mod desktop_ipc;
mod desktop_issue_browser;
mod desktop_issue_cache;
mod desktop_log;
mod desktop_prefs;
mod desktop_protocol;
mod desktop_rich_text;
mod desktop_scene;
mod desktop_session_events;
mod desktop_ui_engine;
mod desktop_worker_host;
mod power_inhibit;
mod render_helpers;
mod session_data;
mod session_launch;
mod single_session;
mod single_session_render;
#[cfg(test)]
#[path = "state_space_tests.rs"]
mod state_space_tests;
mod workspace;

mod workspace_vertices;
pub(crate) use workspace_vertices::*;
mod hero_mask;
pub(crate) use hero_mask::*;
mod canvas;
pub(crate) use canvas::*;
use ab_glyph::{Font, FontArc, Glyph as AbGlyph, PxScale, ScaleFont, point};
use animation::{
    APP_MODE_TRANSITION_DURATION, AnimatedRect, AnimatedViewport, ColorTransition, FocusPulse,
    StatusTextTransition, StatusTextTransitionFrame, StatusTextVisualFrame,
    SurfaceTransitionAnimator, SurfaceVisualFrame, SurfaceVisualTarget, VisibleColumnLayout,
    WorkspaceRenderLayout,
};
use anyhow::{Context, Result};
use base64::Engine;
use bytemuck::{Pod, Zeroable};
use desktop_app_driver::{
    DESKTOP_UI_SNAPSHOT_VERSION, DesktopAppDriver, DesktopAppRuntime, DesktopSceneBuildContext,
    DesktopSingleSessionSnapshot, DesktopSnapshotRestoreError, DesktopSurfaceSnapshot,
    DesktopUiSnapshot, DesktopWorkspaceSnapshot, DesktopWorkspaceSurfaceSnapshot,
};
use desktop_benchmark::*;
use desktop_config::*;
use desktop_ipc::{DesktopHostToWorkerEnvelope, write_desktop_ipc_frame};
#[cfg(test)]
pub(crate) use desktop_issue_browser::IssueBrowserLayoutMode;
use desktop_issue_browser::{
    IssueBrowserLayout, compose_single_session_issue_browser_vertices, issue_browser_layout,
};
use desktop_protocol::{
    DesktopHostToWorkerMessage, DesktopInputEvent, DesktopKeyEvent, DesktopKeyModifiers,
    DesktopMouseButton, DesktopMouseEvent, DesktopProtocolEnvelope, DesktopSceneUpdate,
    DesktopSessionEventBatchWire, DesktopSessionEventWire, DesktopSnapshotResponse,
    DesktopWindowEvent, DesktopWindowState, DesktopWorkerInit, DesktopWorkerMode,
    DesktopWorkerReady, DesktopWorkerShutdownReason, DesktopWorkerToHostMessage,
};
use desktop_scene::{
    DesktopColor, DesktopDisplayCommand, DesktopRect as DesktopSceneRect, DesktopRectPaint,
    DesktopScene, DesktopSceneViewport,
};
use desktop_session_events::{
    BACKEND_EVENT_FORWARD_INTERVAL, BACKEND_EVENT_FORWARD_MAX_PAYLOAD_BYTES,
    BACKEND_EVENT_FORWARD_MAX_RAW_EVENTS, DesktopSessionEventBatch,
    coalesce_desktop_session_events, collect_desktop_session_event_batch,
    spawn_session_event_forwarder,
};
use desktop_worker_host::DesktopWorkerConnection;
use glyphon::{
    Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Wrap,
};
use image::RgbaImage;
use render_helpers::*;
use session_launch::DesktopSessionStatus;
use single_session::{
    ReasoningEffortCycleOutcome, SINGLE_SESSION_FONT_FAMILY, SINGLE_SESSION_WELCOME_FONT_FAMILY,
    SelectionPoint, SingleSessionApp, SingleSessionLineStyle, SingleSessionMessage,
    SingleSessionStyledLine, handwritten_welcome_phrase, single_session_surface,
    single_session_typography, single_session_typography_for_scale,
};
use single_session_render::*;
use wgpu::{CompositeAlphaMode, PresentMode, SurfaceError, TextureUsages};
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Fullscreen, Window, WindowBuilder};
use workspace::{InputMode, KeyInput, KeyOutcome, PanelSizePreset, Workspace};

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_WINDOW_WIDTH: f64 = 1280.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;
const DESKTOP_RELOAD_WINDOW_ENV: &str = "JCODE_DESKTOP_RELOAD_WINDOW";
const DESKTOP_RELOAD_HANDOFF_READY_ENV: &str = "JCODE_DESKTOP_RELOAD_READY_FILE";
const DESKTOP_RELOAD_HANDOFF_RELEASE_ENV: &str = "JCODE_DESKTOP_RELOAD_RELEASE_FILE";
const DESKTOP_RELOAD_HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(25);
const DESKTOP_RELOAD_HANDOFF_TIMEOUT: Duration = Duration::from_secs(8);
const DESKTOP_RELOAD_STARTUP_RELEASE_TIMEOUT: Duration = Duration::from_secs(3);
const DESKTOP_RELOAD_MAX_RESTORED_DIMENSION: u32 = 32_768;
const OUTER_PADDING: f32 = 12.0;
const GAP: f32 = 10.0;
const STATUS_BAR_HEIGHT: f32 = 30.0;
const FOCUSED_BORDER_WIDTH: f32 = 2.0;
const UNFOCUSED_BORDER_WIDTH: f32 = 1.5;
const PANEL_RADIUS: f32 = 12.0;
const STATUS_RADIUS: f32 = 9.0;
const ROUNDED_CORNER_SEGMENTS: usize = 6;
const PANEL_FIT_TOLERANCE: f32 = 0.15;
const STATUS_PREVIEW_LANE_RADIUS: i32 = 2;
const STATUS_PREVIEW_MAX_WIDTH: f32 = 420.0;
const STATUS_PREVIEW_HEIGHT: f32 = 14.0;
const STATUS_PREVIEW_PANEL_WIDTH: f32 = 9.0;
const STATUS_PREVIEW_PANEL_GAP: f32 = 2.0;
const STATUS_PREVIEW_GROUP_GAP: f32 = 10.0;
const STATUS_PREVIEW_SIDE_RESERVE: f32 = 74.0;
const STATUS_PREVIEW_MAX_TICKS_PER_LANE: i32 = 32;
const SPACE_HOLD_PROGRESS_HEIGHT: f32 = 7.0;
const SPACE_HOLD_PROGRESS_WIDTH_FRACTION: f32 = 0.36;
const SPACE_HOLD_PROGRESS_TRACK_COLOR: [f32; 4] = [0.055, 0.060, 0.075, 0.96];
const SPACE_HOLD_PROGRESS_FILL_COLOR: [f32; 4] = [0.180, 0.900, 0.470, 1.0];
const WORKSPACE_NUMBER_LEFT_PADDING: f32 = 14.0;
const WORKSPACE_NUMBER_DIGIT_WIDTH: f32 = 8.0;
const WORKSPACE_NUMBER_DIGIT_HEIGHT: f32 = 14.0;
const WORKSPACE_NUMBER_DIGIT_GAP: f32 = 4.0;
const WORKSPACE_NUMBER_STROKE: f32 = 2.0;
const BITMAP_TEXT_PIXEL: f32 = 2.0;
const STATUS_TEXT_RIGHT_PADDING: f32 = 14.0;
const PANEL_TITLE_LEFT_PADDING: f32 = 12.0;
const PANEL_TITLE_TOP_PADDING: f32 = 12.0;
const PANEL_BODY_TOP_PADDING: f32 = 38.0;
const PANEL_BODY_LINE_GAP: f32 = 8.0;
const SINGLE_SESSION_DRAFT_TOP_OFFSET: f32 = 158.0;
const SINGLE_SESSION_CARET_WIDTH: f32 = 2.0;
const SINGLE_SESSION_CARET_COLOR: [f32; 4] = [0.130, 0.150, 0.190, 0.92];
const SESSION_SPAWN_REFRESH_DELAY: Duration = Duration::from_millis(350);
const BACKGROUND_POLL_INTERVAL: Duration = Duration::from_millis(33);
const BACKEND_REDRAW_FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// Minimum spacing between animation-driven redraws.
///
/// Without this, the desktop render loop re-requests a redraw immediately after
/// every animated frame (welcome-hero reveal, focus pulse, spinners, smooth
/// scroll, etc.). Because the surface uses non-blocking `Mailbox` presentation,
/// `present()` returns instantly, so the unthrottled loop renders at hundreds of
/// fps and pins the main thread near 100% CPU, starving input handling and the
/// compositor (the root cause of desktop lag/jank). ~16ms paces continuous
/// animations to about 60fps, matching typical display refresh.
const DESKTOP_ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const SURFACE_TIMEOUT_BACKOFF_MIN: Duration = Duration::from_millis(16);
const SURFACE_TIMEOUT_BACKOFF_MAX: Duration = Duration::from_millis(250);
const HEADLESS_CHAT_SMOKE_TIMEOUT: Duration = Duration::from_secs(90);
const DESKTOP_SPINNER_FRAME_MS: u128 = 180;
const MOUSE_WHEEL_LINES_PER_DETENT: f32 = 3.0;
const MAX_MOUSE_SCROLL_LINES_PER_EVENT: f32 = 24.0;
const SCROLL_GESTURE_IDLE_RESET: Duration = Duration::from_millis(180);
const SCROLL_FRACTIONAL_EPSILON: f32 = 0.000_1;
const SCROLL_MOMENTUM_GAIN: f32 = 8.5;
const SCROLL_MOMENTUM_DECAY_PER_SECOND: f32 = 7.0;
const SCROLL_MOMENTUM_MAX_VELOCITY: f32 = 72.0;
const SCROLL_MOMENTUM_STOP_VELOCITY: f32 = 0.08;
const SCROLL_FRAME_MAX_DT_SECONDS: f32 = 0.050;
const SINGLE_SESSION_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(90);
const SINGLE_SESSION_BODY_TEXT_WINDOW_BEFORE_LINES: usize = 8;
const SINGLE_SESSION_BODY_TEXT_WINDOW_AFTER_LINES: usize = 16;
const SINGLE_SESSION_STREAMING_BODY_TEXT_WINDOW_BEFORE_LINES: usize = 2;
const SINGLE_SESSION_STREAMING_BODY_TEXT_WINDOW_AFTER_LINES: usize = 4;
const STREAMING_TEXT_FADE_DURATION: Duration = Duration::from_millis(150);
const STREAMING_TEXT_FADE_START_OPACITY: f32 = 0.4;
const STREAMING_TEXT_RISE_START_OFFSET_PIXELS: f32 = 3.5;
const STREAMING_TEXT_HANDOFF_DURATION: Duration = Duration::from_millis(135);
const STREAMING_TEXT_HANDOFF_START_OPACITY: f32 = 0.18;
const DESKTOP_ASYNC_JOB_LIMIT: usize = 12;
const PRIMITIVE_VERTEX_BUFFER_MIN_CAPACITY: usize = 1024;
const PRIMITIVE_VERTEX_BUFFER_SHRINK_RATIO: usize = 4;
const WORKSPACE_BASE_VERTEX_CAPACITY_HINT: usize = 512;
const WORKSPACE_SURFACE_VERTEX_CAPACITY_HINT: usize = 2048;
static DESKTOP_ASYNC_JOB_COUNT: AtomicUsize = AtomicUsize::new(0);

struct DesktopAsyncJobPermit<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for DesktopAsyncJobPermit<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

fn try_acquire_desktop_async_job_slot<'a>(
    counter: &'a AtomicUsize,
    limit: usize,
) -> Result<DesktopAsyncJobPermit<'a>> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            anyhow::bail!("desktop async job limit reached ({limit})");
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(DesktopAsyncJobPermit { counter }),
            Err(next_current) => current = next_current,
        }
    }
}

fn spawn_bounded_desktop_async_job(
    name: impl Into<String>,
    job: impl FnOnce() + Send + 'static,
) -> Result<()> {
    let name = name.into();
    let permit =
        try_acquire_desktop_async_job_slot(&DESKTOP_ASYNC_JOB_COUNT, DESKTOP_ASYNC_JOB_LIMIT)
            .with_context(|| format!("failed to start {name}"))?;
    std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let _permit = permit;
            job();
        })
        .with_context(|| format!("failed to spawn {name}"))?;
    Ok(())
}

#[derive(Clone)]
struct DesktopReasoningEffortRequestQueue {
    request_tx: mpsc::Sender<DesktopReasoningEffortRequest>,
    latest_generation: Arc<AtomicU64>,
}

struct DesktopReasoningEffortRequest {
    generation: u64,
    effort: String,
    target_session_id: Option<String>,
    event_tx: session_launch::DesktopSessionEventSender,
}

impl DesktopReasoningEffortRequestQueue {
    fn request(
        &self,
        effort: String,
        target_session_id: Option<String>,
        event_tx: session_launch::DesktopSessionEventSender,
    ) -> Result<()> {
        let generation = self.latest_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.request_tx
            .send(DesktopReasoningEffortRequest {
                generation,
                effort,
                target_session_id,
                event_tx,
            })
            .context("failed to queue desktop reasoning effort change")
    }
}

fn spawn_desktop_reasoning_effort_request_queue() -> Result<DesktopReasoningEffortRequestQueue> {
    let (request_tx, request_rx) = mpsc::channel();
    let latest_generation = Arc::new(AtomicU64::new(0));
    let worker_latest_generation = Arc::clone(&latest_generation);
    std::thread::Builder::new()
        .name("jcode-desktop-effort-queue".to_string())
        .spawn(move || {
            run_desktop_reasoning_effort_request_queue(request_rx, worker_latest_generation);
        })
        .context("failed to spawn desktop reasoning effort queue")?;
    Ok(DesktopReasoningEffortRequestQueue {
        request_tx,
        latest_generation,
    })
}

fn run_desktop_reasoning_effort_request_queue(
    request_rx: mpsc::Receiver<DesktopReasoningEffortRequest>,
    latest_generation: Arc<AtomicU64>,
) {
    while let Ok(mut request) = request_rx.recv() {
        let mut coalesced = 0usize;
        let mut disconnected = false;
        loop {
            match request_rx.try_recv() {
                Ok(next_request) => {
                    request = next_request;
                    coalesced += 1;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if coalesced > 0 {
            desktop_log::info(format_args!(
                "jcode-desktop: coalesced {coalesced} superseded reasoning effort request(s); applying {}",
                desktop_log::truncate_for_log(&request.effort, 64)
            ));
        }
        apply_desktop_reasoning_effort_request(request, &latest_generation);
        if disconnected {
            break;
        }
    }
}

fn apply_desktop_reasoning_effort_request(
    request: DesktopReasoningEffortRequest,
    latest_generation: &AtomicU64,
) {
    let (response_tx, response_rx) = mpsc::channel();
    let result = session_launch::set_reasoning_effort(
        &request.effort,
        request.target_session_id.as_deref(),
        Some(response_tx),
    );
    let still_latest = latest_generation.load(Ordering::Acquire) == request.generation;
    if still_latest {
        for event in response_rx.try_iter() {
            let _ = request.event_tx.send(event);
        }
        if let Err(error) = result {
            desktop_log::error(format_args!(
                "jcode-desktop: reasoning effort sync failed generation={} target_session={}: {error:#}",
                request.generation,
                request.target_session_id.as_deref().unwrap_or("<current>")
            ));
            let _ = request
                .event_tx
                .send(session_launch::DesktopSessionEvent::Status(
                    DesktopSessionStatus::ReasoningEffortFailed(format!("{error:#}")),
                ));
        }
    } else if let Err(error) = result {
        desktop_log::warn(format_args!(
            "jcode-desktop: stale reasoning effort sync failed generation={} target_session={}: {error:#}",
            request.generation,
            request.target_session_id.as_deref().unwrap_or("<current>")
        ));
    } else {
        let dropped = response_rx.try_iter().count();
        desktop_log::info(format_args!(
            "jcode-desktop: dropped stale reasoning effort response generation={} event_count={dropped}",
            request.generation
        ));
    }
}

#[derive(Clone, Debug, Default)]
struct SurfaceTimeoutBackoff {
    consecutive_timeouts: u32,
}

impl SurfaceTimeoutBackoff {
    fn reset(&mut self) {
        self.consecutive_timeouts = 0;
    }

    fn record_timeout(&mut self) -> (Duration, u32) {
        let exponent = self.consecutive_timeouts.min(4);
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        let delay = SURFACE_TIMEOUT_BACKOFF_MIN
            .saturating_mul(1_u32 << exponent)
            .min(SURFACE_TIMEOUT_BACKOFF_MAX);
        (delay, self.consecutive_timeouts)
    }
}

fn desktop_surface_size_is_renderable(size: PhysicalSize<u32>) -> bool {
    size.width > 0 && size.height > 0
}

fn desktop_background_wake(
    now: Instant,
    surface_renderable: bool,
    frame_animation_active: bool,
) -> Option<Instant> {
    if surface_renderable && frame_animation_active {
        Some(now + BACKGROUND_POLL_INTERVAL)
    } else {
        None
    }
}

/// Compute the next paced animation redraw time.
///
/// Returns `Some(now + DESKTOP_ANIMATION_FRAME_INTERVAL)` while an animation is
/// active and `None` once it settles. Callers schedule this instead of calling
/// `request_redraw()` immediately, which would render as fast as the CPU allows
/// (the surface presents without blocking) and pin the main thread near 100%
/// CPU, starving input handling and the compositor.
fn next_animation_redraw_at(now: Instant, animation_active: bool) -> Option<Instant> {
    animation_active.then(|| now + DESKTOP_ANIMATION_FRAME_INTERVAL)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StreamingTextArrivalStyle {
    opacity: f32,
    y_offset_pixels: f32,
    active: bool,
}

fn streaming_text_arrival_style_for_elapsed(elapsed: Duration) -> StreamingTextArrivalStyle {
    if animation::desktop_reduced_motion_enabled() {
        return StreamingTextArrivalStyle {
            opacity: 1.0,
            y_offset_pixels: 0.0,
            active: false,
        };
    }

    let progress =
        (elapsed.as_secs_f32() / STREAMING_TEXT_FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    if progress >= 1.0 {
        return StreamingTextArrivalStyle {
            opacity: 1.0,
            y_offset_pixels: 0.0,
            active: false,
        };
    }
    let eased = animation::ease_out_cubic(progress);
    StreamingTextArrivalStyle {
        opacity: STREAMING_TEXT_FADE_START_OPACITY
            + (1.0 - STREAMING_TEXT_FADE_START_OPACITY) * eased,
        y_offset_pixels: STREAMING_TEXT_RISE_START_OFFSET_PIXELS * (1.0 - eased),
        active: true,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn streaming_text_fade_opacity_for_elapsed(elapsed: Duration) -> (f32, bool) {
    let style = streaming_text_arrival_style_for_elapsed(elapsed);
    (style.opacity, style.active)
}

fn streaming_text_fade_start_after_len_change(
    previous_len: usize,
    next_len: usize,
    current_started_at: Option<Instant>,
    now: Instant,
) -> Option<Instant> {
    if next_len == 0 {
        return None;
    }

    let fade_active = current_started_at.is_some_and(|started_at| {
        now.saturating_duration_since(started_at) < STREAMING_TEXT_FADE_DURATION
    });
    if fade_active {
        return current_started_at;
    }

    // Only fade in the beginning of a streaming response. Restarting after
    // every slow delta dims the already-visible response and reads as flicker.
    if previous_len == 0 && next_len > 0 {
        Some(now)
    } else {
        None
    }
}

fn streaming_text_handoff_style_for_elapsed(elapsed: Duration) -> StreamingTextArrivalStyle {
    if animation::desktop_reduced_motion_enabled() {
        return StreamingTextArrivalStyle {
            opacity: 0.0,
            y_offset_pixels: 0.0,
            active: false,
        };
    }

    let progress =
        (elapsed.as_secs_f32() / STREAMING_TEXT_HANDOFF_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    if progress >= 1.0 {
        return StreamingTextArrivalStyle {
            opacity: 0.0,
            y_offset_pixels: 0.0,
            active: false,
        };
    }

    let eased = animation::ease_out_cubic(progress);
    StreamingTextArrivalStyle {
        opacity: STREAMING_TEXT_HANDOFF_START_OPACITY * (1.0 - eased),
        y_offset_pixels: 0.0,
        active: true,
    }
}

/// Body cache key that also tracks how much of the streaming response is
/// revealed, so the cached wrapped lines rebuild as the reveal advances.
fn streaming_reveal_body_cache_key(
    rendered_body_key: u64,
    streaming_response_empty: bool,
    revealed_bytes: usize,
) -> u64 {
    if streaming_response_empty {
        return rendered_body_key;
    }
    let mut hasher = DefaultHasher::new();
    rendered_body_key.hash(&mut hasher);
    revealed_bytes.hash(&mut hasher);
    hasher.finish()
}

fn streaming_text_handoff_start_after_len_change(
    previous_len: usize,
    next_len: usize,
    has_visible_streaming_buffer: bool,
    current_started_at: Option<Instant>,
    now: Instant,
) -> Option<Instant> {
    if animation::desktop_reduced_motion_enabled() || next_len > 0 {
        return None;
    }

    if previous_len > 0 && has_visible_streaming_buffer {
        return Some(now);
    }

    current_started_at.filter(|started_at| {
        now.saturating_duration_since(*started_at) < STREAMING_TEXT_HANDOFF_DURATION
    })
}
const DESKTOP_120FPS_FRAME_BUDGET: Duration = Duration::from_micros(8_333);
const DESKTOP_PRESENT_STALL_BUDGET: Duration = Duration::from_millis(33);
const DESKTOP_INPUT_LATENCY_BUDGET: Duration = Duration::from_millis(25);
const DESKTOP_NO_PAINT_BUDGET: Duration = Duration::from_millis(250);
const DESKTOP_FRAME_PROFILE_REPORT_INTERVAL: Duration = Duration::from_secs(1);

const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.955,
    g: 0.965,
    b: 0.985,
    a: 1.0,
};

const BACKGROUND_TOP_LEFT: [f32; 4] = [0.890, 0.930, 1.000, 1.0];
const BACKGROUND_TOP_RIGHT: [f32; 4] = [0.960, 0.910, 1.000, 1.0];
const BACKGROUND_BOTTOM_RIGHT: [f32; 4] = [0.875, 0.980, 0.930, 1.0];
const BACKGROUND_BOTTOM_LEFT: [f32; 4] = [0.945, 0.960, 0.995, 1.0];
const FOCUS_RING_COLOR: [f32; 4] = [0.135, 0.155, 0.205, 0.90];
const NAV_STATUS_COLOR: [f32; 4] = [0.145, 0.165, 0.220, 1.0];
const INSERT_STATUS_COLOR: [f32; 4] = [0.245, 0.395, 0.340, 1.0];
const STATUS_PREVIEW_ACTIVE_GROUP_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.16];
const STATUS_PREVIEW_EMPTY_FOCUSED_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.50];
const STATUS_PREVIEW_VIEWPORT_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.78];
const WORKSPACE_NUMBER_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.90];
const STATUS_TEXT_COLOR: [f32; 4] = [0.953, 0.965, 0.984, 0.88];
const PANEL_TITLE_COLOR: [f32; 4] = [0.010, 0.014, 0.025, 1.0];
const PANEL_BODY_COLOR: [f32; 4] = [0.008, 0.012, 0.020, 1.0];
const ASSISTANT_TEXT_COLOR: [f32; 4] = [0.026, 0.034, 0.052, 1.0];
const ASSISTANT_HEADING_TEXT_COLOR: [f32; 4] = [0.030, 0.095, 0.300, 1.0];
const ASSISTANT_QUOTE_TEXT_COLOR: [f32; 4] = [0.210, 0.090, 0.355, 1.0];
const ASSISTANT_TABLE_TEXT_COLOR: [f32; 4] = [0.000, 0.155, 0.185, 1.0];
const ASSISTANT_LINK_TEXT_COLOR: [f32; 4] = [0.000, 0.170, 0.430, 1.0];
const USER_TEXT_COLOR: [f32; 4] = [0.012, 0.030, 0.180, 1.0];
const USER_CONTINUATION_TEXT_COLOR: [f32; 4] = [0.018, 0.035, 0.155, 1.0];
const TOOL_TEXT_COLOR: [f32; 4] = [0.150, 0.095, 0.325, 1.0];
const TOOL_DETAIL_TEXT_COLOR: [f32; 4] = [0.135, 0.155, 0.220, 1.0];
const TOOL_MUTED_TEXT_COLOR: [f32; 4] = [0.345, 0.365, 0.430, 0.96];
const TOOL_RUNNING_TEXT_COLOR: [f32; 4] = [0.045, 0.265, 0.640, 1.0];
const TOOL_SUCCESS_TEXT_COLOR: [f32; 4] = [0.035, 0.360, 0.220, 1.0];
const TOOL_FAILED_TEXT_COLOR: [f32; 4] = [0.560, 0.070, 0.095, 1.0];
const TOOL_PENDING_TEXT_COLOR: [f32; 4] = [0.320, 0.345, 0.405, 1.0];
const TOOL_CARD_BACKGROUND_COLOR: [f32; 4] = [0.985, 0.990, 1.000, 0.68];
const TOOL_CARD_ACTIVE_BACKGROUND_COLOR: [f32; 4] = [0.890, 0.945, 1.000, 0.72];
const TOOL_CARD_SUCCESS_BACKGROUND_COLOR: [f32; 4] = [0.875, 0.975, 0.925, 0.56];
const TOOL_CARD_FAILED_BACKGROUND_COLOR: [f32; 4] = [1.000, 0.900, 0.910, 0.64];
const TOOL_CARD_GROUP_BACKGROUND_COLOR: [f32; 4] = [0.945, 0.930, 1.000, 0.50];
const TOOL_CARD_BORDER_COLOR: [f32; 4] = [0.105, 0.165, 0.295, 0.22];
const TOOL_CARD_ACTIVE_BORDER_COLOR: [f32; 4] = [0.000, 0.260, 0.720, 0.36];
const TOOL_TIMELINE_RAIL_COLOR: [f32; 4] = [0.105, 0.165, 0.295, 0.20];
const TOOL_TIMELINE_ACTIVE_RAIL_COLOR: [f32; 4] = [0.000, 0.260, 0.720, 0.46];
const TOOL_OUTPUT_DRAWER_COLOR: [f32; 4] = [0.030, 0.055, 0.095, 0.070];
const TOOL_STATUS_CHIP_COLOR: [f32; 4] = [1.000, 1.000, 1.000, 0.42];
const META_TEXT_COLOR: [f32; 4] = [0.095, 0.110, 0.155, 0.98];
const CODE_TEXT_COLOR: [f32; 4] = [0.055, 0.065, 0.095, 1.0];
const STATUS_TEXT_ACCENT_COLOR: [f32; 4] = [0.030, 0.125, 0.080, 1.0];
const ERROR_TEXT_COLOR: [f32; 4] = [0.360, 0.000, 0.000, 1.0];
const OVERLAY_TEXT_COLOR: [f32; 4] = [0.030, 0.045, 0.075, 1.0];
const OVERLAY_SELECTION_TEXT_COLOR: [f32; 4] = [0.010, 0.035, 0.105, 1.0];
const USER_PROMPT_ACCENT_COLOR: [f32; 4] = [0.000, 0.105, 0.250, 1.0];
const PANEL_SECTION_COLOR: [f32; 4] = [0.045, 0.055, 0.080, 0.95];
const SELECTION_HIGHLIGHT_COLOR: [f32; 4] = [0.220, 0.420, 0.700, 0.22];
const WELCOME_AURORA_BLUE: [f32; 4] = [0.250, 0.520, 1.000, 0.145];
const WELCOME_AURORA_VIOLET: [f32; 4] = [0.720, 0.360, 0.980, 0.125];
const WELCOME_AURORA_MINT: [f32; 4] = [0.220, 0.840, 0.660, 0.115];
const WELCOME_AURORA_WARM: [f32; 4] = [1.000, 0.620, 0.360, 0.075];
const WELCOME_HANDWRITING_COLOR: [f32; 4] = [0.012, 0.080, 0.250, 0.94];
const NATIVE_SPINNER_HEAD_COLOR: [f32; 4] = [0.000, 0.260, 0.720, 1.0];
const CODE_BLOCK_BACKGROUND_COLOR: [f32; 4] = [0.075, 0.095, 0.135, 0.105];
const INLINE_CODE_BACKGROUND_COLOR: [f32; 4] = [0.075, 0.095, 0.135, 0.175];
const QUOTE_CARD_BACKGROUND_COLOR: [f32; 4] = [0.520, 0.330, 0.760, 0.090];
const TABLE_CARD_BACKGROUND_COLOR: [f32; 4] = [0.080, 0.460, 0.520, 0.085];
const ERROR_CARD_BACKGROUND_COLOR: [f32; 4] = [0.850, 0.170, 0.170, 0.105];
const OVERLAY_SELECTION_BACKGROUND_COLOR: [f32; 4] = [0.280, 0.470, 0.780, 0.115];
const STATUS_PREVIEW_ACCENTS: [[f32; 3]; 8] = [
    [0.560, 0.690, 0.980],
    [0.780, 0.610, 0.910],
    [0.520, 0.760, 0.620],
    [0.900, 0.650, 0.450],
    [0.600, 0.780, 0.840],
    [0.880, 0.580, 0.690],
    [0.720, 0.740, 0.820],
    [0.810, 0.760, 0.520],
];



fn main() {
    desktop_log::init();
    install_desktop_diagnostic_hooks();
    desktop_log::info(format_args!(
        "jcode-desktop: starting pid={} version={} build_hash={}",
        std::process::id(),
        desktop_header_version_label(),
        desktop_build_hash_label()
    ));

    if let Err(error) = pollster::block_on(run()) {
        desktop_log::error(format_args!("jcode-desktop: fatal error: {error:#}"));
        std::process::exit(1);
    }
}

fn install_desktop_diagnostic_hooks() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        desktop_log::error(format_args!("jcode-desktop: panic: {panic_info}"));
        desktop_log::error(format_args!(
            "jcode-desktop: panic backtrace: {}",
            std::backtrace::Backtrace::force_capture()
        ));
        default_hook(panic_info);
    }));
}

async fn run() -> Result<()> {
    log_desktop_platform_support_warning();
    let args = std::env::args().collect::<Vec<_>>();
    let startup_benchmark = startup_benchmark_requested(&args);
    let startup_content_benchmark = startup_content_benchmark_requested(&args);
    let startup_trace = DesktopStartupTrace::new(
        startup_benchmark || startup_content_benchmark || startup_log_requested(&args),
    );
    startup_trace.mark("args parsed");
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", desktop_help_text());
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "{} {}",
            desktop_header_version_label(),
            desktop_build_hash_label()
        );
        return Ok(());
    }
    if let Some(message) = headless_chat_smoke_message(&args) {
        return run_headless_chat_smoke(message);
    }
    if let Some(frames) = resize_render_benchmark_frames(&args) {
        return run_resize_render_benchmark(frames);
    }
    if let Some(frames) = scroll_render_benchmark_frames(&args) {
        return run_scroll_render_benchmark(frames);
    }
    if let Some(frames) = real_transcript_scroll_benchmark_frames(&args) {
        return run_real_transcript_scroll_benchmark(frames);
    }
    if let Some(frames) = real_transcript_action_benchmark_frames(&args) {
        return run_real_transcript_action_benchmark(frames);
    }
    if let Some(output_dir) = hero_screenshot_capture_dir(&args) {
        return run_hero_screenshot_capture(&output_dir).await;
    }
    if let Some(capture) = gallery_screenshot_capture_request(&args) {
        return run_gallery_screenshot_capture(&capture).await;
    }
    if let Some(raw_events) = stream_e2e_benchmark_raw_events(&args) {
        return run_stream_e2e_benchmark(raw_events);
    }
    if desktop_gallery::launcher_requested(&args) {
        return desktop_gallery::launch_temporary_windows();
    }
    let fullscreen = args.iter().any(|arg| arg == "--fullscreen");
    let desktop_gallery_state = desktop_gallery::state_from_args(&args);
    let desktop_gallery = desktop_gallery_state.is_some();
    let process_role = desktop_process_role_from_args(args.iter().map(String::as_str));
    let desktop_mode = desktop_mode_from_args(args.iter().map(String::as_str));
    if process_role == DesktopProcessRole::AppWorker {
        return run_desktop_app_worker_process(desktop_mode);
    }
    let resume_session_id = desktop_resume_session_id_from_args(args.iter().map(String::as_str));
    let desktop_reload_startup = DesktopReloadStartup::from_env();
    emit_desktop_profile_event(
        "jcode-desktop-launch-profile",
        serde_json::json!({
            "mode": desktop_mode.as_str(),
            "process_role": process_role.as_str(),
            "version": desktop_header_version_label(),
            "build_hash": desktop_build_hash_label(),
            "pid": std::process::id(),
        }),
    );
    let event_loop = EventLoopBuilder::<DesktopUserEvent>::with_user_event()
        .build()
        .context("failed to create event loop")?;
    let event_loop_proxy = event_loop.create_proxy();
    startup_trace.mark("event loop created");
    let mut window_builder = WindowBuilder::new().with_title("Jcode Desktop");
    if let Some(placement) = desktop_reload_startup.window_placement {
        window_builder = placement.apply_to_window_builder(window_builder);
    } else {
        window_builder = window_builder.with_inner_size(LogicalSize::new(
            DEFAULT_WINDOW_WIDTH,
            DEFAULT_WINDOW_HEIGHT,
        ));
    }

    if desktop_reload_startup.hidden_until_handoff_release() {
        window_builder = window_builder.with_visible(false);
    }

    if fullscreen {
        window_builder = window_builder.with_fullscreen(Some(Fullscreen::Borderless(None)));
    }

    let window = Arc::new(
        window_builder
            .build(&event_loop)
            .context("failed to create desktop window")?,
    );
    startup_trace.mark("window created");
    let mut renderer = DesktopHostRendererState::NoGpuBoot;
    renderer.start_gpu_init(window.clone(), event_loop_proxy.clone(), startup_trace)?;
    startup_trace.mark("canvas init spawned");

    let mut pending_workspace_startup_load = false;
    let mut pending_workspace_startup_preferences = None;
    let mut app = if let Some(gallery_state) = desktop_gallery_state.as_deref() {
        desktop_gallery::temporary_app(gallery_state)
    } else if desktop_mode == DesktopMode::WorkspacePrototype {
        let mut workspace = Workspace::loading_sessions();
        if let Some(preferences) = load_desktop_preferences() {
            workspace.apply_preferences(preferences.clone());
            pending_workspace_startup_preferences = Some(preferences);
        }
        pending_workspace_startup_load = true;
        DesktopApp::Workspace(workspace)
    } else {
        initial_single_session_app(resume_session_id.as_deref())
    };
    startup_trace.mark("app state initialized");
    window.set_title(&app.status_title());
    let mut reload_startup_handoff = desktop_reload_startup.handoff;
    let mut modifiers = ModifiersState::empty();
    let mut cursor_position = winit::dpi::PhysicalPosition::new(0.0, 0.0);
    let mut selecting_body = false;
    let mut selecting_draft = false;
    let mut scroll_accumulator = ScrollLineAccumulator::default();
    let mut scroll_metrics_cache = SingleSessionScrollMetricsCache::default();
    let mut hot_reloader = DesktopHotReloader::new(process_role.reload_strategy());
    if process_role == DesktopProcessRole::StableHost {
        hot_reloader.start_app_worker_for_current_binary(&app, &window, "stable host startup");
    }
    let preferences_save_tx = spawn_desktop_preferences_saver();
    let mut power_inhibitor = power_inhibit::PowerInhibitor::new();
    let (session_event_tx, session_event_rx) = mpsc::channel();
    spawn_session_event_forwarder(session_event_rx, event_loop_proxy.clone());
    if simulate_stream_requested(&args) && app.is_single_session() {
        // Dev-only: drive the real streaming pipeline with synthetic, bursty
        // TextDelta events so the streaming reveal animation can be observed and
        // recorded live without a backend. Mirrors provider chunk cadence.
        if let DesktopApp::SingleSession(single) = &mut app {
            seed_desktop_stream_simulator_transcript(single);
        }
        window.set_title(&app.status_title());
        spawn_desktop_stream_simulator(session_event_tx.clone());
    }
    let reasoning_effort_queue = spawn_desktop_reasoning_effort_request_queue()?;
    let mut recovery_scan_pending = app.is_single_session() && !desktop_gallery;
    let mut first_frame_presented = false;
    let mut first_content_frame_presented = false;
    let mut interaction_latency = DesktopInteractionLatencyProfiler::new();
    let mut no_paint_watchdog = DesktopNoPaintWatchdog::new();
    let mut last_backend_redraw_request: Option<Instant> = None;
    let mut pending_backend_redraw_since: Option<Instant> = None;
    let mut surface_timeout_backoff = SurfaceTimeoutBackoff::default();
    let mut surface_timeout_redraw_at: Option<Instant> = None;
    // Scheduled time for the next animation-driven redraw. Continuous animations
    // re-arm this each presented frame so the loop paces itself to roughly the
    // display refresh rate instead of busy-spinning the main thread.
    let mut animation_redraw_at: Option<Instant> = None;
    let mut pending_resize: Option<PhysicalSize<u32>> = None;
    let mut space_hold_started_at: Option<Instant> = None;
    let mut space_hold_consumed = false;
    let mut github_issue_sync_running = false;
    let mut desktop_clipboard = DesktopClipboard::default();

    if pending_workspace_startup_load {
        spawn_session_cards_load(
            DesktopSessionCardsPurpose::WorkspaceInitialLoad,
            event_loop_proxy.clone(),
            Duration::ZERO,
        );
    }

    let mut event_loop_entered = false;
    event_loop.run(move |event, target| {
        if !event_loop_entered {
            event_loop_entered = true;
            startup_trace.mark("event loop entered");
        }
        let event_loop_now = Instant::now();
        let surface_renderable = desktop_surface_size_is_renderable(window.inner_size());
        let renderer_ready = renderer.is_gpu_ready();
        let has_background_work = app.has_background_work();
        power_inhibitor.set_active(has_background_work);
        let default_wake = desktop_background_wake(
            event_loop_now,
            surface_renderable,
            app.has_frame_animation(),
        );
        let backend_wake = pending_backend_redraw_since
            .and(last_backend_redraw_request)
            .map(|last| last + BACKEND_REDRAW_FRAME_INTERVAL);
        let hot_reload_wake = hot_reloader.next_wake(event_loop_now);
        let space_hold_wake = space_hold_started_at.and_then(|started_at| match &app {
            DesktopApp::Workspace(workspace) if !space_hold_consumed => {
                Some(started_at + workspace.space_hold_toggle_duration())
            }
            _ => None,
        });
        let wake = [
            default_wake,
            backend_wake,
            hot_reload_wake,
            space_hold_wake,
            surface_timeout_redraw_at,
            animation_redraw_at,
        ]
            .into_iter()
            .flatten()
            .min();
        if let Some(wake) = wake {
            target.set_control_flow(ControlFlow::WaitUntil(wake));
        } else {
            target.set_control_flow(ControlFlow::Wait);
        }

        let pending_interaction_kind = interaction_latency.pending_kind();
        let frame_animation_active = app.has_frame_animation();
        let pending_backend_redraw = pending_backend_redraw_since.is_some();
        let no_paint_active = surface_renderable
            && renderer_ready
            && (!first_frame_presented
                || has_background_work
                || frame_animation_active
                || pending_backend_redraw
                || pending_interaction_kind.is_some());
        if no_paint_watchdog.observe_active_tick(
            event_loop_now,
            NoPaintWatchdogContext {
                active: no_paint_active,
                mode: app.mode(),
                has_background_work,
                frame_animation_active,
                pending_backend_redraw,
                pending_interaction_kind,
            },
        ) {
            window.request_redraw();
        }
        let worker_drain = hot_reloader.drain_app_worker_messages();
        if let Some(scene) = worker_drain.latest_scene {
            // Keep receiving worker scenes so the IPC path stays exercised, but do
            // not make them primary yet. The worker currently emits only the
            // display-list skeleton, while the in-process host renderer still owns
            // the complete desktop UI. Rendering the worker scene here regresses
            // normal launches to a blank/gray window.
            drop(scene);
            window.request_redraw();
        }
        if worker_drain.reload_requested {
            show_desktop_reload_notice(&mut app);
            window.set_title(&app.status_title());
            window.request_redraw();
            if hot_reloader.force_reload(&app, &window) {
                target.exit();
                return;
            }
        }

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    pending_resize = Some(size);
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Window(DesktopWindowEvent::Resized {
                            width: size.width,
                            height: size.height,
                            scale_factor: window.scale_factor() as f32,
                        }),
                    );
                    window.request_redraw();
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    pending_resize = Some(window.inner_size());
                    let size = window.inner_size();
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Window(DesktopWindowEvent::Resized {
                            width: size.width,
                            height: size.height,
                            scale_factor: window.scale_factor() as f32,
                        }),
                    );
                    window.request_redraw();
                }
                WindowEvent::Focused(focused) => {
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Window(DesktopWindowEvent::Focused(focused)),
                    );
                }
                WindowEvent::ModifiersChanged(new_modifiers) => {
                    modifiers = new_modifiers.state();
                }
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Mouse(desktop_mouse_wheel_event(delta)),
                    );
                    let size = window.inner_size();
                    let now = Instant::now();
                    let previous_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    let mut should_redraw = false;
                    if !app.is_single_session() {
                        scroll_accumulator.reset();
                        scroll_metrics_cache.clear();
                    } else if let Some(lines) = scroll_accumulator.scroll_lines(delta, now) {
                        should_redraw |=
                            app.scroll_single_session_body(lines, size, &mut scroll_metrics_cache);
                    }
                    if matches!(phase, TouchPhase::Cancelled) {
                        scroll_accumulator.reset();
                    }
                    let next_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    should_redraw |= (next_smooth_scroll - previous_smooth_scroll).abs()
                        >= SCROLL_FRACTIONAL_EPSILON;
                    if should_redraw {
                        interaction_latency.mark("mouse_wheel", now);
                        window.request_redraw();
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let cursor_started = Instant::now();
                    cursor_position = position;
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Mouse(DesktopMouseEvent::Move {
                            x: cursor_position.x as f32,
                            y: cursor_position.y as f32,
                        }),
                    );
                    if selecting_draft
                        && app.update_single_session_draft_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        )
                    {
                        interaction_latency.mark("draft_selection_drag", cursor_started);
                        window.request_redraw();
                    } else if selecting_body
                        && app.update_single_session_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        )
                    {
                        interaction_latency.mark("body_selection_drag", cursor_started);
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    let mouse_started = Instant::now();
                    forward_app_worker_input(
                        &mut hot_reloader,
                        DesktopInputEvent::Mouse(DesktopMouseEvent::Button {
                            button: DesktopMouseButton::Left,
                            pressed: state == ElementState::Pressed,
                        }),
                    );
                    match state {
                        ElementState::Pressed => {
                        if app.begin_single_session_draft_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        ) {
                            selecting_body = false;
                            selecting_draft = true;
                            window.set_title(&app.status_title());
                            interaction_latency.mark("mouse_press", mouse_started);
                            window.request_redraw();
                            return;
                        }

                        selecting_draft = false;
                        selecting_body = app.begin_single_session_selection_at(
                            cursor_position.x as f32,
                            cursor_position.y as f32,
                            window.inner_size(),
                        );
                        if selecting_body {
                            interaction_latency.mark("mouse_press", mouse_started);
                            window.request_redraw();
                        }
                    }
                    ElementState::Released => {
                        if selecting_draft {
                            app.update_single_session_draft_selection_at(
                                cursor_position.x as f32,
                                cursor_position.y as f32,
                                window.inner_size(),
                            );
                            selecting_draft = false;
                            let selected = app.selected_single_session_draft_text();
                            if let Some(text) = selected {
                                copy_text_to_clipboard(
                                    &mut desktop_clipboard,
                                    &text,
                                    "copied input selection",
                                    &mut app,
                                );
                            }
                            window.set_title(&app.status_title());
                            interaction_latency.mark("mouse_release", mouse_started);
                            window.request_redraw();
                        } else if selecting_body {
                            app.update_single_session_selection_at(
                                cursor_position.x as f32,
                                cursor_position.y as f32,
                                window.inner_size(),
                            );
                            selecting_body = false;
                            let selected = app.selected_single_session_text(window.inner_size());
                            if let Some(text) = selected {
                                copy_text_to_clipboard(
                                    &mut desktop_clipboard,
                                    &text,
                                    "copied selection",
                                    &mut app,
                                );
                            }
                            window.set_title(&app.status_title());
                            interaction_latency.mark("mouse_release", mouse_started);
                            window.request_redraw();
                        }
                    }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Released => {
                    if app.is_workspace() && is_space_key(&event.logical_key) {
                        if space_hold_started_at.take().is_some()
                            && !space_hold_consumed
                            && matches!(&app, DesktopApp::Workspace(workspace) if workspace.mode == InputMode::Insert)
                            && matches!(app.handle_key(KeyInput::Character(" ".to_string())), KeyOutcome::Redraw)
                        {
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        space_hold_consumed = false;
                    }
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    let keyboard_started = Instant::now();
                    let size = window.inner_size();
                    let had_smooth_scroll = app
                        .single_session_smooth_scroll_lines(
                            scroll_accumulator.pending_lines(),
                            size,
                            &mut scroll_metrics_cache,
                        )
                        .abs()
                        >= SCROLL_FRACTIONAL_EPSILON;
                    scroll_accumulator.reset();
                    if had_smooth_scroll {
                        window.request_redraw();
                    }
                    if app.is_workspace()
                        && is_space_key(&event.logical_key)
                        && modifiers.is_empty()
                    {
                        if space_hold_started_at.is_none() {
                            space_hold_started_at = Some(keyboard_started);
                            space_hold_consumed = false;
                        }
                        window.request_redraw();
                        return;
                    }

                    let key_input = to_key_input(&event.logical_key, modifiers);
                    let key_debug = format!("{key_input:?}");
                    interaction_latency.mark("keyboard_input", keyboard_started);
                    if hot_reloader.has_app_worker() {
                        forward_app_worker_input(
                            &mut hot_reloader,
                            DesktopInputEvent::Key(desktop_key_event_from_winit(
                                &event.logical_key,
                                modifiers,
                                true,
                            )),
                        );
                        window.request_redraw();
                    }
                    if key_input == KeyInput::RefreshSessions && app.is_workspace() {
                        spawn_session_cards_load(
                            DesktopSessionCardsPurpose::WorkspaceRefresh,
                            event_loop_proxy.clone(),
                            Duration::ZERO,
                        );
                        window.request_redraw();
                        return;
                    }

                    match app.handle_key(key_input) {
                        KeyOutcome::Exit => target.exit(),
                        KeyOutcome::Redraw => {
                            if let DesktopApp::Workspace(workspace) = &app {
                                queue_desktop_preferences_save(workspace, &preferences_save_tx);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::OpenSession { session_id, title } => {
                            if let DesktopApp::Workspace(workspace) = &app {
                                queue_desktop_preferences_save(workspace, &preferences_save_tx);
                            }
                            if app.promote_focused_workspace_session() {
                                scroll_accumulator = ScrollLineAccumulator::default();
                                scroll_metrics_cache = SingleSessionScrollMetricsCache::default();
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            } else if let Err(error) =
                                session_launch::launch_validated_resume_session(&session_id, &title)
                            {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to open session {session_id}: {error:#}"
                                ));
                            }
                        }
                        KeyOutcome::SpawnSession => {
                            if let DesktopApp::SingleSession(app) = &mut app {
                                app.reset_fresh_session();
                                window.set_title(&app.status_title());
                                window.request_redraw();
                                return;
                            }

                            if let Err(error) = session_launch::launch_new_session() {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to spawn session: {error:#}"
                                ));
                            } else {
                                spawn_session_cards_load(
                                    DesktopSessionCardsPurpose::WorkspaceRefresh,
                                    event_loop_proxy.clone(),
                                    SESSION_SPAWN_REFRESH_DELAY,
                                );
                                window.request_redraw();
                            }
                        }
                        KeyOutcome::SpawnSelfDevSession => {
                            if let Err(error) = session_launch::launch_selfdev_session() {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to spawn self-dev session: {error:#}"
                                ));
                            }
                        }
                        KeyOutcome::SpawnHomeSession => {
                            if let Err(error) = session_launch::launch_home_session() {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to spawn home session: {error:#}"
                                ));
                            }
                        }
                        KeyOutcome::SendDraft {
                            session_id,
                            title,
                            message,
                            images,
                        } => {
                            if app.is_single_session() {
                                match session_launch::spawn_message_to_session(
                                    session_id.clone(),
                                    message,
                                    images,
                                    session_event_tx.clone(),
                                ) {
                                    Ok(handle) => app.set_single_session_handle(handle),
                                    Err(error) => apply_single_session_error(&mut app, error),
                                }
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            } else if !images.is_empty() {
                                match session_launch::spawn_message_to_session(
                                    session_id.clone(),
                                    message,
                                    images,
                                    session_event_tx.clone(),
                                ) {
                                    Ok(_handle) => {
                                        spawn_session_cards_load(
                                            DesktopSessionCardsPurpose::WorkspaceRefresh,
                                            event_loop_proxy.clone(),
                                            SESSION_SPAWN_REFRESH_DELAY,
                                        );
                                        window.request_redraw();
                                    }
                                    Err(error) => desktop_log::error(format_args!(
                                        "jcode-desktop: failed to send image draft to {session_id}: {error:#}"
                                    )),
                                }
                            } else if let Err(error) = session_launch::send_message_to_session(
                                &session_id,
                                &title,
                                &message,
                            ) {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to send draft to {session_id}: {error:#}"
                                ));
                            } else {
                                spawn_session_cards_load(
                                    DesktopSessionCardsPurpose::WorkspaceRefresh,
                                    event_loop_proxy.clone(),
                                    SESSION_SPAWN_REFRESH_DELAY,
                                );
                                window.request_redraw();
                            }
                        }
                        KeyOutcome::StartFreshSession { message, images } => {
                            match session_launch::spawn_fresh_server_session(
                                message,
                                images,
                                session_event_tx.clone(),
                            ) {
                                Ok(handle) => app.set_single_session_handle(handle),
                                Err(error) => apply_single_session_error(&mut app, error),
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CancelGeneration => {
                            app.cancel_single_session_generation();
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CopyLatestResponse(text) => {
                            copy_text_to_clipboard(
                                &mut desktop_clipboard,
                                &text,
                                "copied latest response",
                                &mut app,
                            );
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CopyText {
                            text,
                            success_notice,
                        } => {
                            copy_text_to_clipboard(
                                &mut desktop_clipboard,
                                &text,
                                success_notice,
                                &mut app,
                            );
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CutDraftToClipboard(text) => {
                            copy_text_to_clipboard(
                                &mut desktop_clipboard,
                                &text,
                                "cut input line",
                                &mut app,
                            );
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CycleModel(direction) => {
                            if let Err(error) = session_launch::spawn_cycle_model(
                                direction,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::SwitchingModel,
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CycleReasoningEffort(direction) => {
                            let target_session_id = app.single_session_live_id();
                            let outcome = app.preview_single_session_reasoning_effort_cycle(direction);
                            match outcome {
                                ReasoningEffortCycleOutcome::Set(effort) => {
                                    if let Err(error) = reasoning_effort_queue.request(
                                        effort,
                                        target_session_id,
                                        session_event_tx.clone(),
                                    ) {
                                        apply_single_session_error(&mut app, error);
                                    }
                                }
                                ReasoningEffortCycleOutcome::AlreadyAtLimit { .. }
                                | ReasoningEffortCycleOutcome::Unavailable => {}
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::LoadModelCatalog => {
                            if let Err(error) = session_launch::spawn_load_model_catalog(
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::LoadSessionSwitcher => {
                            let purpose = if app.is_workspace() {
                                DesktopSessionCardsPurpose::WorkspaceRefresh
                            } else {
                                DesktopSessionCardsPurpose::SingleSessionSwitcher
                            };
                            spawn_session_cards_load(
                                purpose,
                                event_loop_proxy.clone(),
                                Duration::ZERO,
                            );
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::RestoreCrashedSessions => {
                            spawn_restore_crashed_sessions(event_loop_proxy.clone());
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetModel(model) => {
                            if let Err(error) = session_launch::spawn_set_model(
                                model,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::SwitchingModel,
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::RefreshModelCatalog => {
                            if let Err(error) = session_launch::spawn_refresh_models(
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetReasoningEffort(effort) => {
                            let target_session_id = app.single_session_live_id();
                            match app.preview_single_session_reasoning_effort_set(&effort) {
                                Some(effort) => {
                                    if app
                                        .set_reasoning_effort_via_active_session(effort.clone())
                                        .is_err()
                                        && let Err(error) = reasoning_effort_queue.request(
                                            effort,
                                            target_session_id,
                                            session_event_tx.clone(),
                                        )
                                    {
                                        apply_single_session_error(&mut app, error);
                                    }
                                }
                                None => app.set_single_session_status_label(
                                    "thinking level is not available for this model",
                                ),
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetServiceTier(service_tier) => {
                            if let Err(error) = session_launch::spawn_set_service_tier(
                                service_tier,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("setting fast mode"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetTransport(transport) => {
                            if let Err(error) = session_launch::spawn_set_transport(
                                transport,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("setting transport"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SetCompactionMode(mode) => {
                            if let Err(error) = session_launch::spawn_set_compaction_mode(
                                mode,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("setting compaction mode"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::CompactSession => {
                            if let Err(error) = session_launch::spawn_compact_session(
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("requesting compaction"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::RenameSession(title) => {
                            if let Err(error) = session_launch::spawn_rename_session(
                                title,
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("renaming session"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::ClearServerSession => {
                            if let Err(error) = session_launch::spawn_clear_server_session(
                                app.single_session_live_id(),
                                session_event_tx.clone(),
                            ) {
                                apply_single_session_error(&mut app, error);
                            } else {
                                app.apply_session_event(session_launch::DesktopSessionEvent::Status(
                                    DesktopSessionStatus::external("clearing session"),
                                ));
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::SendStdinResponse { request_id, input } => {
                            if let Err(error) = app.send_single_session_stdin_response(request_id, input)
                            {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::AttachClipboardImage => {
                            match clipboard_image_png_base64(&mut desktop_clipboard) {
                                Ok((media_type, base64_data)) => {
                                    app.attach_clipboard_image(media_type, base64_data);
                                }
                                Err(error) => apply_single_session_error(&mut app, error),
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::PasteText => {
                            if let Err(error) =
                                paste_clipboard_into_app(&mut desktop_clipboard, &mut app)
                            {
                                apply_single_session_error(&mut app, error);
                            }
                            window.set_title(&app.status_title());
                            window.request_redraw();
                        }
                        KeyOutcome::ForceReload => {
                            if hot_reloader.force_reload(&app, &window) {
                                target.exit();
                            } else {
                                window.set_title(&app.status_title());
                                window.request_redraw();
                            }
                        }
                        KeyOutcome::None => {}
                    }
                    if start_pending_github_issue_sync(
                        &mut app,
                        &mut github_issue_sync_running,
                        event_loop_proxy.clone(),
                    ) {
                        window.set_title(&app.status_title());
                        window.request_redraw();
                    }
                    if start_pending_transcript_hydration(&mut app, event_loop_proxy.clone()) {
                        window.request_redraw();
                    }
                    log_desktop_slow_interaction(
                        "keyboard_input",
                        keyboard_started.elapsed(),
                        serde_json::json!({ "key": key_debug }),
                    );
                }
                WindowEvent::RedrawRequested => {
                    let Some(canvas) = renderer.canvas_mut() else {
                        return;
                    };
                    if let Some(size) = pending_resize.take() {
                        canvas.resize(size);
                    }
                    let window_size = window.inner_size();
                    if !desktop_surface_size_is_renderable(window_size) {
                        canvas.suspend_for_zero_size(window_size);
                        surface_timeout_backoff.reset();
                        surface_timeout_redraw_at = None;
                        return;
                    }
                    let smooth_scroll_lines = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        window_size,
                        &mut scroll_metrics_cache,
                    );
                    let render_result = canvas.render(
                        &app,
                        window.current_monitor().map(|monitor| monitor.size()),
                        smooth_scroll_lines,
                        workspace_space_hold_progress(
                            &app,
                            space_hold_started_at,
                            space_hold_consumed,
                        ),
                    );
                    match render_result {
                    Ok(frame) => {
                        surface_timeout_backoff.reset();
                        surface_timeout_redraw_at = None;
                        no_paint_watchdog.observe_presented(Instant::now(), &frame);
                        interaction_latency.observe_presented(&frame);
                        if !first_frame_presented {
                            first_frame_presented = true;
                            startup_trace.mark("first frame presented");
                            if startup_benchmark {
                                target.exit();
                                return;
                            }
                            if recovery_scan_pending {
                                recovery_scan_pending = false;
                                spawn_recovery_session_count_scan(
                                    event_loop_proxy.clone(),
                                    startup_trace,
                                );
                            }
                        }
                        if frame.content_ready && !first_content_frame_presented {
                            first_content_frame_presented = true;
                            startup_trace.mark("first content frame presented");
                        }
                        if startup_content_benchmark && frame.content_ready {
                            target.exit();
                            return;
                        }
                        // Pace continuous animations instead of immediately
                        // re-requesting a redraw. An immediate request makes the
                        // event loop render as fast as the CPU allows (the surface
                        // presents without blocking), pinning the main thread near
                        // 100% CPU and starving input/compositor scheduling. The
                        // scheduled wake is serviced in AboutToWait.
                        animation_redraw_at =
                            next_animation_redraw_at(Instant::now(), frame.animation_active);
                    }
                    Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                        surface_timeout_backoff.reset();
                        surface_timeout_redraw_at = None;
                        canvas.resize(window.inner_size());
                        window.request_redraw();
                    }
                    Err(SurfaceError::OutOfMemory) => target.exit(),
                    Err(SurfaceError::Timeout) => {
                        let now = Instant::now();
                        let (delay, consecutive_timeouts) = surface_timeout_backoff.record_timeout();
                        let redraw_at = now + delay;
                        surface_timeout_redraw_at = Some(redraw_at);
                        if consecutive_timeouts == 1 || delay == SURFACE_TIMEOUT_BACKOFF_MAX {
                            desktop_log::warn(format_args!(
                                "jcode-desktop: surface acquire timed out, retrying in {}ms after {} consecutive timeout(s)",
                                delay.as_millis(),
                                consecutive_timeouts
                            ));
                        }
                        target.set_control_flow(ControlFlow::WaitUntil(redraw_at));
                    }
                    }
                }
                _ => {}
            },
            Event::UserEvent(DesktopUserEvent::RecoveryCount(recovery_count)) => {
                if let DesktopApp::SingleSession(single_session) = &mut app {
                    single_session.set_recovery_session_count(recovery_count);
                    window.set_title(&app.status_title());
                    interaction_latency.mark("recovery_count", Instant::now());
                    window.request_redraw();
                }
            }
            Event::UserEvent(DesktopUserEvent::CanvasReady(result)) => {
                let DesktopCanvasInitResult { canvas, elapsed } = *result;
                match canvas {
                    Ok(mut ready_canvas) => {
                        startup_trace.mark(&format!(
                            "canvas ready (async {}ms)",
                            elapsed.as_millis()
                        ));
                        ready_canvas.resize(window.inner_size());
                        renderer = DesktopHostRendererState::GpuReady(Box::new(ready_canvas));
                        if let Some(handoff) = reload_startup_handoff.as_ref() {
                            handoff.signal_ready_and_wait_for_release();
                            window.set_visible(true);
                            startup_trace.mark("reload handoff released");
                        }
                        reload_startup_handoff = None;
                        window.request_redraw();
                    }
                    Err(message) => {
                        desktop_log::error(format_args!(
                            "jcode-desktop: failed to initialize desktop renderer: {message}"
                        ));
                        renderer = DesktopHostRendererState::GpuFailed { _message: message };
                        target.exit();
                    }
                }
            }
            Event::UserEvent(DesktopUserEvent::SessionCardsLoaded {
                purpose,
                cards,
                loaded_in,
            }) => {
                let card_count = cards.len();
                let mut applied = false;
                match purpose {
                    DesktopSessionCardsPurpose::WorkspaceInitialLoad => {
                        if let DesktopApp::Workspace(workspace) = &mut app {
                            workspace.replace_session_cards(cards);
                            if let Some(preferences) = pending_workspace_startup_preferences.take() {
                                workspace.apply_preferences(preferences);
                            }
                            applied = true;
                        }
                    }
                    DesktopSessionCardsPurpose::WorkspaceRefresh => {
                        if let DesktopApp::Workspace(workspace) = &mut app {
                            workspace.replace_session_cards(cards);
                            queue_desktop_preferences_save(workspace, &preferences_save_tx);
                            applied = true;
                        }
                    }
                    DesktopSessionCardsPurpose::SingleSessionSwitcher => {
                        if app.is_single_session() {
                            app.apply_single_session_switcher_cards(cards);
                            applied = true;
                        }
                    }
                }
                log_desktop_session_cards_load_profile(purpose, loaded_in, card_count, applied);
                if applied {
                    window.set_title(&app.status_title());
                    interaction_latency.mark("session_cards_load", Instant::now());
                    window.request_redraw();
                }
            }
            Event::UserEvent(DesktopUserEvent::SessionCardLoaded {
                session_id,
                card,
                loaded_in,
            }) => {
                let card_found = card.is_some();
                let mut applied = false;
                if let DesktopApp::SingleSession(single_session) = &mut app
                    && single_session.live_session_id.as_deref() == Some(session_id.as_str())
                    && let Some(card) = card
                {
                    single_session.replace_session(Some(card));
                    applied = true;
                }
                log_desktop_session_card_refresh_profile(
                    &session_id,
                    loaded_in,
                    card_found,
                    applied,
                );
                if applied {
                    window.set_title(&app.status_title());
                    interaction_latency.mark("session_card_refresh", Instant::now());
                    window.request_redraw();
                }
            }
            Event::UserEvent(DesktopUserEvent::CrashedSessionsRestoreFinished {
                restored,
                errors,
                elapsed,
            }) => {
                log_desktop_crashed_sessions_restore_profile(restored, errors.len(), elapsed);
                if restored == 0 {
                    let message = if errors.is_empty() {
                        "no crashed sessions found".to_string()
                    } else {
                        format!("failed to restore crashed sessions: {}", errors.join("; "))
                    };
                    apply_single_session_error(&mut app, anyhow::anyhow!(message));
                } else if let DesktopApp::SingleSession(single_session) = &mut app {
                    single_session.set_recovery_session_count(0);
                    single_session.set_status_label(format!("restored {restored} crashed session(s)"));
                }
                window.set_title(&app.status_title());
                interaction_latency.mark("restore_crashed_sessions", Instant::now());
                window.request_redraw();
            }
            Event::UserEvent(DesktopUserEvent::GitHubIssuesSyncFinished(result)) => {
                github_issue_sync_running = false;
                app.apply_github_issue_sync_result(result);
                window.set_title(&app.status_title());
                interaction_latency.mark("github_issue_sync", Instant::now());
                window.request_redraw();
            }
            Event::UserEvent(DesktopUserEvent::TranscriptHydrated {
                session_id,
                result,
                loaded_in,
            }) => {
                if app.apply_hydrated_transcript(&session_id, result) {
                    desktop_log::info(format_args!(
                        "jcode-desktop: hydrated resumed transcript for {session_id} in {}ms",
                        loaded_in.as_millis()
                    ));
                    window.set_title(&app.status_title());
                    interaction_latency.mark("transcript_hydration", Instant::now());
                    window.request_redraw();
                }
            }
            Event::UserEvent(DesktopUserEvent::SessionEvents(batch)) => {
                let ui_received_at = Instant::now();
                let accumulated_for = batch.accumulated_for();
                let raw_event_count = batch.raw_event_count;
                let raw_payload_bytes = batch.raw_payload_bytes;
                let forwarded_at = batch.forwarded_at;
                forward_desktop_session_event_batch_to_worker(&mut hot_reloader, &batch);
                let apply_stats = apply_desktop_session_event_batch_with_stats(&mut app, batch.events);
                let ui_queue_delay = ui_received_at.saturating_duration_since(forwarded_at);
                let mut redraw_requested = false;
                let mut redraw_deferred = false;
                let mut session_card_refresh_spawned = false;
                if apply_stats.visible_changed {
                    let now = Instant::now();
                    if apply_stats.session_card_refresh_requested
                        && let Some(session_id) = app.single_session_live_id()
                    {
                        spawn_single_session_card_refresh(session_id, event_loop_proxy.clone());
                        session_card_refresh_spawned = true;
                    }
                    if let Some((message, images)) = app.take_next_queued_single_session_draft() {
                        let result = if let Some(session_id) = app.single_session_live_id() {
                            session_launch::spawn_message_to_session(
                                session_id,
                                message,
                                images,
                                session_event_tx.clone(),
                            )
                        } else {
                            session_launch::spawn_fresh_server_session(
                                message,
                                images,
                                session_event_tx.clone(),
                            )
                        };
                        match result {
                            Ok(handle) => app.set_single_session_handle(handle),
                            Err(error) => apply_single_session_error(&mut app, error),
                        }
                    }
                    window.set_title(&app.status_title());
                    let redraw_due = last_backend_redraw_request.is_none_or(|last| {
                        now.saturating_duration_since(last) >= BACKEND_REDRAW_FRAME_INTERVAL
                    });
                    if redraw_due {
                        let first_pending = pending_backend_redraw_since.take().unwrap_or(now);
                        interaction_latency.mark("backend_events", first_pending);
                        last_backend_redraw_request = Some(now);
                        window.request_redraw();
                        redraw_requested = true;
                    } else {
                        pending_backend_redraw_since.get_or_insert(now);
                        redraw_deferred = true;
                    }
                }
                log_desktop_session_event_batch_profile(
                    raw_event_count,
                    raw_payload_bytes,
                    accumulated_for,
                    ui_queue_delay,
                    &apply_stats,
                    redraw_requested,
                    redraw_deferred,
                    session_card_refresh_spawned,
                );
            }
            Event::AboutToWait => {
                let surface_renderable = desktop_surface_size_is_renderable(window.inner_size());
                if let Some(redraw_at) = surface_timeout_redraw_at {
                    let now = Instant::now();
                    if now >= redraw_at {
                        surface_timeout_redraw_at = None;
                        if surface_renderable {
                            window.request_redraw();
                        }
                    }
                }
                // Service the paced animation redraw scheduled by RedrawRequested.
                // This keeps continuous animations advancing at ~display refresh
                // without busy-spinning the loop between frames.
                if let Some(redraw_at) = animation_redraw_at {
                    let now = Instant::now();
                    if now >= redraw_at {
                        animation_redraw_at = None;
                        if surface_renderable {
                            window.request_redraw();
                        }
                    }
                }
                if surface_renderable && app.is_single_session() {
                    let about_to_wait_started = Instant::now();
                    let size = window.inner_size();
                    let previous_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    let frame = scroll_accumulator.frame(Instant::now());
                    if let Some(lines) = frame.scroll_lines
                        && !app.scroll_single_session_body(lines, size, &mut scroll_metrics_cache)
                    {
                        scroll_accumulator.stop();
                    }
                    let next_smooth_scroll = app.single_session_smooth_scroll_lines(
                        scroll_accumulator.pending_lines(),
                        size,
                        &mut scroll_metrics_cache,
                    );
                    if frame.active
                        || (next_smooth_scroll - previous_smooth_scroll).abs()
                            >= SCROLL_FRACTIONAL_EPSILON
                    {
                        interaction_latency.mark("scroll_momentum", about_to_wait_started);
                        window.request_redraw();
                    }
                } else if scroll_accumulator.is_active() {
                    scroll_accumulator.reset();
                    scroll_metrics_cache.clear();
                }
                if let (DesktopApp::Workspace(workspace), Some(started_at)) = (&mut app, space_hold_started_at)
                    && !space_hold_consumed
                {
                    let now = Instant::now();
                    if now.saturating_duration_since(started_at) >= workspace.space_hold_toggle_duration() {
                        space_hold_consumed = true;
                        if matches!(workspace.handle_key(KeyInput::ToggleInputMode), KeyOutcome::Redraw) {
                            window.set_title(&app.status_title());
                        }
                    }
                    if surface_renderable {
                        window.request_redraw();
                    }
                }
                if let Some(first_pending_backend_redraw) = pending_backend_redraw_since {
                    let now = Instant::now();
                    if surface_renderable
                        && last_backend_redraw_request.is_none_or(|last| {
                            now.saturating_duration_since(last) >= BACKEND_REDRAW_FRAME_INTERVAL
                        })
                    {
                        pending_backend_redraw_since = None;
                        interaction_latency.mark("backend_events", first_pending_backend_redraw);
                        last_backend_redraw_request = Some(now);
                        window.request_redraw();
                    }
                }
                if hot_reloader.poll(&app, &window) {
                    target.exit();
                    return;
                }

                if let Some(canvas) = renderer.canvas_mut()
                    && surface_renderable
                    && canvas.needs_initial_frame
                {
                    canvas.needs_initial_frame = false;
                    window.request_redraw();
                } else if surface_renderable
                    && app.has_frame_animation()
                    && animation_redraw_at.is_none()
                {
                    // An animation is active but no paced redraw is scheduled yet
                    // (e.g. it just became active). Schedule one instead of
                    // requesting a redraw on every loop iteration, which would
                    // busy-spin the main thread at 100% CPU.
                    animation_redraw_at = next_animation_redraw_at(Instant::now(), true);
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}

fn load_session_cards_for_desktop() -> Vec<workspace::SessionCard> {
    match session_data::load_recent_session_cards() {
        Ok(cards) => cards,
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to load session metadata: {error:#}"
            ));
            Vec::new()
        }
    }
}

fn load_crashed_session_cards_for_desktop() -> Vec<workspace::SessionCard> {
    match session_data::load_crashed_session_cards() {
        Ok(cards) => cards,
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to load crashed session metadata: {error:#}"
            ));
            Vec::new()
        }
    }
}

fn spawn_recovery_session_count_scan(
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
    startup_trace: DesktopStartupTrace,
) {
    if let Err(error) = spawn_bounded_desktop_async_job("jcode-desktop-recovery-scan", move || {
        startup_trace.mark("recovery scan started");
        let recovery_count = load_crashed_session_cards_for_desktop().len();
        startup_trace.mark(&format!(
            "recovery scan completed ({recovery_count} crashed)"
        ));
        if event_loop_proxy
            .send_event(DesktopUserEvent::RecoveryCount(recovery_count))
            .is_err()
        {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to deliver recovery count, event loop is closed"
            ));
        }
    }) {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to start recovery scan: {error:#}"
        ));
    }
}

fn spawn_single_session_card_refresh(
    session_id: String,
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
) {
    if let Err(error) =
        spawn_bounded_desktop_async_job("jcode-desktop-session-card-refresh", move || {
            let started = Instant::now();
            let card = load_session_cards_for_desktop()
                .into_iter()
                .find(|card| card.session_id == session_id);
            let loaded_in = started.elapsed();
            if event_loop_proxy
                .send_event(DesktopUserEvent::SessionCardLoaded {
                    session_id,
                    card,
                    loaded_in,
                })
                .is_err()
            {
                desktop_log::warn(format_args!(
                    "jcode-desktop: failed to deliver session card refresh, event loop is closed"
                ));
            }
        })
    {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to start session card refresh: {error:#}"
        ));
    }
}

fn spawn_session_cards_load(
    purpose: DesktopSessionCardsPurpose,
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
    delay: Duration,
) {
    if let Err(error) = spawn_bounded_desktop_async_job(
        format!("jcode-desktop-session-cards-{purpose:?}"),
        move || {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let started = Instant::now();
            let cards = load_session_cards_for_desktop();
            let loaded_in = started.elapsed();
            if event_loop_proxy
                .send_event(DesktopUserEvent::SessionCardsLoaded {
                    purpose,
                    cards,
                    loaded_in,
                })
                .is_err()
            {
                desktop_log::warn(format_args!(
                    "jcode-desktop: failed to deliver session cards load, event loop is closed"
                ));
            }
        },
    ) {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to start session card load: {error:#}"
        ));
    }
}

fn spawn_restore_crashed_sessions(event_loop_proxy: EventLoopProxy<DesktopUserEvent>) {
    if let Err(error) = spawn_bounded_desktop_async_job(
        "jcode-desktop-restore-crashed-sessions",
        move || {
            let started = Instant::now();
            let crashed = load_crashed_session_cards_for_desktop();
            let mut restored = 0usize;
            let mut errors = Vec::new();
            for card in crashed {
                match session_launch::launch_validated_resume_session(&card.session_id, &card.title)
                {
                    Ok(()) => restored += 1,
                    Err(error) => errors.push(format!("{}: {error:#}", card.session_id)),
                }
            }
            if event_loop_proxy
                .send_event(DesktopUserEvent::CrashedSessionsRestoreFinished {
                    restored,
                    errors,
                    elapsed: started.elapsed(),
                })
                .is_err()
            {
                desktop_log::warn(format_args!(
                    "jcode-desktop: failed to deliver crashed-session restore result, event loop is closed"
                ));
            }
        },
    ) {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to start crashed-session restore: {error:#}"
        ));
    }
}

fn spawn_github_issue_sync(event_loop_proxy: EventLoopProxy<DesktopUserEvent>) -> Result<()> {
    spawn_bounded_desktop_async_job("jcode-desktop-github-issues-sync", move || {
        let result = desktop_issue_cache::sync_current_repo_issue_cache()
            .map_err(|error| format!("{error:#}"));
        match &result {
            Ok(summary) => desktop_log::info(format_args!(
                "jcode-desktop: synced {} GitHub issue(s) for {} in {}ms to {} (comment_threads={} comment_errors={})",
                summary.issue_count,
                summary.repo,
                summary.elapsed.as_millis(),
                summary.cache_path.display(),
                summary.fetched_comment_threads,
                summary.comment_fetch_errors
            )),
            Err(error) => desktop_log::warn(format_args!(
                "jcode-desktop: GitHub issue sync failed: {error}"
            )),
        }
        if event_loop_proxy
            .send_event(DesktopUserEvent::GitHubIssuesSyncFinished(result))
            .is_err()
        {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to deliver GitHub issue sync result"
            ));
        }
    })
}

fn start_pending_github_issue_sync(
    app: &mut DesktopApp,
    sync_running: &mut bool,
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
) -> bool {
    if !app.take_github_issue_sync_request() {
        return false;
    }
    if *sync_running {
        app.note_github_issue_sync_already_running();
        return true;
    }
    match spawn_github_issue_sync(event_loop_proxy) {
        Ok(()) => {
            *sync_running = true;
            true
        }
        Err(error) => {
            app.apply_github_issue_sync_result(Err(format!("{error:#}")));
            true
        }
    }
}

/// Start an off-thread transcript load for a session resumed from the
/// switcher (or a promoted workspace card). The result is delivered back to
/// the event loop as `DesktopUserEvent::TranscriptHydrated`, so large
/// transcript parses never stall key handling. Falls back to a synchronous
/// load if the job slot or thread spawn fails.
fn start_pending_transcript_hydration(
    app: &mut DesktopApp,
    event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
) -> bool {
    let Some(session_id) = app.take_pending_transcript_hydration() else {
        return false;
    };
    let job_session_id = session_id.clone();
    let spawned =
        spawn_bounded_desktop_async_job("jcode-desktop-transcript-hydration", move || {
            let started = Instant::now();
            let result = session_data::load_session_transcript_by_id(&job_session_id)
                .map_err(|error| format!("{error:#}"));
            if event_loop_proxy
                .send_event(DesktopUserEvent::TranscriptHydrated {
                    session_id: job_session_id,
                    result,
                    loaded_in: started.elapsed(),
                })
                .is_err()
            {
                desktop_log::warn(format_args!(
                    "jcode-desktop: failed to deliver hydrated transcript"
                ));
            }
        });
    if let Err(error) = spawned {
        desktop_log::warn(format_args!(
            "jcode-desktop: transcript hydration fell back to blocking load: {error:#}"
        ));
        let result = session_data::load_session_transcript_by_id(&session_id)
            .map_err(|error| format!("{error:#}"));
        app.apply_hydrated_transcript(&session_id, result);
    }
    true
}

fn spawn_desktop_preferences_saver() -> Option<mpsc::Sender<workspace::DesktopPreferences>> {
    let (tx, rx) = mpsc::channel::<workspace::DesktopPreferences>();
    match std::thread::Builder::new()
        .name("jcode-desktop-preferences-saver".to_string())
        .spawn(move || {
            while let Ok(mut preferences) = rx.recv() {
                let received_at = Instant::now();
                let mut coalesced_saves = 1usize;
                while let Ok(next_preferences) = rx.try_recv() {
                    preferences = next_preferences;
                    coalesced_saves += 1;
                }
                save_desktop_preferences_off_ui_thread(
                    preferences,
                    coalesced_saves,
                    received_at.elapsed(),
                );
            }
        }) {
        Ok(_) => Some(tx),
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to start preferences saver: {error:#}"
            ));
            None
        }
    }
}

fn queue_desktop_preferences_save(
    workspace: &Workspace,
    preferences_save_tx: &Option<mpsc::Sender<workspace::DesktopPreferences>>,
) {
    let preferences = workspace.preferences();
    if let Some(tx) = preferences_save_tx
        && tx.send(preferences.clone()).is_ok()
    {
        return;
    }

    if let Err(error) =
        spawn_bounded_desktop_async_job("jcode-desktop-preferences-save-once", move || {
            save_desktop_preferences_off_ui_thread(preferences, 1, Duration::ZERO);
        })
    {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to queue preferences save: {error:#}"
        ));
    }
}

fn save_desktop_preferences_off_ui_thread(
    preferences: workspace::DesktopPreferences,
    coalesced_saves: usize,
    queued_for: Duration,
) {
    let started = Instant::now();
    let error = desktop_prefs::save_preferences(&preferences)
        .err()
        .map(|error| format!("{error:#}"));
    log_desktop_preferences_save_profile(
        started.elapsed(),
        queued_for,
        coalesced_saves,
        error.as_deref(),
    );
}

fn headless_chat_smoke_message(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--headless-chat-smoke=")
            .map(ToOwned::to_owned)
            .or_else(|| {
                (arg == "--headless-chat-smoke")
                    .then(|| args.get(index + 1).cloned())
                    .flatten()
            })
    })
}

/// Dev-only flag: `--simulate-stream` drives the live single-session app with
/// synthetic streaming deltas so the streaming reveal animation can be observed
/// and recorded without a real backend.
fn simulate_stream_requested(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--simulate-stream" || arg == "--simulate-streaming")
}

const DESKTOP_STREAM_SIMULATOR_SCRIPT: &str = "Sure, let me walk through how the streaming text reveal works in the desktop app. \
When the provider sends tokens, they arrive in bursty chunks rather than a smooth flow, \
so the renderer keeps a `revealed_chars` cursor that eases toward the full response length. \
The trailing characters get a per-character alpha ramp called the tail fade, \
and a soft breathing cursor sits at the very end of the revealed text to signal activity.\n\n\
Here is a short list of the moving parts:\n\
- The reveal motion integrates a rate proportional to the backlog.\n\
- The body text buffer is rebuilt as the reveal advances.\n\
- A separate overlay buffer paints the streaming tail with its own opacity.\n\n\
Once the response finishes, the overlay hands off to the committed transcript message. \
That handoff should be seamless, with no visible jump or flicker as the text settles into place. \
This paragraph is intentionally long so the streaming text wraps across many lines and the \
viewport scrolls while new tokens keep arriving at the bottom of the transcript.";

/// Seed a small prior transcript so the simulated stream appends after existing
/// messages, mirroring the common case of streaming inside an active session.
fn seed_desktop_stream_simulator_transcript(app: &mut SingleSessionApp) {
    app.replace_session(Some(workspace::SessionCard {
        session_id: "simulate-stream".to_string(),
        title: "Streaming simulation".to_string(),
        subtitle: "dev stream harness".to_string(),
        detail: "fixture".to_string(),
        preview_lines: Vec::new(),
        detail_lines: Vec::new(),
        transcript_messages: Vec::new(),
    }));
    app.messages.push(SingleSessionMessage::user(
        "Explain how the desktop streaming text reveal works.",
    ));
    app.messages.push(SingleSessionMessage::assistant(
        "Earlier reply: the desktop renders streamed assistant text with an adaptive reveal so bursty provider chunks flow in smoothly instead of popping.",
    ));
    app.scroll_body_to_bottom();
}

/// Spawn a background thread that emits synthetic streaming events to exercise
/// the real desktop streaming animation pipeline.
fn spawn_desktop_stream_simulator(
    session_event_tx: mpsc::Sender<session_launch::DesktopSessionEvent>,
) {
    std::thread::Builder::new()
        .name("jcode-desktop-stream-simulator".to_string())
        .spawn(move || {
            // Give the window a moment to come up before streaming starts.
            std::thread::sleep(Duration::from_millis(900));
            if session_event_tx
                .send(session_launch::DesktopSessionEvent::SessionStarted {
                    session_id: "simulate-stream".to_string(),
                })
                .is_err()
            {
                return;
            }
            // Emit word-sized deltas, occasionally bursting several words at once
            // to mimic real provider chunking, with brief stalls between bursts.
            let words: Vec<&str> = DESKTOP_STREAM_SIMULATOR_SCRIPT
                .split_inclusive(' ')
                .collect();
            let mut index = 0usize;
            let mut burst_phase = 0usize;
            while index < words.len() {
                let burst = match burst_phase % 4 {
                    0 => 1,
                    1 => 3,
                    2 => 2,
                    _ => 5,
                };
                burst_phase += 1;
                let end = (index + burst).min(words.len());
                let chunk: String = words[index..end].concat();
                index = end;
                if session_event_tx
                    .send(session_launch::DesktopSessionEvent::TextDelta(chunk))
                    .is_err()
                {
                    return;
                }
                let pause = match burst_phase % 5 {
                    0 => Duration::from_millis(220),
                    3 => Duration::from_millis(120),
                    _ => Duration::from_millis(45),
                };
                std::thread::sleep(pause);
            }
            std::thread::sleep(Duration::from_millis(400));
            let _ = session_event_tx.send(session_launch::DesktopSessionEvent::Done);
        })
        .ok();
}

const DESKTOP_HELP_LINES: &[&str] = &[
    "Jcode Desktop",
    "",
    "Usage:",
    "  jcode-desktop [OPTIONS]",
    "",
    "Options:",
    "  --fullscreen                 Start borderless fullscreen",
    "  --workspace                  Open the workspace prototype instead of the single-session chat",
    "  --desktop-process-role ROLE  Internal: standalone, host, or worker",
    "  --desktop-host               Internal alias for --desktop-process-role=host",
    "  --desktop-app-worker         Internal alias for --desktop-process-role=worker",
    "  --startup-log                Print launch timing milestones to stderr",
    "  --startup-benchmark          Print launch timings and exit after the first frame",
    "  --capture-hero-animation DIR Write deterministic hero animation PNG frames and exit",
    "  --capture-gallery-screens DIR Render gallery fixture states to PNGs headlessly and exit",
    "  --capture-keys KEYS          With --capture-gallery-screens: comma-separated keys to replay first",
    "  --capture-size WxH           With --capture-gallery-screens: render size in pixels",
    "  --resize-render-benchmark[N]  Print CPU resize/render benchmark JSON and exit",
    "  --scroll-render-benchmark[N]  Print CPU scroll/render benchmark JSON and exit",
    "  --real-transcript-scroll-benchmark[N]  Profile scrolling against your real on-disk transcripts and exit",
    "  --real-transcript-action-benchmark[N]  Profile mixed user actions (scroll/resize/typing/pickers/selection/streaming) on real transcripts and exit",
    "  --stream-e2e-benchmark[N]     Print stream event-to-paint guardrail JSON and exit",
    "  --headless-chat-smoke <MSG>  Run a hidden backend smoke test and print JSON events",
    "  --headless-chat-smoke=<MSG>  Same as above",
    "  -V, --version                Print version information",
    "  -h, --help                   Print this help",
    "",
];

fn desktop_help_text() -> String {
    DESKTOP_HELP_LINES.join("\n")
}


/// Request for a headless gallery screenshot capture.
///
/// `--capture-gallery-screens DIR` renders every gallery fixture state to a
/// PNG in DIR without opening a window. `--gallery-state STATE` (optional)
/// restricts the capture to a single state, and `--capture-keys KEYSPEC`
/// (optional) replays comma-separated key names against each state before
/// rendering, so arbitrary interaction states can be inspected visually.
struct GalleryScreenshotCaptureRequest {
    output_dir: PathBuf,
    state: Option<String>,
    keys: Vec<String>,
    size: Option<PhysicalSize<u32>>,
}

fn gallery_screenshot_capture_request(args: &[String]) -> Option<GalleryScreenshotCaptureRequest> {
    let output_dir = args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--capture-gallery-screens=")
            .map(PathBuf::from)
            .or_else(|| {
                (arg == "--capture-gallery-screens")
                    .then(|| args.get(index + 1).map(PathBuf::from))
                    .flatten()
            })
    })?;
    let keys = args
        .iter()
        .enumerate()
        .find_map(|(index, arg)| {
            arg.strip_prefix("--capture-keys=")
                .map(str::to_string)
                .or_else(|| {
                    (arg == "--capture-keys")
                        .then(|| args.get(index + 1).cloned())
                        .flatten()
                })
        })
        .map(|spec| {
            spec.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let size = args
        .iter()
        .enumerate()
        .find_map(|(index, arg)| {
            arg.strip_prefix("--capture-size=")
                .map(str::to_string)
                .or_else(|| {
                    (arg == "--capture-size")
                        .then(|| args.get(index + 1).cloned())
                        .flatten()
                })
        })
        .and_then(|spec| {
            let (width, height) = spec.split_once('x')?;
            Some(PhysicalSize::new(
                width.trim().parse().ok()?,
                height.trim().parse().ok()?,
            ))
        });
    Some(GalleryScreenshotCaptureRequest {
        output_dir,
        state: desktop_gallery::state_from_args(args),
        keys,
        size,
    })
}

/// Parse a key name from `--capture-keys` into a `KeyInput`.
fn capture_key_input(name: &str) -> Option<KeyInput> {
    Some(match name {
        "escape" => KeyInput::Escape,
        "enter" => KeyInput::Enter,
        "backspace" => KeyInput::Backspace,
        "tab" => KeyInput::Autocomplete,
        "submit" => KeyInput::SubmitDraft,
        "model-picker" => KeyInput::OpenModelPicker,
        "session-switcher" => KeyInput::OpenSessionSwitcher,
        "hotkey-help" => KeyInput::HotkeyHelp,
        "session-info" => KeyInput::ToggleSessionInfo,
        "scroll-up" => KeyInput::ScrollBodyLines(-3),
        "scroll-down" => KeyInput::ScrollBodyLines(3),
        "scroll-top" => KeyInput::ScrollBodyToTop,
        "scroll-bottom" => KeyInput::ScrollBodyToBottom,
        "page-up" => KeyInput::ScrollBodyPages(-1),
        "page-down" => KeyInput::ScrollBodyPages(1),
        "text-bigger" => KeyInput::AdjustTextScale(1),
        "text-smaller" => KeyInput::AdjustTextScale(-1),
        other => {
            let text = other.strip_prefix("char:")?;
            KeyInput::Character(text.to_string())
        }
    })
}

async fn run_gallery_screenshot_capture(request: &GalleryScreenshotCaptureRequest) -> Result<()> {
    std::fs::create_dir_all(&request.output_dir).with_context(|| {
        format!(
            "failed to create gallery screenshot directory {}",
            request.output_dir.display()
        )
    })?;
    let states: Vec<String> = match &request.state {
        Some(state) => vec![state.clone()],
        None => desktop_gallery::gallery_states()
            .iter()
            .map(|state| state.to_string())
            .collect(),
    };
    let keys = request
        .keys
        .iter()
        .map(|name| {
            capture_key_input(name).with_context(|| format!("unknown capture key name {name:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let size = request.size.unwrap_or_else(|| {
        PhysicalSize::new(DEFAULT_WINDOW_WIDTH as u32, DEFAULT_WINDOW_HEIGHT as u32)
    });
    let mut manifest = Vec::new();
    for state in &states {
        let mut app = desktop_gallery::temporary_app(state);
        for key in &keys {
            app.handle_key(key.clone());
        }
        let DesktopApp::SingleSession(single) = &mut app else {
            anyhow::bail!("gallery screenshot capture only supports single-session states");
        };
        single.settle_animations_for_capture();
        let single = &*single;
        let rendered_lines = single_session_rendered_body_lines_for_tick(single, size, 4);
        let widget_geometry =
            inline_widget_capture_geometry(single, size, rendered_lines.len()).map(
                |(card, text_top, line_height, visible_text_bottom, visible_text_right)| {
                    serde_json::json!({
                        "card": { "x": card.x, "y": card.y, "width": card.width, "height": card.height },
                        "text_top": text_top,
                        "line_height": line_height,
                        "visible_text_bottom": visible_text_bottom,
                        "visible_text_right": visible_text_right,
                    })
                },
            );
        let (image, vertices) = render_hero_frame_to_image(single, size, 4, 1.0, false).await?;
        let filename = if request.keys.is_empty() {
            format!("gallery-{state}.png")
        } else {
            let key_part = request
                .keys
                .join("+")
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | ':') {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            format!("gallery-{state}+{key_part}.png")
        };
        let path = request.output_dir.join(&filename);
        image
            .save(&path)
            .with_context(|| format!("failed to save {}", path.display()))?;
        manifest.push(serde_json::json!({
            "state": state,
            "file": filename,
            "keys": request.keys,
            "vertices": vertices,
            "inline_widget": widget_geometry,
            "snapshot": serde_json::to_value(app.snapshot())?,
        }));
    }
    println!(
        "{}",
        serde_json::json!({
            "output_dir": request.output_dir,
            "screens": manifest,
        })
    );
    Ok(())
}


enum DesktopUserEvent {
    CanvasReady(Box<DesktopCanvasInitResult>),
    SessionEvents(DesktopSessionEventBatch),
    SessionCardsLoaded {
        purpose: DesktopSessionCardsPurpose,
        cards: Vec<workspace::SessionCard>,
        loaded_in: Duration,
    },
    SessionCardLoaded {
        session_id: String,
        card: Option<workspace::SessionCard>,
        loaded_in: Duration,
    },
    CrashedSessionsRestoreFinished {
        restored: usize,
        errors: Vec<String>,
        elapsed: Duration,
    },
    GitHubIssuesSyncFinished(
        std::result::Result<desktop_issue_cache::GitHubIssueSyncSummary, String>,
    ),
    TranscriptHydrated {
        session_id: String,
        result: std::result::Result<Option<Vec<workspace::SessionTranscriptMessage>>, String>,
        loaded_in: Duration,
    },
    RecoveryCount(usize),
}

struct DesktopCanvasInitResult {
    canvas: std::result::Result<Canvas, String>,
    elapsed: Duration,
}

enum DesktopHostRendererState {
    NoGpuBoot,
    GpuInitializing { _started_at: Instant },
    GpuReady(Box<Canvas>),
    GpuFailed { _message: String },
}

impl DesktopHostRendererState {
    fn start_gpu_init(
        &mut self,
        window: Arc<Window>,
        event_loop_proxy: EventLoopProxy<DesktopUserEvent>,
        startup_trace: DesktopStartupTrace,
    ) -> Result<()> {
        if matches!(self, Self::GpuInitializing { .. } | Self::GpuReady(_)) {
            return Ok(());
        }

        let started_at = Instant::now();
        std::thread::Builder::new()
            .name("jcode-desktop-gpu-init".to_string())
            .spawn(move || {
                startup_trace.mark("canvas init started");
                let canvas = pollster::block_on(Canvas::new(window, startup_trace))
                    .map_err(|error| format!("{error:#}"));
                let result = DesktopCanvasInitResult {
                    canvas,
                    elapsed: started_at.elapsed(),
                };
                if event_loop_proxy
                    .send_event(DesktopUserEvent::CanvasReady(Box::new(result)))
                    .is_err()
                {
                    desktop_log::warn(format_args!(
                        "jcode-desktop: failed to deliver async canvas initialization result"
                    ));
                }
            })
            .context("failed to spawn desktop GPU initialization thread")?;
        *self = Self::GpuInitializing {
            _started_at: started_at,
        };
        Ok(())
    }

    fn is_gpu_ready(&self) -> bool {
        matches!(self, Self::GpuReady(_))
    }

    fn canvas_mut(&mut self) -> Option<&mut Canvas> {
        match self {
            Self::GpuReady(canvas) => Some(canvas.as_mut()),
            Self::NoGpuBoot | Self::GpuInitializing { .. } | Self::GpuFailed { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopSessionCardsPurpose {
    WorkspaceInitialLoad,
    WorkspaceRefresh,
    SingleSessionSwitcher,
}

fn run_headless_chat_smoke(message: String) -> Result<()> {
    if message.trim().is_empty() {
        anyhow::bail!("headless chat smoke message cannot be empty");
    }

    let (event_tx, event_rx) = mpsc::channel();
    let _handle = session_launch::spawn_fresh_server_session(message, Vec::new(), event_tx)
        .context("failed to start desktop headless chat smoke")?;
    let started = Instant::now();
    let mut session_id = None;
    let mut response = String::new();
    let mut last_status = None;

    while started.elapsed() < HEADLESS_CHAT_SMOKE_TIMEOUT {
        let remaining = HEADLESS_CHAT_SMOKE_TIMEOUT.saturating_sub(started.elapsed());
        let poll = remaining.min(Duration::from_millis(250));
        let event = match event_rx.recv_timeout(poll) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!(
                    "desktop chat smoke worker disconnected before completion; last_status={}",
                    last_status.as_deref().unwrap_or("unknown")
                );
            }
        };

        match event {
            session_launch::DesktopSessionEvent::Status(status) => {
                let status = status.label();
                last_status = Some(status.clone());
                println!(
                    "{}",
                    serde_json::json!({"event": "status", "status": status})
                );
            }
            session_launch::DesktopSessionEvent::SessionStarted { session_id: id } => {
                session_id = Some(id.clone());
                println!(
                    "{}",
                    serde_json::json!({"event": "session", "session_id": id})
                );
            }
            session_launch::DesktopSessionEvent::SessionRenamed {
                title,
                display_title,
            } => {
                last_status = Some(if title.is_some() {
                    format!("renamed session to {display_title}")
                } else {
                    format!("cleared session name; title is now {display_title}")
                });
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "session_renamed",
                        "title": title,
                        "display_title": display_title,
                    })
                );
            }
            session_launch::DesktopSessionEvent::Reloaded { session_id: id } => {
                session_id = Some(id.clone());
                last_status = Some("server reconnected".to_string());
                println!(
                    "{}",
                    serde_json::json!({"event": "reloaded", "session_id": id})
                );
            }
            session_launch::DesktopSessionEvent::TextDelta(text) => {
                response.push_str(&text);
                println!(
                    "{}",
                    serde_json::json!({"event": "text_delta", "chars": text.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::TextReplace(text) => {
                response = text;
                println!(
                    "{}",
                    serde_json::json!({"event": "text_replace", "chars": response.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::ToolStarted { id, name } => {
                last_status = Some(format!("preparing tool {name}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_started", "id": id, "name": name})
                );
            }
            session_launch::DesktopSessionEvent::ToolExecuting { id, name } => {
                last_status = Some(format!("using tool {name}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_executing", "id": id, "name": name})
                );
            }
            session_launch::DesktopSessionEvent::ToolInput { id, delta } => {
                println!(
                    "{}",
                    serde_json::json!({"event": "tool_input", "id": id, "chars": delta.chars().count()})
                );
            }
            session_launch::DesktopSessionEvent::ToolFinished {
                id,
                name,
                summary,
                is_error,
            } => {
                last_status = Some(if is_error {
                    format!("tool {name} failed")
                } else {
                    format!("tool {name} done")
                });
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "tool_finished",
                        "id": id,
                        "name": name,
                        "summary": summary,
                        "is_error": is_error,
                    })
                );
            }
            session_launch::DesktopSessionEvent::Reloading { new_socket } => {
                last_status = Some("server reloading, reconnecting".to_string());
                println!(
                    "{}",
                    serde_json::json!({"event": "reloading", "new_socket": new_socket})
                );
            }
            session_launch::DesktopSessionEvent::ModelChanged {
                model,
                provider_name,
                error,
            } => {
                if let Some(error) = error {
                    last_status = Some(format!("model switch failed: {error}"));
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "model_changed",
                            "model": model,
                            "provider_name": provider_name,
                            "error": error,
                        })
                    );
                    continue;
                }
                let label = provider_name
                    .as_deref()
                    .map(|provider| format!("{provider} · {model}"))
                    .unwrap_or_else(|| model.clone());
                last_status = Some(format!("model: {label}"));
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "model_changed",
                        "model": model,
                        "provider_name": provider_name,
                    })
                );
            }
            session_launch::DesktopSessionEvent::ModelCatalog {
                current_model,
                provider_name,
                models,
                ..
            } => {
                last_status = Some(format!("models loaded ({})", models.len()));
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "model_catalog",
                        "current_model": current_model,
                        "provider_name": provider_name,
                        "models": models.len(),
                    })
                );
            }
            session_launch::DesktopSessionEvent::ModelCatalogError { error } => {
                last_status = Some(format!("model picker error: {error}"));
                println!(
                    "{}",
                    serde_json::json!({"event": "model_catalog_error", "error": error})
                );
            }
            session_launch::DesktopSessionEvent::StdinRequest {
                request_id,
                prompt,
                is_password,
                tool_call_id,
            } => {
                last_status = Some("interactive input requested".to_string());
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "stdin_request",
                        "request_id": request_id,
                        "prompt": prompt,
                        "is_password": is_password,
                        "tool_call_id": tool_call_id,
                    })
                );
            }
            session_launch::DesktopSessionEvent::ReloadProgress {
                step,
                message,
                success,
                output,
            } => {
                last_status = Some(format!("reload {step}: {message}"));
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "reload_progress",
                        "step": step,
                        "message": message,
                        "success": success,
                        "output": output,
                    })
                );
            }
            session_launch::DesktopSessionEvent::RuntimeMetadata {
                connection_type,
                status_detail,
                upstream_provider,
            } => {
                if let Some(status_detail) = &status_detail {
                    last_status = Some(status_detail.clone());
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "runtime_metadata",
                        "connection_type": connection_type,
                        "status_detail": status_detail,
                        "upstream_provider": upstream_provider,
                    })
                );
            }
            session_launch::DesktopSessionEvent::TokenUsage {
                input,
                output,
                cache_read_input,
                cache_creation_input,
            } => {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "tokens",
                        "input": input,
                        "output": output,
                        "cache_read_input": cache_read_input,
                        "cache_creation_input": cache_creation_input,
                    })
                );
            }
            session_launch::DesktopSessionEvent::SystemNotice { title, message } => {
                last_status = Some(title.clone());
                println!(
                    "{}",
                    serde_json::json!({"event": "system_notice", "title": title, "message": message})
                );
            }
            session_launch::DesktopSessionEvent::SessionCloseRequested { reason } => {
                anyhow::bail!(
                    "desktop chat smoke session close requested; session_id={}; reason={}",
                    session_id.as_deref().unwrap_or("unknown"),
                    reason
                );
            }
            session_launch::DesktopSessionEvent::Done => {
                let response = response.trim().to_string();
                if response.is_empty() {
                    anyhow::bail!(
                        "desktop chat smoke completed without assistant text; session_id={}; last_status={}",
                        session_id.as_deref().unwrap_or("unknown"),
                        last_status.as_deref().unwrap_or("unknown")
                    );
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "ok",
                        "session_id": session_id,
                        "response_chars": response.chars().count(),
                        "response_preview": response.chars().take(240).collect::<String>(),
                    })
                );
                return Ok(());
            }
            session_launch::DesktopSessionEvent::Error(error) => {
                anyhow::bail!(
                    "desktop chat smoke failed; session_id={}; error={}",
                    session_id.as_deref().unwrap_or("unknown"),
                    error
                );
            }
        }
    }

    anyhow::bail!(
        "desktop chat smoke timed out after {:?}; session_id={}; response_chars={}; last_status={}",
        HEADLESS_CHAT_SMOKE_TIMEOUT,
        session_id.as_deref().unwrap_or("unknown"),
        response.chars().count(),
        last_status.as_deref().unwrap_or("unknown")
    )
}

fn run_resize_render_benchmark(frames: usize) -> Result<()> {
    let frames = frames.max(1);
    let target_p95_ms = 16.0;
    let target_max_ms = 33.0;
    let base_size = PhysicalSize::new(1200, 760);
    let mut app = desktop_large_transcript_benchmark_app();
    let initial_body_lines = single_session_rendered_body_lines_for_tick(&app, base_size, 0);
    if let Some(metrics) = single_session_body_scroll_metrics_for_total_lines(
        &app,
        base_size,
        initial_body_lines.len(),
    ) {
        app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }
    let sizes = (0..frames).map(benchmark_resize_size).collect::<Vec<_>>();

    let mut legacy_font_system = benchmark_font_system();
    let (legacy_samples, legacy_checksum) = benchmark_frame_samples(frames, |frame| {
        let size = sizes[frame];
        let tick = frame as u64;
        let key = single_session_text_key_for_tick_with_scroll(&app, size, tick, 0.0);
        let buffers = single_session_text_buffers_from_key(&key, size, &mut legacy_font_system);
        let areas = single_session_text_areas_for_app_with_scroll(&app, &buffers, size, tick, 0.0);
        let body_glyphs = buffers
            .get(1)
            .map(|buffer| {
                buffer
                    .layout_runs()
                    .map(|run| run.glyphs.len())
                    .sum::<usize>()
            })
            .unwrap_or_default();
        let vertices =
            build_single_session_vertices_with_scroll_and_reveal(&app, size, 0.0, tick, 0.0, 1.0);
        key.body.len() ^ buffers.len() ^ areas.len() ^ vertices.len() ^ body_glyphs
    });

    let mut optimized_font_system = benchmark_font_system();
    let mut optimized_raw_body_key = Some(app.rendered_body_cache_key((0, 0)));
    let mut optimized_raw_body_lines = app.body_styled_lines_for_tick(0);
    let mut optimized_body_key = None;
    let mut optimized_body_lines = Vec::new();
    let mut optimized_text_cache_key = None;
    let mut optimized_text_key = None;
    let mut optimized_buffers: Vec<Buffer> = Vec::new();
    let mut optimized_window_start = None;
    let mut optimized_window_end = None;
    let mut optimized_body_rebuilds = 0usize;
    let mut optimized_body_wraps = 0usize;
    let (optimized_samples, optimized_checksum) = benchmark_frame_samples(frames, |frame| {
        let size = sizes[frame];
        let tick = frame as u64;
        let body_layout_size = single_session_body_layout_cache_size(&app, size);
        let body_key = app.rendered_body_cache_key(body_layout_size);
        let rendered_body_changed = if optimized_body_key != Some(body_key) {
            let raw_body_key = app.rendered_body_cache_key((0, 0));
            if optimized_raw_body_key != Some(raw_body_key) {
                optimized_raw_body_lines = app.body_styled_lines_for_tick(tick);
                optimized_raw_body_key = Some(raw_body_key);
            }
            optimized_body_lines = single_session_rendered_body_lines_from_raw_ref(
                &app,
                size,
                &optimized_raw_body_lines,
            );
            optimized_body_key = Some(body_key);
            optimized_window_start = None;
            optimized_window_end = None;
            optimized_body_wraps += 1;
            true
        } else {
            false
        };

        let viewport =
            single_session_body_viewport_from_lines(&app, size, 0.0, &optimized_body_lines);
        let text_cache_key = single_session_text_buffer_cache_key(&app, size, tick, body_key);
        let key = single_session_text_key_for_tick_with_rendered_body(
            &app,
            size,
            tick,
            0.0,
            &optimized_body_lines,
        );
        let text_key_changed = optimized_text_key.as_ref() != Some(&key);
        if optimized_text_cache_key != Some(text_cache_key) || text_key_changed {
            let desired_body_window = single_session_body_text_window_bounds(&viewport);
            let body_window_contains = if let (Some(window_start), Some(window_end)) =
                (optimized_window_start, optimized_window_end)
            {
                single_session_body_text_window_contains(window_start, window_end, &viewport)
            } else {
                false
            };
            let previous_key = optimized_text_key.take();
            let mut old_buffers = std::mem::take(&mut optimized_buffers);
            let body_content_changed_in_buffer =
                rendered_body_changed && app.streaming_response.is_empty();
            let body_layout_compatible = previous_key.as_ref().is_some_and(|previous| {
                single_session_body_text_buffer_layout_compatible(
                    previous.size,
                    size,
                    app.text_scale(),
                )
            });
            let mut can_reuse_body_buffer = old_buffers.len() > 1
                && body_window_contains
                && !body_content_changed_in_buffer
                && body_layout_compatible;
            if old_buffers.len() > 1
                && (!body_window_contains
                    || body_content_changed_in_buffer
                    || !body_layout_compatible)
            {
                let (window_start, window_end) = desired_body_window;
                old_buffers[1] = single_session_body_text_buffer_from_lines(
                    &mut optimized_font_system,
                    &optimized_body_lines[window_start..window_end],
                    size,
                    app.text_scale(),
                );
                optimized_window_start = Some(window_start);
                optimized_window_end = Some(window_end);
                optimized_body_rebuilds += 1;
                can_reuse_body_buffer = true;
            }
            optimized_buffers = single_session_text_buffers_from_key_reusing_unchanged(
                &key,
                previous_key.as_ref(),
                old_buffers,
                can_reuse_body_buffer,
                size,
                &mut optimized_font_system,
            );
            optimized_text_key = Some(key);
            optimized_text_cache_key = Some(text_cache_key);
            if !can_reuse_body_buffer {
                optimized_window_start = None;
                optimized_window_end = None;
            }
        }

        let viewport =
            single_session_body_viewport_from_lines(&app, size, 0.0, &optimized_body_lines);
        if let (Some(window_start), Some(window_end)) =
            (optimized_window_start, optimized_window_end)
            && single_session_body_text_window_contains(window_start, window_end, &viewport)
        {
            if let Some(body_buffer) = optimized_buffers.get_mut(1) {
                body_buffer.set_scroll(
                    viewport
                        .start_line
                        .saturating_sub(window_start)
                        .min(i32::MAX as usize) as i32,
                );
            }
        } else {
            let (window_start, window_end) = single_session_body_text_window_bounds(&viewport);
            if let Some(body_buffer) = optimized_buffers.get_mut(1) {
                *body_buffer = single_session_body_text_buffer_from_lines(
                    &mut optimized_font_system,
                    &optimized_body_lines[window_start..window_end],
                    size,
                    app.text_scale(),
                );
                body_buffer.set_scroll(
                    viewport
                        .start_line
                        .saturating_sub(window_start)
                        .min(i32::MAX as usize) as i32,
                );
                optimized_body_rebuilds += 1;
            }
            optimized_window_start = Some(window_start);
            optimized_window_end = Some(window_end);
        }

        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &app,
            &optimized_buffers,
            size,
            0.0,
            viewport,
        );
        let body_glyphs = optimized_buffers
            .get(1)
            .map(|buffer| {
                buffer
                    .layout_runs()
                    .map(|run| run.glyphs.len())
                    .sum::<usize>()
            })
            .unwrap_or_default();
        let vertices = build_single_session_vertices_with_cached_body(
            &app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &optimized_body_lines,
        );
        optimized_body_lines.len()
            ^ optimized_buffers.len()
            ^ areas.len()
            ^ vertices.len()
            ^ body_glyphs
    });

    let mut workspace_resize_cards = benchmark_workspace_session_cards(16);
    for (index, card) in workspace_resize_cards.iter_mut().enumerate() {
        card.transcript_messages = vec![
            workspace::SessionTranscriptMessage {
                role: "user".to_string(),
                content: format!("resize prompt {index}"),
            },
            workspace::SessionTranscriptMessage {
                role: "assistant".to_string(),
                content: format!(
                    "resize response {index}: representative workspace transcript content that wraps while the window is resized"
                ),
            },
        ];
    }
    let workspace_resize_app = Workspace::from_session_cards(workspace_resize_cards);
    let mut workspace_resize_font_system = benchmark_font_system();
    let mut workspace_resize_text_pane_cache = HashMap::new();
    let (workspace_resize_samples, workspace_resize_checksum) =
        benchmark_frame_samples(frames, |frame| {
            let size = sizes[frame];
            let layout = workspace_render_layout(&workspace_resize_app, size, Some(size));
            let panes = build_workspace_single_session_text_panes(
                &mut workspace_resize_text_pane_cache,
                &workspace_resize_app,
                size,
                layout,
                None,
                &mut workspace_resize_font_system,
            );
            let pane_count = panes.len();
            let areas = workspace_single_session_text_areas(&panes);
            let area_count = areas.len();
            drop(areas);
            drop(panes);
            let mut vertices =
                Vec::with_capacity(workspace_vertex_capacity_hint(&workspace_resize_app));
            build_vertices_into(
                WorkspaceVertexBuildParams {
                    workspace: &workspace_resize_app,
                    size,
                    render_layout: layout,
                    focus_pulse: 0.0,
                    space_hold_progress: None,
                    surface_frames: None,
                    exiting_surfaces: &HashMap::new(),
                    workspace_panel_cache: Some(&workspace_resize_text_pane_cache),
                    status_color: workspace_status_bar_target_color(&workspace_resize_app),
                    status_text_frame: None,
                },
                &mut vertices,
            );
            pane_count ^ area_count ^ vertices.len() ^ workspace_resize_app.surfaces.len()
        });

    let optimized_p95 = percentile_ms(&optimized_samples, 0.95);
    let optimized_max = max_sample_ms(&optimized_samples);
    let workspace_resize_p95 = percentile_ms(&workspace_resize_samples, 0.95);
    let workspace_resize_max = max_sample_ms(&workspace_resize_samples);
    let passes_workspace_resize_budget =
        workspace_resize_p95 <= target_p95_ms && workspace_resize_max <= target_max_ms;
    let passes_resize_cpu_budget = optimized_p95 <= target_p95_ms
        && optimized_max <= target_max_ms
        && passes_workspace_resize_budget;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frames": frames,
            "target_p95_ms": target_p95_ms,
            "target_repeated_max_ms": target_max_ms,
            "passes_resize_cpu_budget": passes_resize_cpu_budget,
            "passes_workspace_resize_budget": passes_workspace_resize_budget,
            "scenario": "large transcript and workspace continuous resize CPU layout paths",
            "size_range": {
                "min_width": sizes.iter().map(|size| size.width).min().unwrap_or_default(),
                "max_width": sizes.iter().map(|size| size.width).max().unwrap_or_default(),
                "min_height": sizes.iter().map(|size| size.height).min().unwrap_or_default(),
                "max_height": sizes.iter().map(|size| size.height).max().unwrap_or_default(),
            },
            "optimized_body_wraps": optimized_body_wraps,
            "optimized_body_buffer_rebuilds": optimized_body_rebuilds,
            "legacy": benchmark_samples_json("legacy_resize_full_text_relayout", &legacy_samples, legacy_checksum),
            "optimized": benchmark_samples_json("optimized_resize_cached_visible_body", &optimized_samples, optimized_checksum),
            "workspace_multi_pane": benchmark_samples_json("workspace_multi_pane_resize_reflow", &workspace_resize_samples, workspace_resize_checksum),
        }))?
    );
    Ok(())
}

fn run_scroll_render_benchmark(frames: usize) -> Result<()> {
    let frames = frames.max(1);
    let size = PhysicalSize::new(1200, 760);
    let mut app = desktop_scroll_benchmark_app();
    if let Some(metrics) = single_session_body_scroll_metrics(&app, size, 0) {
        app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }

    let mut setup_font_system = benchmark_font_system();
    let setup_started = Instant::now();
    let setup_key = single_session_text_key_for_tick_with_scroll(&app, size, 0, 0.0);
    let setup_buffers =
        single_session_text_buffers_from_key(&setup_key, size, &mut setup_font_system);
    let setup_areas =
        single_session_text_areas_for_app_with_scroll(&app, &setup_buffers, size, 0, 0.0);
    let setup_vertices =
        build_single_session_vertices_with_scroll_and_reveal(&app, size, 0.0, 0, 0.0, 1.0);
    let setup_elapsed = setup_started.elapsed();
    let setup_checksum =
        setup_key.body.len() ^ setup_buffers.len() ^ setup_areas.len() ^ setup_vertices.len();

    let cold_fresh_app = SingleSessionApp::new(None);
    let cold_fresh_started = Instant::now();
    let cold_phase_started = Instant::now();
    let mut cold_fresh_font_system = benchmark_font_system();
    let cold_fresh_font_ms = cold_phase_started.elapsed().as_secs_f64() * 1000.0;
    let cold_phase_started = Instant::now();
    let cold_fresh_key =
        single_session_text_key_for_tick_with_scroll(&cold_fresh_app, size, 0, 0.0);
    let cold_fresh_key_ms = cold_phase_started.elapsed().as_secs_f64() * 1000.0;
    let cold_phase_started = Instant::now();
    let cold_fresh_buffers =
        single_session_text_buffers_from_key(&cold_fresh_key, size, &mut cold_fresh_font_system);
    let cold_fresh_buffers_ms = cold_phase_started.elapsed().as_secs_f64() * 1000.0;
    let cold_phase_started = Instant::now();
    let cold_fresh_areas = single_session_text_areas_for_app_with_scroll(
        &cold_fresh_app,
        &cold_fresh_buffers,
        size,
        0,
        0.0,
    );
    let cold_fresh_areas_ms = cold_phase_started.elapsed().as_secs_f64() * 1000.0;
    let cold_phase_started = Instant::now();
    let cold_fresh_vertices = build_single_session_vertices_with_scroll_and_reveal(
        &cold_fresh_app,
        size,
        0.0,
        0,
        0.0,
        1.0,
    );
    let cold_fresh_vertices_ms = cold_phase_started.elapsed().as_secs_f64() * 1000.0;
    let cold_fresh_ms = cold_fresh_started.elapsed().as_secs_f64() * 1000.0;
    let cold_fresh_checksum = cold_fresh_key.body.len()
        ^ cold_fresh_buffers.len()
        ^ cold_fresh_areas.len()
        ^ cold_fresh_vertices.len();

    let prewarmed_fresh_app = SingleSessionApp::new(None);
    let mut prewarmed_fresh_font_system = benchmark_font_system();
    let prewarmed_fresh_started = Instant::now();
    let prewarmed_fresh_key =
        single_session_text_key_for_tick_with_scroll(&prewarmed_fresh_app, size, 0, 0.0);
    let prewarmed_fresh_buffers = single_session_text_buffers_from_key(
        &prewarmed_fresh_key,
        size,
        &mut prewarmed_fresh_font_system,
    );
    let prewarmed_fresh_areas = single_session_text_areas_for_app_with_scroll(
        &prewarmed_fresh_app,
        &prewarmed_fresh_buffers,
        size,
        0,
        0.0,
    );
    let prewarmed_fresh_vertices = build_single_session_vertices_with_scroll_and_reveal(
        &prewarmed_fresh_app,
        size,
        0.0,
        0,
        0.0,
        1.0,
    );
    let prewarmed_fresh_ms = prewarmed_fresh_started.elapsed().as_secs_f64() * 1000.0;
    let prewarmed_fresh_checksum = prewarmed_fresh_key.body.len()
        ^ prewarmed_fresh_buffers.len()
        ^ prewarmed_fresh_areas.len()
        ^ prewarmed_fresh_vertices.len();

    let warm_fresh_app = SingleSessionApp::new(None);
    let mut warm_fresh_font_system = benchmark_font_system();
    let warm_fresh_initial_key =
        single_session_text_key_for_tick_with_scroll(&warm_fresh_app, size, 0, 0.0);
    let warm_fresh_initial_buffers = single_session_text_buffers_from_key(
        &warm_fresh_initial_key,
        size,
        &mut warm_fresh_font_system,
    );
    let warm_fresh_started = Instant::now();
    let warm_fresh_next_key =
        single_session_text_key_for_tick_with_scroll(&warm_fresh_app, size, 1, 0.0);
    let warm_fresh_buffers = single_session_text_buffers_from_key_reusing_unchanged(
        &warm_fresh_next_key,
        Some(&warm_fresh_initial_key),
        warm_fresh_initial_buffers,
        true,
        size,
        &mut warm_fresh_font_system,
    );
    let warm_fresh_areas = single_session_text_areas_for_app_with_scroll(
        &warm_fresh_app,
        &warm_fresh_buffers,
        size,
        1,
        0.0,
    );
    let warm_fresh_vertices = build_single_session_vertices_with_scroll_and_reveal(
        &warm_fresh_app,
        size,
        0.0,
        1,
        0.0,
        1.0,
    );
    let warm_fresh_ms = warm_fresh_started.elapsed().as_secs_f64() * 1000.0;
    let warm_fresh_checksum = warm_fresh_next_key.body.len()
        ^ warm_fresh_buffers.len()
        ^ warm_fresh_areas.len()
        ^ warm_fresh_vertices.len();

    let mut legacy_font_system = benchmark_font_system();
    let (legacy_smooth_text_ms, legacy_smooth_text_checksum) = benchmark_phase(frames, |frame| {
        let tick = frame as u64;
        let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);
        let key =
            single_session_text_key_for_tick_with_scroll(&app, size, tick, smooth_scroll_lines);
        let buffers = single_session_text_buffers_from_key(&key, size, &mut legacy_font_system);
        let areas = single_session_text_areas_for_app_with_scroll(
            &app,
            &buffers,
            size,
            tick,
            smooth_scroll_lines,
        );
        let vertices = build_single_session_vertices_with_scroll_and_reveal(
            &app,
            size,
            0.0,
            tick,
            smooth_scroll_lines,
            1.0,
        );
        key.body.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut optimized_font_system = benchmark_font_system();
    let optimized_key = single_session_text_key_for_tick_with_scroll(&app, size, 0, 0.0);
    let optimized_buffers =
        single_session_text_buffers_from_key(&optimized_key, size, &mut optimized_font_system);
    let optimized_areas =
        single_session_text_areas_for_app_with_scroll(&app, &optimized_buffers, size, 0, 0.0);
    let optimized_body_lines = single_session_rendered_body_lines_for_tick(&app, size, 0);
    let (optimized_smooth_geometry_ms, optimized_smooth_geometry_checksum) =
        benchmark_phase(frames, |frame| {
            let tick = frame as u64;
            let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);
            let vertices = build_single_session_vertices_with_cached_body(
                &app,
                size,
                0.0,
                tick,
                smooth_scroll_lines,
                1.0,
                &optimized_body_lines,
            );
            optimized_key.body.len()
                ^ optimized_buffers.len()
                ^ optimized_areas.len()
                ^ vertices.len()
        });

    let mut whole_line_app = app.clone();
    let mut whole_line_font_system = benchmark_font_system();
    let whole_line_body_lines =
        single_session_rendered_body_lines_for_tick(&whole_line_app, size, 0);
    let (whole_line_text_ms, whole_line_text_checksum) = benchmark_phase(frames, |frame| {
        whole_line_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
        let tick = frame as u64;
        let key = single_session_text_key_for_tick_with_rendered_body(
            &whole_line_app,
            size,
            tick,
            0.0,
            &whole_line_body_lines,
        );
        let buffers = single_session_text_buffers_from_key(&key, size, &mut whole_line_font_system);
        let areas = single_session_text_areas_for_app_with_cached_body(
            &whole_line_app,
            &buffers,
            size,
            0.0,
            &whole_line_body_lines,
        );
        let vertices = build_single_session_vertices_with_cached_body(
            &whole_line_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &whole_line_body_lines,
        );
        key.body.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut visible_whole_line_app = app.clone();
    let mut visible_whole_line_font_system = benchmark_font_system();
    let visible_whole_line_body_lines =
        single_session_rendered_body_lines_for_tick(&visible_whole_line_app, size, 0);
    let visible_whole_line_key = single_session_text_key_for_tick_with_rendered_body(
        &visible_whole_line_app,
        size,
        0,
        0.0,
        &visible_whole_line_body_lines,
    );
    let mut visible_whole_line_buffers = single_session_text_buffers_from_key(
        &visible_whole_line_key,
        size,
        &mut visible_whole_line_font_system,
    );
    let mut visible_whole_line_start = single_session_body_viewport_from_lines(
        &visible_whole_line_app,
        size,
        0.0,
        &visible_whole_line_body_lines,
    )
    .start_line;
    let initial_visible_viewport = single_session_body_viewport_from_lines(
        &visible_whole_line_app,
        size,
        0.0,
        &visible_whole_line_body_lines,
    );
    let (mut visible_window_start, mut visible_window_end) =
        single_session_body_text_window_bounds(&initial_visible_viewport);
    if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut visible_whole_line_font_system,
            &visible_whole_line_body_lines[visible_window_start..visible_window_end],
            size,
            visible_whole_line_app.text_scale(),
        );
        body_buffer.set_scroll(
            initial_visible_viewport
                .start_line
                .saturating_sub(visible_window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    let mut visible_viewport_ms = 0.0;
    let mut visible_window_ms = 0.0;
    let mut visible_scroll_ms = 0.0;
    let mut visible_glyph_ms = 0.0;
    let mut visible_areas_ms = 0.0;
    let mut visible_vertices_ms = 0.0;
    let (visible_whole_line_text_ms, visible_whole_line_text_checksum) =
        benchmark_phase(frames, |frame| {
            visible_whole_line_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
            let tick = frame as u64;
            let phase_started = Instant::now();
            let viewport = single_session_body_viewport_from_lines(
                &visible_whole_line_app,
                size,
                0.0,
                &visible_whole_line_body_lines,
            );
            visible_viewport_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            if !single_session_body_text_window_contains(
                visible_window_start,
                visible_window_end,
                &viewport,
            ) {
                (visible_window_start, visible_window_end) =
                    single_session_body_text_window_bounds(&viewport);
                if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
                    *body_buffer = single_session_body_text_buffer_from_lines(
                        &mut visible_whole_line_font_system,
                        &visible_whole_line_body_lines[visible_window_start..visible_window_end],
                        size,
                        visible_whole_line_app.text_scale(),
                    );
                }
                visible_whole_line_start = usize::MAX;
            }
            visible_window_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            if viewport.start_line != visible_whole_line_start {
                if let Some(body_buffer) = visible_whole_line_buffers.get_mut(1) {
                    body_buffer.set_scroll(
                        viewport
                            .start_line
                            .saturating_sub(visible_window_start)
                            .min(i32::MAX as usize) as i32,
                    );
                }
                visible_whole_line_start = viewport.start_line;
            }
            visible_scroll_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let glyph_checksum = visible_whole_line_buffers
                .get(1)
                .map(|body_buffer| {
                    body_buffer
                        .layout_runs()
                        .map(|run| run.glyphs.len())
                        .sum::<usize>()
                })
                .unwrap_or_default();
            visible_glyph_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let areas = single_session_text_areas_for_app_with_cached_body_viewport(
                &visible_whole_line_app,
                &visible_whole_line_buffers,
                size,
                0.0,
                viewport,
            );
            visible_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let phase_started = Instant::now();
            let vertices = build_single_session_vertices_with_cached_body(
                &visible_whole_line_app,
                size,
                0.0,
                tick,
                0.0,
                1.0,
                &visible_whole_line_body_lines,
            );
            visible_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            visible_whole_line_key.body.len()
                ^ visible_whole_line_buffers.len()
                ^ areas.len()
                ^ vertices.len()
                ^ glyph_checksum
        });

    let mut typing_app = app.clone();
    typing_app.scroll_body_to_bottom();
    typing_app.draft.clear();
    typing_app.draft_cursor = 0;
    let typing_body_lines = single_session_rendered_body_lines_for_tick(&typing_app, size, 0);
    let mut typing_font_system = benchmark_font_system();
    let typing_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &typing_app,
        size,
        0,
        0.0,
        &typing_body_lines,
    );
    let mut typing_buffers =
        single_session_text_buffers_from_key(&typing_initial_key, size, &mut typing_font_system);
    let typing_initial_viewport =
        single_session_body_viewport_from_lines(&typing_app, size, 0.0, &typing_body_lines);
    let (typing_window_start, typing_window_end) =
        single_session_body_text_window_bounds(&typing_initial_viewport);
    if let Some(body_buffer) = typing_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut typing_font_system,
            &typing_body_lines[typing_window_start..typing_window_end],
            size,
            typing_app.text_scale(),
        );
    }
    let mut typing_previous_key = Some(typing_initial_key);
    let mut typing_text_cache_ms = 0.0;
    let mut typing_areas_ms = 0.0;
    let mut typing_vertices_ms = 0.0;
    let (typing_redraw_ms, typing_redraw_checksum) = benchmark_phase(frames, |frame| {
        let ch = benchmark_typing_char(frame);
        typing_app.draft.push(ch);
        typing_app.draft_cursor = typing_app.draft.len();
        let tick = frame as u64;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &typing_app,
            size,
            tick,
            0.0,
            &typing_body_lines,
        );
        let draft_len = key.draft.len();
        let previous_key = typing_previous_key.take();
        let old_buffers = std::mem::take(&mut typing_buffers);
        typing_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            true,
            size,
            &mut typing_font_system,
        );
        typing_previous_key = Some(key);
        typing_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body(
            &typing_app,
            &typing_buffers,
            size,
            0.0,
            &typing_body_lines,
        );
        typing_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &typing_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &typing_body_lines,
        );
        typing_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        draft_len ^ typing_buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut fresh_typing_app = SingleSessionApp::new(None);
    fresh_typing_app.draft.clear();
    fresh_typing_app.draft_cursor = 0;
    let mut fresh_typing_font_system = benchmark_font_system();
    let mut fresh_typing_text_cache_ms = 0.0;
    let mut fresh_typing_areas_ms = 0.0;
    let mut fresh_typing_vertices_ms = 0.0;
    let (fresh_typing_ms, fresh_typing_checksum) = benchmark_phase(frames, |frame| {
        let ch = benchmark_typing_char(frame);
        fresh_typing_app.draft.push(ch);
        fresh_typing_app.draft_cursor = fresh_typing_app.draft.len();
        let tick = frame as u64;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_scroll(&fresh_typing_app, size, tick, 0.0);
        let buffers =
            single_session_text_buffers_from_key(&key, size, &mut fresh_typing_font_system);
        fresh_typing_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_scroll(
            &fresh_typing_app,
            &buffers,
            size,
            tick,
            0.0,
        );
        fresh_typing_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_scroll_and_reveal(
            &fresh_typing_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
        );
        fresh_typing_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        key.draft.len() ^ buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut streaming_app = app.clone();
    streaming_app.scroll_body_to_bottom();
    streaming_app.streaming_response.clear();
    let mut streaming_font_system = benchmark_font_system();
    let mut streaming_body_lines =
        single_session_rendered_body_lines_for_tick(&streaming_app, size, 0);
    let mut streaming_base_key = None;
    let mut streaming_base_len = 0usize;
    let streaming_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &streaming_app,
        size,
        0,
        0.0,
        &streaming_body_lines,
    );
    let mut streaming_buffers = single_session_text_buffers_from_key(
        &streaming_initial_key,
        size,
        &mut streaming_font_system,
    );
    let streaming_initial_viewport =
        single_session_body_viewport_from_lines(&streaming_app, size, 0.0, &streaming_body_lines);
    let (mut streaming_window_start, mut streaming_window_end) =
        single_session_body_text_window_bounds(&streaming_initial_viewport);
    if let Some(body_buffer) = streaming_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut streaming_font_system,
            &streaming_body_lines[streaming_window_start..streaming_window_end],
            size,
            streaming_app.text_scale(),
        );
        body_buffer.set_scroll(
            streaming_initial_viewport
                .start_line
                .saturating_sub(streaming_window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    let mut streaming_previous_key = Some(streaming_initial_key);
    let mut streaming_tail_text_key = None;
    let mut streaming_tail_text_start_line = None;
    let mut streaming_tail_text_buffer = None;
    let mut streaming_body_ms = 0.0;
    let mut streaming_text_cache_ms = 0.0;
    let mut streaming_areas_ms = 0.0;
    let mut streaming_vertices_ms = 0.0;
    let mut streaming_static_base_rebuilds = 0usize;
    let mut streaming_tail_text_buffer_rebuilds = 0usize;
    let (streaming_delta_ms, streaming_delta_checksum) = benchmark_phase(frames, |frame| {
        streaming_app
            .streaming_response
            .push(benchmark_typing_char(frame));
        if frame % 17 == 0 {
            streaming_app.streaming_response.push('\n');
        }
        let tick = frame as u64;

        let phase_started = Instant::now();
        if !streaming_app.streaming_response.is_empty() {
            let base_key = streaming_app.rendered_body_static_cache_key((size.width, size.height));
            if streaming_base_key != Some(base_key) {
                streaming_static_base_rebuilds += 1;
                streaming_body_lines = single_session_rendered_static_body_lines_for_streaming(
                    &streaming_app,
                    size,
                    tick,
                )
                .unwrap_or_else(|| {
                    single_session_rendered_body_lines_for_tick(&streaming_app, size, tick)
                });
                streaming_base_len = streaming_body_lines.len();
                streaming_base_key = Some(base_key);
            } else {
                streaming_body_lines.truncate(streaming_base_len);
            }
            append_single_session_streaming_response_rendered_body_lines(
                &streaming_app,
                size,
                &mut streaming_body_lines,
            );
        } else {
            streaming_body_lines =
                single_session_rendered_body_lines_for_tick(&streaming_app, size, tick);
            streaming_base_key = None;
            streaming_base_len = 0;
        }
        streaming_body_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &streaming_app,
            size,
            tick,
            0.0,
            &streaming_body_lines,
        );
        let viewport = single_session_body_viewport_from_lines(
            &streaming_app,
            size,
            0.0,
            &streaming_body_lines,
        );
        let visible_static_start = viewport.start_line.min(streaming_base_len);
        let visible_static_end = viewport
            .start_line
            .saturating_add(viewport.lines.len())
            .min(streaming_base_len);
        let desired_streaming_window_start = visible_static_start
            .saturating_sub(SINGLE_SESSION_STREAMING_BODY_TEXT_WINDOW_BEFORE_LINES);
        let desired_streaming_window_end = visible_static_end
            .saturating_add(SINGLE_SESSION_STREAMING_BODY_TEXT_WINDOW_AFTER_LINES)
            .min(streaming_base_len)
            .max(desired_streaming_window_start);
        let body_window_contains = streaming_window_start == desired_streaming_window_start
            && streaming_window_end == desired_streaming_window_end;
        let previous_key = streaming_previous_key.take();
        let mut old_buffers = std::mem::take(&mut streaming_buffers);
        if old_buffers.len() > 1 && !body_window_contains {
            streaming_window_start = desired_streaming_window_start;
            streaming_window_end = desired_streaming_window_end;
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut streaming_font_system,
                &streaming_body_lines[streaming_window_start..streaming_window_end],
                size,
                streaming_app.text_scale(),
            );
        }
        let can_reuse_body_buffer = old_buffers.len() > 1;
        streaming_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut streaming_font_system,
        );
        if let Some(body_buffer) = streaming_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(streaming_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        let streaming_start_line =
            streaming_base_len.saturating_add(usize::from(!streaming_app.messages.is_empty()));
        let visible_start = viewport.start_line;
        let visible_end = viewport.start_line.saturating_add(viewport.lines.len());
        let streaming_visible_start = streaming_start_line.max(visible_start);
        let streaming_visible_end = streaming_body_lines.len().min(visible_end);
        if streaming_visible_start < streaming_visible_end {
            let mut hasher = DefaultHasher::new();
            (size.width, size.height).hash(&mut hasher);
            streaming_app.text_scale().to_bits().hash(&mut hasher);
            streaming_visible_start.hash(&mut hasher);
            streaming_visible_end.hash(&mut hasher);
            streaming_body_lines[streaming_visible_start..streaming_visible_end].hash(&mut hasher);
            let tail_key = hasher.finish();
            if streaming_tail_text_key != Some(tail_key) {
                streaming_tail_text_buffer_rebuilds += 1;
                streaming_tail_text_buffer = Some(single_session_body_text_buffer_from_lines(
                    &mut streaming_font_system,
                    &streaming_body_lines[streaming_visible_start..streaming_visible_end],
                    size,
                    streaming_app.text_scale(),
                ));
                streaming_tail_text_key = Some(tail_key);
                streaming_tail_text_start_line = Some(streaming_visible_start);
            }
        } else {
            streaming_tail_text_key = None;
            streaming_tail_text_start_line = None;
            streaming_tail_text_buffer = None;
        }
        streaming_previous_key = Some(key);
        streaming_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let mut areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &streaming_app,
            &streaming_buffers,
            size,
            0.0,
            viewport.clone(),
        );
        if let (Some(buffer), Some(start_line)) = (
            streaming_tail_text_buffer.as_ref(),
            streaming_tail_text_start_line,
        ) {
            areas.push(single_session_streaming_text_area_for_cached_body_viewport(
                &streaming_app,
                buffer,
                size,
                viewport,
                start_line,
                1.0,
                0.0,
            ));
        }
        streaming_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &streaming_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &streaming_body_lines,
        );
        streaming_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        streaming_body_lines.len()
            ^ streaming_buffers.len()
            ^ streaming_tail_text_buffer.is_some() as usize
            ^ streaming_static_base_rebuilds
            ^ streaming_tail_text_buffer_rebuilds
            ^ areas.len()
            ^ vertices.len()
    });

    let mut long_streaming_app = app.clone();
    long_streaming_app.scroll_body_to_bottom();
    long_streaming_app.streaming_response = (0..512)
        .map(|index| {
            format!(
                "### partial heading {index}\n- live item with **bold** text and `code` span number {index}\n"
            )
        })
        .collect::<String>();
    let (long_streaming_body_wrap_ms, long_streaming_body_wrap_checksum) =
        benchmark_phase(frames, |frame| {
            long_streaming_app
                .streaming_response
                .push(benchmark_typing_char(frame));
            if frame % 29 == 0 {
                long_streaming_app.streaming_response.push('\n');
            }
            let mut rendered_lines = Vec::new();
            append_single_session_streaming_response_rendered_body_lines(
                &long_streaming_app,
                size,
                &mut rendered_lines,
            );
            rendered_lines.len() ^ long_streaming_app.streaming_response.len()
        });
    let mut long_streaming_line_count_app = long_streaming_app.clone();
    let (long_streaming_line_count_ms, long_streaming_line_count_checksum) =
        benchmark_phase(frames, |frame| {
            long_streaming_line_count_app
                .streaming_response
                .push(benchmark_typing_char(frame));
            if frame % 31 == 0 {
                long_streaming_line_count_app.streaming_response.push('\n');
            }
            single_session_streaming_response_rendered_body_line_count(
                &long_streaming_line_count_app,
                size,
            ) ^ long_streaming_line_count_app.streaming_response.len()
        });
    let mut long_unbroken_streaming_app = app.clone();
    long_unbroken_streaming_app.streaming_response = "x".repeat(8192);
    let (long_unbroken_streaming_wrap_ms, long_unbroken_streaming_wrap_checksum) =
        benchmark_phase(frames, |frame| {
            long_unbroken_streaming_app
                .streaming_response
                .push(benchmark_typing_char(frame));
            let mut rendered_lines = Vec::new();
            append_single_session_streaming_response_rendered_body_lines(
                &long_unbroken_streaming_app,
                size,
                &mut rendered_lines,
            );
            rendered_lines.len() ^ long_unbroken_streaming_app.streaming_response.len()
        });

    let mut event_batch_app = DesktopApp::SingleSession(SingleSessionApp::new(None));
    let (event_batch_ms, event_batch_checksum) = benchmark_phase(frames, |frame| {
        let events = (0..128)
            .map(|offset| {
                session_launch::DesktopSessionEvent::TextDelta(
                    benchmark_typing_char(frame + offset).to_string(),
                )
            })
            .collect::<Vec<_>>();
        let original_events = events.len();
        let coalesced = coalesce_desktop_session_events(events);
        let coalesced_events = coalesced.len();
        apply_desktop_session_event_batch(&mut event_batch_app, coalesced);
        original_events ^ coalesced_events
    });

    let (event_forwarder_flood_ms, event_forwarder_flood_checksum) =
        benchmark_phase(frames, |frame| {
            let (tx, rx) = mpsc::channel();
            for offset in 0..(BACKEND_EVENT_FORWARD_MAX_RAW_EVENTS * 3) {
                tx.send(session_launch::DesktopSessionEvent::TextDelta(
                    benchmark_typing_char(frame + offset).to_string(),
                ))
                .unwrap();
            }
            let batch = collect_desktop_session_event_batch(
                session_launch::DesktopSessionEvent::TextDelta(
                    benchmark_typing_char(frame).to_string(),
                ),
                &rx,
            );
            let remaining_is_queued = rx.try_recv().is_ok();
            batch.raw_event_count
                ^ batch.raw_payload_bytes
                ^ batch.events.len()
                ^ usize::from(remaining_is_queued)
        });
    let end_to_end_stream_flood =
        run_desktop_stream_end_to_end_benchmark(BACKEND_EVENT_FORWARD_MAX_RAW_EVENTS * 6);

    let mut hero_app = desktop_scroll_benchmark_app_with_turns(24);
    let hero_body_lines = single_session_rendered_body_lines_for_tick(&hero_app, size, 0);
    let hero_boundary_scroll =
        benchmark_hero_boundary_scroll_lines(&hero_app, size, &hero_body_lines);
    hero_app.body_scroll_lines = hero_boundary_scroll;
    let mut hero_font_system = benchmark_font_system();
    let hero_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &hero_app,
        size,
        0,
        0.0,
        &hero_body_lines,
    );
    let mut hero_buffers =
        single_session_text_buffers_from_key(&hero_initial_key, size, &mut hero_font_system);
    let hero_initial_viewport =
        single_session_body_viewport_from_lines(&hero_app, size, 0.0, &hero_body_lines);
    let (mut hero_window_start, mut hero_window_end) =
        single_session_body_text_window_bounds(&hero_initial_viewport);
    if let Some(body_buffer) = hero_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut hero_font_system,
            &hero_body_lines[hero_window_start..hero_window_end],
            size,
            hero_app.text_scale(),
        );
    }
    let mut hero_previous_key = Some(hero_initial_key);
    let mut hero_viewport_key_ms = 0.0;
    let mut hero_window_rebuild_ms = 0.0;
    let mut hero_buffer_reuse_ms = 0.0;
    let mut hero_body_buffer_rebuilds = 0usize;
    let mut hero_text_cache_ms = 0.0;
    let mut hero_areas_ms = 0.0;
    let mut hero_vertices_ms = 0.0;
    let (hero_boundary_scroll_ms, hero_boundary_checksum) = benchmark_phase(frames, |frame| {
        let tick = frame as u64;
        let scroll_offset = (frame % 24) as f32 - 12.0;
        hero_app.body_scroll_lines = (hero_boundary_scroll + scroll_offset).max(0.0);
        let smooth_scroll_lines = benchmark_smooth_scroll_lines(frame);

        let phase_started = Instant::now();
        let subphase_started = Instant::now();
        let viewport = single_session_body_viewport_from_lines(
            &hero_app,
            size,
            smooth_scroll_lines,
            &hero_body_lines,
        );
        let key = single_session_text_key_for_tick_with_rendered_body(
            &hero_app,
            size,
            tick,
            smooth_scroll_lines,
            &hero_body_lines,
        );
        hero_viewport_key_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;

        let subphase_started = Instant::now();
        let previous_key = hero_previous_key.take();
        let mut old_buffers = std::mem::take(&mut hero_buffers);
        if old_buffers.len() > 1
            && !single_session_body_text_window_contains(
                hero_window_start,
                hero_window_end,
                &viewport,
            )
        {
            hero_body_buffer_rebuilds += 1;
            (hero_window_start, hero_window_end) =
                single_session_body_text_window_bounds(&viewport);
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut hero_font_system,
                &hero_body_lines[hero_window_start..hero_window_end],
                size,
                hero_app.text_scale(),
            );
        }
        hero_window_rebuild_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;

        let subphase_started = Instant::now();
        let can_reuse_body_buffer = old_buffers.len() > 1;
        hero_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut hero_font_system,
        );
        if let Some(body_buffer) = hero_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(hero_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        let hero_visible = key.fresh_welcome_visible;
        hero_previous_key = Some(key);
        hero_buffer_reuse_ms += subphase_started.elapsed().as_secs_f64() * 1000.0;
        hero_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &hero_app,
            &hero_buffers,
            size,
            smooth_scroll_lines,
            viewport,
        );
        hero_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &hero_app,
            size,
            0.0,
            tick,
            smooth_scroll_lines,
            1.0,
            &hero_body_lines,
        );
        hero_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        hero_buffers.len() ^ areas.len() ^ vertices.len() ^ usize::from(hero_visible)
    });

    let mut action_input_app = DesktopApp::SingleSession(SingleSessionApp::new(None));
    let (action_input_ms, action_input_checksum) = benchmark_phase(frames, |frame| {
        let events = (0..128)
            .map(|offset| session_launch::DesktopSessionEvent::ToolInput {
                id: None,
                delta: benchmark_typing_char(frame + offset).to_string(),
            })
            .collect::<Vec<_>>();
        let coalesced = coalesce_desktop_session_events(events);
        let visible_changed = apply_desktop_session_event_batch(&mut action_input_app, coalesced);
        usize::from(visible_changed)
    });

    let mut action_app = desktop_scroll_benchmark_app_with_turns(64);
    action_app.scroll_body_to_bottom();
    action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
        id: None,
        name: "bash".to_string(),
    });
    let mut action_font_system = benchmark_font_system();
    let mut action_body_key = action_app.rendered_body_cache_key((size.width, size.height));
    let mut action_body_lines = single_session_rendered_body_lines_for_tick(&action_app, size, 0);
    let action_initial_key = single_session_text_key_for_tick_with_rendered_body(
        &action_app,
        size,
        0,
        0.0,
        &action_body_lines,
    );
    let mut action_buffers =
        single_session_text_buffers_from_key(&action_initial_key, size, &mut action_font_system);
    let action_initial_viewport =
        single_session_body_viewport_from_lines(&action_app, size, 0.0, &action_body_lines);
    let (mut action_window_start, mut action_window_end) =
        single_session_body_text_window_bounds(&action_initial_viewport);
    if let Some(body_buffer) = action_buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut action_font_system,
            &action_body_lines[action_window_start..action_window_end],
            size,
            action_app.text_scale(),
        );
    }
    let mut action_previous_key = Some(action_initial_key);
    let mut action_apply_ms = 0.0;
    let mut action_body_ms = 0.0;
    let mut action_text_cache_ms = 0.0;
    let mut action_areas_ms = 0.0;
    let mut action_vertices_ms = 0.0;
    let (action_visible_ms, action_visible_checksum) = benchmark_phase(frames, |frame| {
        let phase_started = Instant::now();
        action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolInput {
            id: None,
            delta: format!(" chunk-{frame}"),
        });
        action_app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
            id: None,
            name: "bash".to_string(),
        });
        action_apply_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
        let tick = frame as u64;

        let phase_started = Instant::now();
        let next_body_key = action_app.rendered_body_cache_key((size.width, size.height));
        let action_body_changed = action_body_key != next_body_key;
        if action_body_changed {
            action_body_lines =
                single_session_rendered_body_lines_for_tick(&action_app, size, tick);
            action_body_key = next_body_key;
        }
        action_body_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let key = single_session_text_key_for_tick_with_rendered_body(
            &action_app,
            size,
            tick,
            0.0,
            &action_body_lines,
        );
        let viewport =
            single_session_body_viewport_from_lines(&action_app, size, 0.0, &action_body_lines);
        let previous_key = action_previous_key.take();
        let mut old_buffers = std::mem::take(&mut action_buffers);
        if old_buffers.len() > 1
            && (action_body_changed
                || !single_session_body_text_window_contains(
                    action_window_start,
                    action_window_end,
                    &viewport,
                ))
        {
            (action_window_start, action_window_end) =
                single_session_body_text_window_bounds(&viewport);
            old_buffers[1] = single_session_body_text_buffer_from_lines(
                &mut action_font_system,
                &action_body_lines[action_window_start..action_window_end],
                size,
                action_app.text_scale(),
            );
        }
        let can_reuse_body_buffer = old_buffers.len() > 1;
        action_buffers = single_session_text_buffers_from_key_reusing_unchanged(
            &key,
            previous_key.as_ref(),
            old_buffers,
            can_reuse_body_buffer,
            size,
            &mut action_font_system,
        );
        if let Some(body_buffer) = action_buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(action_window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        action_previous_key = Some(key);
        action_text_cache_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &action_app,
            &action_buffers,
            size,
            0.0,
            viewport,
        );
        action_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &action_app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &action_body_lines,
        );
        action_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;

        action_body_lines.len() ^ action_buffers.len() ^ areas.len() ^ vertices.len()
    });

    let mut workspace_app = Workspace::from_session_cards(benchmark_workspace_session_cards(128));
    let mut workspace_layout_ms = 0.0;
    let mut workspace_vertices_ms = 0.0;
    let mut workspace_visible_surfaces = 0usize;
    let mut workspace_navigation_font_system = benchmark_font_system();
    let mut workspace_navigation_text_pane_cache = HashMap::new();
    let (workspace_navigation_ms, workspace_navigation_checksum) =
        benchmark_phase(frames, |frame| {
            let key = if frame % 2 == 0 { "l" } else { "h" };
            let _ = workspace_app.handle_key(KeyInput::Character(key.to_string()));
            let phase_started = Instant::now();
            let layout = workspace_render_layout(&workspace_app, size, Some(size));
            workspace_layout_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let panes = build_workspace_single_session_text_panes(
                &mut workspace_navigation_text_pane_cache,
                &workspace_app,
                size,
                layout,
                None,
                &mut workspace_navigation_font_system,
            );
            let pane_count = panes.len();
            drop(panes);
            let phase_started = Instant::now();
            let mut vertices = Vec::with_capacity(workspace_vertex_capacity_hint(&workspace_app));
            build_vertices_into(
                WorkspaceVertexBuildParams {
                    workspace: &workspace_app,
                    size,
                    render_layout: layout,
                    focus_pulse: 0.0,
                    space_hold_progress: None,
                    surface_frames: None,
                    exiting_surfaces: &HashMap::new(),
                    workspace_panel_cache: Some(&workspace_navigation_text_pane_cache),
                    status_color: workspace_status_bar_target_color(&workspace_app),
                    status_text_frame: None,
                },
                &mut vertices,
            );
            workspace_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            workspace_visible_surfaces +=
                workspace_visible_surface_count(&workspace_app, size, layout);
            vertices.len()
                ^ pane_count
                ^ (workspace_app.focused_id as usize)
                ^ workspace_app.surfaces.len()
        });

    let mut workspace_full_app =
        Workspace::from_session_cards(benchmark_workspace_session_cards(512));
    let mut workspace_full_layout_ms = 0.0;
    let mut workspace_full_vertices_ms = 0.0;
    let mut workspace_full_text_panes_ms = 0.0;
    let mut workspace_full_text_areas_ms = 0.0;
    let mut workspace_full_visible_surfaces = 0usize;
    let mut workspace_full_text_pane_count = 0usize;
    let mut workspace_full_text_area_count = 0usize;
    let mut workspace_full_font_system = benchmark_font_system();
    let mut workspace_full_text_pane_cache = HashMap::new();
    let (workspace_full_frame_ms, workspace_full_frame_checksum) =
        benchmark_phase(frames, |frame| {
            let key = match frame % 4 {
                0 | 1 => "j",
                _ => "k",
            };
            let _ = workspace_full_app.handle_key(KeyInput::Character(key.to_string()));
            let phase_started = Instant::now();
            let layout = workspace_render_layout(&workspace_full_app, size, Some(size));
            workspace_full_layout_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            workspace_full_visible_surfaces +=
                workspace_visible_surface_count(&workspace_full_app, size, layout);
            let phase_started = Instant::now();
            let panes = build_workspace_single_session_text_panes(
                &mut workspace_full_text_pane_cache,
                &workspace_full_app,
                size,
                layout,
                None,
                &mut workspace_full_font_system,
            );
            workspace_full_text_panes_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let pane_count = panes.len();
            workspace_full_text_pane_count += pane_count;
            let phase_started = Instant::now();
            let areas = workspace_single_session_text_areas(&panes);
            workspace_full_text_areas_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            let area_count = areas.len();
            workspace_full_text_area_count += area_count;
            drop(areas);
            drop(panes);
            let phase_started = Instant::now();
            let mut vertices =
                Vec::with_capacity(workspace_vertex_capacity_hint(&workspace_full_app));
            build_vertices_into(
                WorkspaceVertexBuildParams {
                    workspace: &workspace_full_app,
                    size,
                    render_layout: layout,
                    focus_pulse: 0.0,
                    space_hold_progress: None,
                    surface_frames: None,
                    exiting_surfaces: &HashMap::new(),
                    workspace_panel_cache: Some(&workspace_full_text_pane_cache),
                    status_color: workspace_status_bar_target_color(&workspace_full_app),
                    status_text_frame: None,
                },
                &mut vertices,
            );
            workspace_full_vertices_ms += phase_started.elapsed().as_secs_f64() * 1000.0;
            vertices.len() ^ pane_count ^ area_count ^ workspace_full_app.surfaces.len()
        });

    let mut large_app = desktop_large_transcript_benchmark_app();
    let large_body_started = Instant::now();
    let large_body_lines = single_session_rendered_body_lines_for_tick(&large_app, size, 0);
    if let Some(metrics) =
        single_session_body_scroll_metrics_for_total_lines(&large_app, size, large_body_lines.len())
    {
        large_app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }
    let large_body_elapsed = large_body_started.elapsed();
    let (large_scroll_ms, large_scroll_checksum) = benchmark_phase(frames, |frame| {
        large_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
        let viewport =
            single_session_body_viewport_from_lines(&large_app, size, 0.0, &large_body_lines);
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &large_app,
            &visible_whole_line_buffers,
            size,
            0.0,
            viewport,
        );
        let vertices = build_single_session_vertices_with_cached_body(
            &large_app,
            size,
            0.0,
            frame as u64,
            0.0,
            1.0,
            &large_body_lines,
        );
        large_body_lines.len() ^ areas.len() ^ vertices.len()
    });
    let (large_cache_key_ms, large_cache_key_checksum) = benchmark_phase(frames, |frame| {
        let key = large_app.rendered_body_cache_key((size.width, size.height));
        (key as usize) ^ frame ^ large_app.messages.len()
    });

    let mut workspace_focus_app =
        Workspace::from_session_cards(benchmark_workspace_session_cards(128));
    let workspace_focus_layout = workspace_render_layout(&workspace_focus_app, size, Some(size));
    let mut workspace_focus_font_system = benchmark_font_system();
    let mut workspace_focus_text_pane_cache = HashMap::new();
    let _ = build_workspace_single_session_text_panes(
        &mut workspace_focus_text_pane_cache,
        &workspace_focus_app,
        size,
        workspace_focus_layout,
        None,
        &mut workspace_focus_font_system,
    );
    let (workspace_focus_pulse_ms, workspace_focus_pulse_checksum) =
        benchmark_phase(frames, |frame| {
            let key = match frame % 4 {
                0 => "l",
                1 => "j",
                2 => "h",
                _ => "k",
            };
            let _ = workspace_focus_app.handle_key(KeyInput::Character(key.to_string()));
            let layout = workspace_render_layout(&workspace_focus_app, size, Some(size));
            let focus_pulse = 0.35 + 0.45 * ((frame as f32) * 0.19).sin().abs();
            let mut vertices =
                Vec::with_capacity(workspace_vertex_capacity_hint(&workspace_focus_app));
            build_vertices_into(
                WorkspaceVertexBuildParams {
                    workspace: &workspace_focus_app,
                    size,
                    render_layout: layout,
                    focus_pulse,
                    space_hold_progress: None,
                    surface_frames: None,
                    exiting_surfaces: &HashMap::new(),
                    workspace_panel_cache: Some(&workspace_focus_text_pane_cache),
                    status_color: workspace_status_bar_target_color(&workspace_focus_app),
                    status_text_frame: None,
                },
                &mut vertices,
            );
            vertices.len() ^ workspace_focus_app.focused_id as usize
        });

    let mut workspace_transition_app =
        Workspace::from_session_cards(benchmark_workspace_session_cards(128));
    let mut workspace_transition_animator = SurfaceTransitionAnimator::default();
    let mut workspace_transition_exit_cache = HashMap::new();
    let transition_base_layout =
        workspace_render_layout(&workspace_transition_app, size, Some(size));
    let transition_targets = workspace_surface_transition_targets(
        &workspace_transition_app,
        size,
        transition_base_layout,
    );
    let transition_start = Instant::now();
    let _ = workspace_transition_animator.frame(transition_targets, transition_start);
    let mut workspace_transition_font_system = benchmark_font_system();
    let mut workspace_transition_text_pane_cache = HashMap::new();
    let _ = build_workspace_single_session_text_panes(
        &mut workspace_transition_text_pane_cache,
        &workspace_transition_app,
        size,
        transition_base_layout,
        None,
        &mut workspace_transition_font_system,
    );
    let (workspace_surface_transition_ms, workspace_surface_transition_checksum) =
        benchmark_phase(frames, |frame| {
            if frame == 0 || frame % 20 == 0 {
                let _ = workspace_transition_app.handle_key(KeyInput::Character("l".to_string()));
            } else if frame % 20 == 10 {
                let _ = workspace_transition_app.handle_key(KeyInput::Character("h".to_string()));
            }
            let layout = workspace_render_layout(&workspace_transition_app, size, Some(size));
            let targets =
                workspace_surface_transition_targets(&workspace_transition_app, size, layout);
            let now = transition_start + Duration::from_millis((frame as u64 + 1) * 8);
            let surface_frames = WorkspaceSurfaceTransitionFrames::new(
                workspace_transition_animator.frame(targets, now),
                workspace_transition_animator.is_animating(),
            );
            update_workspace_surface_exit_cache(
                &mut workspace_transition_exit_cache,
                &workspace_transition_app,
                &surface_frames,
            );
            let mut vertices =
                Vec::with_capacity(workspace_vertex_capacity_hint(&workspace_transition_app));
            build_vertices_into(
                WorkspaceVertexBuildParams {
                    workspace: &workspace_transition_app,
                    size,
                    render_layout: layout,
                    focus_pulse: 0.0,
                    space_hold_progress: None,
                    surface_frames: Some(&surface_frames),
                    exiting_surfaces: &workspace_transition_exit_cache,
                    workspace_panel_cache: Some(&workspace_transition_text_pane_cache),
                    status_color: workspace_status_bar_target_color(&workspace_transition_app),
                    status_text_frame: None,
                },
                &mut vertices,
            );
            vertices.len() ^ surface_frames.frames.len() ^ usize::from(surface_frames.animating)
        });

    let mut selection_drag_app = desktop_scroll_benchmark_app_with_turns(120);
    let selection_body_lines =
        single_session_rendered_body_lines_for_tick(&selection_drag_app, size, 0);
    if let Some(metrics) = single_session_body_scroll_metrics_for_total_lines(
        &selection_drag_app,
        size,
        selection_body_lines.len(),
    ) {
        selection_drag_app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }
    selection_drag_app.begin_selection(SelectionPoint { line: 0, column: 0 });
    let (selection_drag_ms, selection_drag_checksum) = benchmark_phase(frames, |frame| {
        let viewport = single_session_body_viewport_from_lines(
            &selection_drag_app,
            size,
            0.0,
            &selection_body_lines,
        );
        let line = (frame * 3).min(viewport.lines.len().saturating_sub(1));
        selection_drag_app.update_selection(SelectionPoint {
            line,
            column: viewport
                .lines
                .get(line)
                .map(|line| line.text.len())
                .unwrap_or_default(),
        });
        let vertices = build_single_session_vertices_with_cached_body(
            &selection_drag_app,
            size,
            0.0,
            frame as u64,
            0.0,
            1.0,
            &selection_body_lines,
        );
        viewport.lines.len() ^ vertices.len()
    });

    let mut streaming_scroll_app = app.clone();
    streaming_scroll_app.streaming_response.clear();
    let mut streaming_scroll_body_lines =
        single_session_rendered_body_lines_for_tick(&streaming_scroll_app, size, 0);
    let (streaming_while_scrolling_ms, streaming_while_scrolling_checksum) =
        benchmark_phase(frames, |frame| {
            streaming_scroll_app
                .streaming_response
                .push(benchmark_typing_char(frame));
            if frame % 13 == 0 {
                streaming_scroll_app.streaming_response.push('\n');
            }
            streaming_scroll_app.scroll_body_lines(if frame % 2 == 0 { 1 } else { -1 });
            streaming_scroll_body_lines = single_session_rendered_body_lines_for_tick(
                &streaming_scroll_app,
                size,
                frame as u64,
            );
            let viewport = single_session_body_viewport_from_lines(
                &streaming_scroll_app,
                size,
                benchmark_smooth_scroll_lines(frame),
                &streaming_scroll_body_lines,
            );
            let vertices = build_single_session_vertices_with_cached_body(
                &streaming_scroll_app,
                size,
                0.0,
                frame as u64,
                benchmark_smooth_scroll_lines(frame),
                1.0,
                &streaming_scroll_body_lines,
            );
            streaming_scroll_body_lines.len() ^ viewport.lines.len() ^ vertices.len()
        });

    let mut many_tools_app = desktop_scroll_benchmark_app_with_turns(16);
    for index in 0..320usize {
        let id = Some(format!("tool-{index}"));
        many_tools_app.apply_session_event(session_launch::DesktopSessionEvent::ToolStarted {
            id: id.clone(),
            name: "bash".to_string(),
        });
        many_tools_app.apply_session_event(session_launch::DesktopSessionEvent::ToolInput {
            id: id.clone(),
            delta: format!("echo card {index}\n"),
        });
        many_tools_app.apply_session_event(session_launch::DesktopSessionEvent::ToolExecuting {
            id: id.clone(),
            name: "bash".to_string(),
        });
        many_tools_app.apply_session_event(session_launch::DesktopSessionEvent::ToolFinished {
            id,
            name: "bash".to_string(),
            summary: format!("output line for tool card {index}\n"),
            is_error: false,
        });
    }
    let many_tools_body_lines =
        single_session_rendered_body_lines_for_tick(&many_tools_app, size, 0);
    if let Some(metrics) = single_session_body_scroll_metrics_for_total_lines(
        &many_tools_app,
        size,
        many_tools_body_lines.len(),
    ) {
        many_tools_app.body_scroll_lines = metrics.max_scroll_lines as f32 / 2.0;
    }
    let (many_tool_cards_ms, many_tool_cards_checksum) = benchmark_phase(frames, |frame| {
        many_tools_app.scroll_body_lines(if frame % 2 == 0 { 4 } else { -4 });
        let viewport = single_session_body_viewport_from_lines(
            &many_tools_app,
            size,
            0.0,
            &many_tools_body_lines,
        );
        let vertices = build_single_session_vertices_with_cached_body(
            &many_tools_app,
            size,
            0.0,
            frame as u64,
            0.0,
            1.0,
            &many_tools_body_lines,
        );
        many_tools_body_lines.len() ^ viewport.lines.len() ^ vertices.len()
    });

    let mut large_composer_app = app.clone();
    large_composer_app.scroll_body_to_bottom();
    large_composer_app.draft = (0..40).map(|index| format!("large pasted paragraph {index}: lorem ipsum dolor sit amet, consectetur adipiscing elit. ")).collect::<String>();
    large_composer_app.draft_cursor = large_composer_app.draft.len();
    let large_composer_body_lines =
        single_session_rendered_body_lines_for_tick(&large_composer_app, size, 0);
    let mut large_composer_font_system = benchmark_font_system();
    let mut large_composer_previous_key = None;
    let mut large_composer_buffers: Vec<Buffer> = Vec::new();
    let (large_composer_draft_ms, large_composer_draft_checksum) =
        benchmark_phase(frames, |frame| {
            large_composer_app.draft.push(benchmark_typing_char(frame));
            if frame % 41 == 0 {
                large_composer_app.draft.push('\n');
            }
            large_composer_app.draft_cursor = large_composer_app.draft.len();
            let key = single_session_text_key_for_tick_with_rendered_body(
                &large_composer_app,
                size,
                frame as u64,
                0.0,
                &large_composer_body_lines,
            );
            let previous_key = large_composer_previous_key.take();
            let old_buffers = std::mem::take(&mut large_composer_buffers);
            large_composer_buffers = single_session_text_buffers_from_key_reusing_unchanged(
                &key,
                previous_key.as_ref(),
                old_buffers,
                true,
                size,
                &mut large_composer_font_system,
            );
            large_composer_previous_key = Some(key);
            let areas = single_session_text_areas_for_app_with_cached_body(
                &large_composer_app,
                &large_composer_buffers,
                size,
                0.0,
                &large_composer_body_lines,
            );
            let vertices = build_single_session_vertices_with_cached_body(
                &large_composer_app,
                size,
                0.0,
                frame as u64,
                0.0,
                1.0,
                &large_composer_body_lines,
            );
            large_composer_app.draft.len()
                ^ large_composer_buffers.len()
                ^ areas.len()
                ^ vertices.len()
        });

    let target_budget_ms = duration_ms(DESKTOP_120FPS_FRAME_BUDGET);
    let critical_phase_means_ms = [
        visible_whole_line_text_ms / frames as f64,
        typing_redraw_ms / frames as f64,
        fresh_typing_ms / frames as f64,
        streaming_delta_ms / frames as f64,
        long_streaming_body_wrap_ms / frames as f64,
        long_streaming_line_count_ms / frames as f64,
        long_unbroken_streaming_wrap_ms / frames as f64,
        event_batch_ms / frames as f64,
        event_forwarder_flood_ms / frames as f64,
        hero_boundary_scroll_ms / frames as f64,
        action_input_ms / frames as f64,
        action_visible_ms / frames as f64,
        workspace_navigation_ms / frames as f64,
        workspace_full_frame_ms / frames as f64,
        workspace_focus_pulse_ms / frames as f64,
        workspace_surface_transition_ms / frames as f64,
        streaming_while_scrolling_ms / frames as f64,
        many_tool_cards_ms / frames as f64,
        large_scroll_ms / frames as f64,
        large_cache_key_ms / frames as f64,
    ];
    let passes_interaction_cpu_budget = critical_phase_means_ms
        .iter()
        .all(|mean_ms| *mean_ms <= target_budget_ms);
    let metrics = single_session_body_scroll_metrics(&app, size, 0);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frames": frames,
            "target_frame_budget_ms": target_budget_ms,
            "passes_120fps_scroll_cpu_budget": (visible_whole_line_text_ms / frames as f64)
                <= target_budget_ms,
            "passes_120fps_interaction_cpu_budget": passes_interaction_cpu_budget,
            "passes_no_paint_watchdog_budget": end_to_end_stream_flood.passes_no_paint_budget(),
            "passes_streaming_incremental_wrap_guard": streaming_static_base_rebuilds <= 1,
            "passes_long_streaming_body_wrap_budget": (long_streaming_body_wrap_ms / frames as f64) <= target_budget_ms,
            "passes_long_streaming_line_count_budget": (long_streaming_line_count_ms / frames as f64) <= target_budget_ms,
            "passes_long_unbroken_streaming_wrap_budget": (long_unbroken_streaming_wrap_ms / frames as f64) <= target_budget_ms,
            "passes_workspace_focus_pulse_budget": (workspace_focus_pulse_ms / frames as f64) <= target_budget_ms,
            "passes_workspace_surface_transition_budget": (workspace_surface_transition_ms / frames as f64) <= target_budget_ms,
            "passes_selection_drag_budget": (selection_drag_ms / frames as f64) <= 16.0,
            "passes_streaming_while_scrolling_budget": (streaming_while_scrolling_ms / frames as f64) <= target_budget_ms,
            "passes_many_tool_cards_budget": (many_tool_cards_ms / frames as f64) <= target_budget_ms,
            "passes_large_composer_draft_budget": (large_composer_draft_ms / frames as f64) <= 16.0,
            "event_delivery": {
                "previous_background_poll_interval_ms": duration_ms(BACKGROUND_POLL_INTERVAL),
                "backend_redraw_frame_interval_ms": duration_ms(BACKEND_REDRAW_FRAME_INTERVAL),
                "backend_event_forward_interval_ms": duration_ms(BACKEND_EVENT_FORWARD_INTERVAL),
                "backend_event_forward_max_raw_events": BACKEND_EVENT_FORWARD_MAX_RAW_EVENTS,
                "backend_event_forward_max_payload_bytes": BACKEND_EVENT_FORWARD_MAX_PAYLOAD_BYTES,
                "backend_events_wake_event_loop": true,
                "coalesces_consecutive_text_delta_events": true,
                "bounded_stream_flood_forwarding": true,
            },
            "no_paint_watchdog": {
                "budget_ms": duration_ms(DESKTOP_NO_PAINT_BUDGET),
                "log_event": "jcode-desktop-no-paint-profile",
                "requests_recovery_redraw": true,
            },
            "end_to_end_stream_flood": end_to_end_stream_flood.to_json(),
            "size": { "width": size.width, "height": size.height },
            "messages": app.messages.len(),
            "scroll_metrics": metrics.map(|metrics| serde_json::json!({
                "total_lines": metrics.total_lines,
                "visible_lines": metrics.visible_lines,
                "max_scroll_lines": metrics.max_scroll_lines,
                "start_scroll_lines": app.body_scroll_lines,
            })),
            "setup": benchmark_phase_json(
                "setup_text_and_geometry",
                setup_elapsed.as_secs_f64() * 1000.0,
                1,
                setup_checksum,
            ),
            "phases": [
                benchmark_phase_json(
                    "legacy_smooth_scroll_text_relayout",
                    legacy_smooth_text_ms,
                    frames,
                    legacy_smooth_text_checksum,
                ),
                benchmark_phase_json(
                    "optimized_smooth_scroll_geometry_only",
                    optimized_smooth_geometry_ms,
                    frames,
                    optimized_smooth_geometry_checksum,
                ),
                benchmark_phase_json(
                    "legacy_whole_line_scroll_text_relayout",
                    whole_line_text_ms,
                    frames,
                    whole_line_text_checksum,
                ),
                benchmark_phase_json(
                    "optimized_whole_line_scroll_visible_body_only",
                    visible_whole_line_text_ms,
                    frames,
                    visible_whole_line_text_checksum,
                ),
                benchmark_phase_json(
                    "typing_redraw_reuse_body_cache",
                    typing_redraw_ms,
                    frames,
                    typing_redraw_checksum,
                ),
                benchmark_phase_json(
                    "fresh_welcome_typing_redraw",
                    fresh_typing_ms,
                    frames,
                    fresh_typing_checksum,
                ),
                benchmark_phase_json(
                    "streaming_delta_redraw",
                    streaming_delta_ms,
                    frames,
                    streaming_delta_checksum,
                ),
                benchmark_phase_json(
                    "long_streaming_response_body_wrap",
                    long_streaming_body_wrap_ms,
                    frames,
                    long_streaming_body_wrap_checksum,
                ),
                benchmark_phase_json(
                    "long_streaming_response_line_count",
                    long_streaming_line_count_ms,
                    frames,
                    long_streaming_line_count_checksum,
                ),
                benchmark_phase_json(
                    "long_unbroken_streaming_line_wrap",
                    long_unbroken_streaming_wrap_ms,
                    frames,
                    long_unbroken_streaming_wrap_checksum,
                ),
                benchmark_phase_json(
                    "background_event_batch_coalesce_apply",
                    event_batch_ms,
                    frames,
                    event_batch_checksum,
                ),
                benchmark_phase_json(
                    "background_event_forwarder_flood_collect",
                    event_forwarder_flood_ms,
                    frames,
                    event_forwarder_flood_checksum,
                ),
                benchmark_phase_json(
                    "hero_boundary_scroll_redraw",
                    hero_boundary_scroll_ms,
                    frames,
                    hero_boundary_checksum,
                ),
                benchmark_phase_json(
                    "action_tool_input_batch_no_redraw",
                    action_input_ms,
                    frames,
                    action_input_checksum,
                ),
                benchmark_phase_json(
                    "action_tool_visible_redraw",
                    action_visible_ms,
                    frames,
                    action_visible_checksum,
                ),
                benchmark_phase_json(
                    "workspace_navigation_geometry",
                    workspace_navigation_ms,
                    frames,
                    workspace_navigation_checksum,
                ),
                benchmark_phase_json(
                    "workspace_full_frame_scroll_attributed",
                    workspace_full_frame_ms,
                    frames,
                    workspace_full_frame_checksum,
                ),
                benchmark_phase_json(
                    "workspace_focus_pulse_animation",
                    workspace_focus_pulse_ms,
                    frames,
                    workspace_focus_pulse_checksum,
                ),
                benchmark_phase_json(
                    "workspace_surface_enter_exit_transition",
                    workspace_surface_transition_ms,
                    frames,
                    workspace_surface_transition_checksum,
                ),
                benchmark_phase_json(
                    "selection_drag_large_transcript",
                    selection_drag_ms,
                    frames,
                    selection_drag_checksum,
                ),
                benchmark_phase_json(
                    "streaming_while_scrolling",
                    streaming_while_scrolling_ms,
                    frames,
                    streaming_while_scrolling_checksum,
                ),
                benchmark_phase_json(
                    "many_tool_cards_scroll",
                    many_tool_cards_ms,
                    frames,
                    many_tool_cards_checksum,
                ),
                benchmark_phase_json(
                    "large_composer_draft_edit",
                    large_composer_draft_ms,
                    frames,
                    large_composer_draft_checksum,
                ),
                benchmark_phase_json(
                    "large_transcript_scroll_visible_body_only",
                    large_scroll_ms,
                    frames,
                    large_scroll_checksum,
                ),
                benchmark_phase_json(
                    "large_transcript_cache_key",
                    large_cache_key_ms,
                    frames,
                    large_cache_key_checksum,
                ),
            ],
            "workspace_navigation_subphases": {
                "visible_surface_count_mean": workspace_visible_surfaces as f64 / frames as f64,
                "layout": benchmark_phase_json("workspace_layout", workspace_layout_ms, frames, 0),
                "vertices": benchmark_phase_json("workspace_vertices", workspace_vertices_ms, frames, 0),
            },
            "workspace_full_frame_subphases": {
                "surfaces_total": workspace_full_app.surfaces.len(),
                "visible_surface_count_mean": workspace_full_visible_surfaces as f64 / frames as f64,
                "text_pane_count_mean": workspace_full_text_pane_count as f64 / frames as f64,
                "text_area_count_mean": workspace_full_text_area_count as f64 / frames as f64,
                "layout": benchmark_phase_json("workspace_full_layout", workspace_full_layout_ms, frames, 0),
                "vertices": benchmark_phase_json("workspace_full_vertices", workspace_full_vertices_ms, frames, 0),
                "text_panes": benchmark_phase_json("workspace_full_text_panes", workspace_full_text_panes_ms, frames, 0),
                "text_areas": benchmark_phase_json("workspace_full_text_areas", workspace_full_text_areas_ms, frames, 0),
            },
            "visible_whole_line_subphases": [
                benchmark_phase_json("viewport", visible_viewport_ms, frames, 0),
                benchmark_phase_json("window", visible_window_ms, frames, 0),
                benchmark_phase_json("set_scroll", visible_scroll_ms, frames, 0),
                benchmark_phase_json("glyph_runs", visible_glyph_ms, frames, 0),
                benchmark_phase_json("areas", visible_areas_ms, frames, 0),
                benchmark_phase_json("vertices", visible_vertices_ms, frames, 0),
            ],
            "cold_start_cpu": [
                benchmark_phase_json("fresh_welcome_cold_text_frame", cold_fresh_ms, 1, cold_fresh_checksum),
                benchmark_phase_json("fresh_welcome_prewarmed_text_frame", prewarmed_fresh_ms, 1, prewarmed_fresh_checksum),
                benchmark_phase_json("fresh_welcome_warm_cached_text_frame", warm_fresh_ms, 1, warm_fresh_checksum),
            ],
            "cold_start_subphases": [
                benchmark_phase_json("font_system", cold_fresh_font_ms, 1, 0),
                benchmark_phase_json("text_key", cold_fresh_key_ms, 1, 0),
                benchmark_phase_json("text_buffers", cold_fresh_buffers_ms, 1, 0),
                benchmark_phase_json("text_areas", cold_fresh_areas_ms, 1, 0),
                benchmark_phase_json("vertices", cold_fresh_vertices_ms, 1, 0),
            ],
            "typing_redraw_subphases": [
                benchmark_phase_json("text_cache", typing_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", typing_areas_ms, frames, 0),
                benchmark_phase_json("vertices", typing_vertices_ms, frames, 0),
            ],
            "fresh_welcome_typing_subphases": [
                benchmark_phase_json("text_cache", fresh_typing_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", fresh_typing_areas_ms, frames, 0),
                benchmark_phase_json("vertices", fresh_typing_vertices_ms, frames, 0),
            ],
            "streaming_delta_subphases": [
                benchmark_phase_json("body_wrap", streaming_body_ms, frames, 0),
                benchmark_phase_json("text_cache", streaming_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", streaming_areas_ms, frames, 0),
                benchmark_phase_json("vertices", streaming_vertices_ms, frames, 0),
            ],
            "streaming_incremental_wrap": {
                "static_base_rebuilds": streaming_static_base_rebuilds,
                "tail_text_buffer_rebuilds": streaming_tail_text_buffer_rebuilds,
                "static_base_rebuild_budget": 1,
                "passes_static_base_rebuild_budget": streaming_static_base_rebuilds <= 1,
            },
            "hero_boundary": {
                "start_scroll_lines": hero_boundary_scroll,
                "body_buffer_rebuilds": hero_body_buffer_rebuilds,
                "subphases": [
                    benchmark_phase_json("text_cache", hero_text_cache_ms, frames, 0),
                    benchmark_phase_json("viewport_and_key", hero_viewport_key_ms, frames, 0),
                    benchmark_phase_json("body_window_rebuild", hero_window_rebuild_ms, frames, hero_body_buffer_rebuilds),
                    benchmark_phase_json("reuse_text_buffers", hero_buffer_reuse_ms, frames, 0),
                    benchmark_phase_json("areas", hero_areas_ms, frames, 0),
                    benchmark_phase_json("vertices", hero_vertices_ms, frames, 0),
                ],
            },
            "action_tool_visible_subphases": [
                benchmark_phase_json("event_apply_body_mutation", action_apply_ms, frames, 0),
                benchmark_phase_json("body_wrap", action_body_ms, frames, 0),
                benchmark_phase_json("text_cache", action_text_cache_ms, frames, 0),
                benchmark_phase_json("areas", action_areas_ms, frames, 0),
                benchmark_phase_json("vertices", action_vertices_ms, frames, 0),
            ],
            "large_transcript_setup": benchmark_phase_json(
                "large_transcript_initial_body_wrap",
                large_body_elapsed.as_secs_f64() * 1000.0,
                1,
                large_body_lines.len(),
            ),
        }))?
    );
    Ok(())
}

/// Selection knobs for the real-transcript benchmarks.
///
/// Returns `(max_sessions, min_messages)`: how many of the largest on-disk
/// transcripts to profile, and the minimum message count for a transcript to
/// qualify. Both are overridable via environment variables so a run can target
/// more (or fewer) of the biggest transcripts without a rebuild:
///
/// - `JCODE_DESKTOP_BENCHMARK_SESSIONS` (default 8)
/// - `JCODE_DESKTOP_BENCHMARK_MIN_MESSAGES` (default 24)
fn real_transcript_benchmark_selection() -> (usize, usize) {
    let max_sessions = std::env::var("JCODE_DESKTOP_BENCHMARK_SESSIONS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8);
    let min_messages = std::env::var("JCODE_DESKTOP_BENCHMARK_MIN_MESSAGES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(24);
    (max_sessions, min_messages)
}

/// Profile scrolling against the user's real on-disk transcripts.
///
/// This loads the largest real session files (full, untruncated message lists)
/// and drives the exact production windowed-scroll render path: cached body
/// wrap, a sliding text-buffer window, viewport extraction, glyph shaping for
/// the visible window, text areas, and primitive geometry. Per-frame work is
/// reported per session and aggregated so we can attribute any scroll jank to a
/// specific stage on real content rather than synthetic fixtures.
fn run_real_transcript_scroll_benchmark(frames: usize) -> Result<()> {
    let frames = frames.max(1);
    let size = PhysicalSize::new(1200, 760);
    let (max_sessions, min_messages) = real_transcript_benchmark_selection();
    let transcripts = session_data::load_largest_real_transcripts(max_sessions, min_messages)
        .context("failed to load real transcripts for scroll benchmark")?;

    if transcripts.is_empty() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "frames": frames,
                "sessions": [],
                "note": "no real transcripts with >=24 messages found under ~/.jcode/sessions",
            }))?
        );
        return Ok(());
    }

    let mut session_reports = Vec::new();
    let mut all_frame_samples: Vec<f64> = Vec::new();
    let mut worst_stage_us = 0.0_f64;
    let mut worst_stage_name = String::new();

    for transcript in &transcripts {
        let report = benchmark_real_transcript_scroll(transcript, size, frames);
        if report.worst_stage_us > worst_stage_us {
            worst_stage_us = report.worst_stage_us;
            worst_stage_name = report.worst_stage_name.clone();
        }
        all_frame_samples.extend_from_slice(&report.frame_samples);
        session_reports.push(report);
    }

    let budget_ms = duration_ms(DESKTOP_120FPS_FRAME_BUDGET);
    let aggregate_p50 = percentile_ms(&all_frame_samples, 0.50);
    let aggregate_p95 = percentile_ms(&all_frame_samples, 0.95);
    let aggregate_p99 = percentile_ms(&all_frame_samples, 0.99);
    let aggregate_max = max_sample_ms(&all_frame_samples);
    let passes_budget = aggregate_p99 <= budget_ms;

    let sessions_json = session_reports
        .iter()
        .map(RealTranscriptScrollReport::to_json)
        .collect::<Vec<_>>();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frames": frames,
            "size": { "width": size.width, "height": size.height },
            "target_frame_budget_ms": budget_ms,
            "sessions_profiled": session_reports.len(),
            "aggregate_full_scroll_frame": {
                "frames": all_frame_samples.len(),
                "p50_ms": aggregate_p50,
                "p95_ms": aggregate_p95,
                "p99_ms": aggregate_p99,
                "max_ms": aggregate_max,
            },
            "worst_stage": { "name": worst_stage_name, "max_us_per_frame": worst_stage_us },
            "passes_120fps_scroll_cpu_budget": passes_budget,
            "sessions": sessions_json,
        }))?
    );
    Ok(())
}

struct RealTranscriptScrollReport {
    session_id: String,
    title: String,
    file_bytes: u64,
    message_count: usize,
    total_body_lines: usize,
    max_scroll_lines: usize,
    body_buffer_rebuilds: usize,
    frame_samples: Vec<f64>,
    stage_totals_us: Vec<(&'static str, f64)>,
    setup_full_relayout_ms: f64,
    worst_stage_name: String,
    worst_stage_us: f64,
    worst_rebuild_us: f64,
    worst_rebuild_window_lines: usize,
    worst_rebuild_max_line_chars: usize,
    worst_rebuild_advanced_lines: usize,
    worst_rebuild_segments: usize,
}

impl RealTranscriptScrollReport {
    fn to_json(&self) -> serde_json::Value {
        let frames = self.frame_samples.len().max(1);
        let total_ms = self.frame_samples.iter().sum::<f64>();
        let stages = self
            .stage_totals_us
            .iter()
            .map(|(name, total_us)| {
                serde_json::json!({
                    "name": name,
                    "mean_us_per_frame": total_us / frames as f64,
                    "total_ms": total_us / 1000.0,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "session_id": self.session_id,
            "title": self.title,
            "file_bytes": self.file_bytes,
            "message_count": self.message_count,
            "total_body_lines": self.total_body_lines,
            "max_scroll_lines": self.max_scroll_lines,
            "body_buffer_rebuilds": self.body_buffer_rebuilds,
            "setup_full_body_relayout_ms": self.setup_full_relayout_ms,
            "worst_window_rebuild": {
                "us": self.worst_rebuild_us,
                "window_lines": self.worst_rebuild_window_lines,
                "max_line_chars": self.worst_rebuild_max_line_chars,
                "advanced_shaping_lines": self.worst_rebuild_advanced_lines,
                "segments": self.worst_rebuild_segments,
            },
            "full_scroll_frame": {
                "frames": self.frame_samples.len(),
                "mean_ms_per_frame": total_ms / frames as f64,
                "p50_ms": percentile_ms(&self.frame_samples, 0.50),
                "p95_ms": percentile_ms(&self.frame_samples, 0.95),
                "p99_ms": percentile_ms(&self.frame_samples, 0.99),
                "max_ms": max_sample_ms(&self.frame_samples),
            },
            "subphases": stages,
        })
    }
}

/// Build a `SingleSessionApp` backed by a full real transcript, exactly the way
/// the production resume path hydrates one from disk.
fn real_transcript_scroll_app(transcript: &session_data::BenchmarkTranscript) -> SingleSessionApp {
    let mut app = SingleSessionApp::new(None);
    app.apply_resumed_session_transcript(transcript.messages.clone());
    app.set_status_label(format!("real transcript: {}", transcript.title));
    app
}

fn benchmark_real_transcript_scroll(
    transcript: &session_data::BenchmarkTranscript,
    size: PhysicalSize<u32>,
    frames: usize,
) -> RealTranscriptScrollReport {
    let mut app = real_transcript_scroll_app(transcript);
    let mut font_system = benchmark_font_system();

    // One-time full body wrap (the cost paid when a transcript is first loaded
    // or the window is resized). After this, scrolling must stay windowed.
    let setup_started = Instant::now();
    let body_lines = single_session_rendered_body_lines_for_tick(&app, size, 0);
    let setup_full_relayout_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
    let total_body_lines = body_lines.len();

    let max_scroll_lines =
        single_session_body_scroll_metrics_for_total_lines(&app, size, total_body_lines)
            .map(|metrics| metrics.max_scroll_lines)
            .unwrap_or(0);

    // Prime the sliding text-buffer window at the bottom of the transcript, the
    // way the app does after hydrating a resumed session.
    app.scroll_body_to_bottom();
    let initial_viewport = single_session_body_viewport_from_lines(&app, size, 0.0, &body_lines);
    let initial_key =
        single_session_text_key_for_tick_with_rendered_body(&app, size, 0, 0.0, &body_lines);
    let mut buffers = single_session_text_buffers_from_key(&initial_key, size, &mut font_system);
    let (mut window_start, mut window_end) =
        single_session_body_text_window_bounds(&initial_viewport);
    if let Some(body_buffer) = buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            &mut font_system,
            &body_lines[window_start..window_end],
            size,
            app.text_scale(),
        );
        body_buffer.set_scroll(
            initial_viewport
                .start_line
                .saturating_sub(window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    let mut last_scroll_start = initial_viewport.start_line;

    // Drive a long scroll sweep from bottom to top and back, one whole line per
    // frame, so every frame crosses a new line boundary (the worst realistic
    // continuous-scroll case).
    let span = max_scroll_lines.max(1);
    let mut viewport_us = 0.0;
    let mut window_rebuild_us = 0.0;
    let mut scroll_us = 0.0;
    let mut glyph_us = 0.0;
    let mut areas_us = 0.0;
    let mut vertices_us = 0.0;
    let mut body_buffer_rebuilds = 0usize;

    // Optional diagnostic: capture the single slowest window rebuild and describe
    // the window content so we can attribute the cost (line count, advanced
    // shaping triggers, longest line) rather than guessing.
    let diagnose = std::env::var_os("JCODE_DESKTOP_SCROLL_DIAG").is_some();
    let mut worst_rebuild_us = 0.0_f64;
    let mut worst_rebuild_window_lines = 0usize;
    let mut worst_rebuild_max_line_chars = 0usize;
    let mut worst_rebuild_advanced_lines = 0usize;
    let mut worst_rebuild_segments = 0usize;

    let (frame_samples, _checksum) = benchmark_frame_samples(frames, |frame| {
        // Triangle-wave scroll position covering the full transcript height.
        let phase = frame % (span * 2);
        let target = if phase <= span {
            phase
        } else {
            span * 2 - phase
        };
        app.body_scroll_lines = target as f32;
        let tick = frame as u64;

        let phase_started = Instant::now();
        let viewport = single_session_body_viewport_from_lines(&app, size, 0.0, &body_lines);
        viewport_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        let phase_started = Instant::now();
        if !single_session_body_text_window_contains(window_start, window_end, &viewport) {
            (window_start, window_end) = single_session_body_text_window_bounds(&viewport);
            let rebuild_started = Instant::now();
            if let Some(body_buffer) = buffers.get_mut(1) {
                *body_buffer = single_session_body_text_buffer_from_lines(
                    &mut font_system,
                    &body_lines[window_start..window_end],
                    size,
                    app.text_scale(),
                );
            }
            if diagnose {
                let rebuild_us = rebuild_started.elapsed().as_secs_f64() * 1_000_000.0;
                if rebuild_us > worst_rebuild_us {
                    worst_rebuild_us = rebuild_us;
                    let window = &body_lines[window_start..window_end];
                    worst_rebuild_window_lines = window.len();
                    worst_rebuild_max_line_chars = window
                        .iter()
                        .map(|l| l.text.chars().count())
                        .max()
                        .unwrap_or(0);
                    worst_rebuild_advanced_lines =
                        window.iter().filter(|l| !l.text.is_ascii()).count();
                    worst_rebuild_segments = window.iter().map(|l| l.inline_spans.len() + 1).sum();
                    if let Ok(path) = std::env::var("JCODE_DESKTOP_SCROLL_DIAG_DUMP") {
                        let text = window
                            .iter()
                            .map(|l| l.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = std::fs::write(format!("{path}.{}", transcript.session_id), text);
                    }
                }
            }
            body_buffer_rebuilds += 1;
            last_scroll_start = usize::MAX;
        }
        window_rebuild_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        let phase_started = Instant::now();
        if viewport.start_line != last_scroll_start {
            if let Some(body_buffer) = buffers.get_mut(1) {
                body_buffer.set_scroll(
                    viewport
                        .start_line
                        .saturating_sub(window_start)
                        .min(i32::MAX as usize) as i32,
                );
            }
            last_scroll_start = viewport.start_line;
        }
        scroll_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        let phase_started = Instant::now();
        let glyph_checksum = buffers
            .get(1)
            .map(|body_buffer| {
                body_buffer
                    .layout_runs()
                    .map(|run| run.glyphs.len())
                    .sum::<usize>()
            })
            .unwrap_or_default();
        glyph_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        let phase_started = Instant::now();
        let areas = single_session_text_areas_for_app_with_cached_body_viewport(
            &app, &buffers, size, 0.0, viewport,
        );
        areas_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        let phase_started = Instant::now();
        let vertices = build_single_session_vertices_with_cached_body(
            &app,
            size,
            0.0,
            tick,
            0.0,
            1.0,
            &body_lines,
        );
        vertices_us += phase_started.elapsed().as_secs_f64() * 1_000_000.0;

        buffers.len() ^ areas.len() ^ vertices.len() ^ glyph_checksum
    });

    let stage_totals_us = vec![
        ("viewport_extract", viewport_us),
        ("body_window_rebuild", window_rebuild_us),
        ("body_scroll_set", scroll_us),
        ("glyph_layout_count", glyph_us),
        ("text_areas", areas_us),
        ("primitive_vertices", vertices_us),
    ];
    let frames_f = frames.max(1) as f64;
    let (worst_stage_name, worst_stage_us) = stage_totals_us
        .iter()
        .map(|(name, total)| (name.to_string(), total / frames_f))
        .fold((String::new(), 0.0_f64), |acc, candidate| {
            if candidate.1 > acc.1 { candidate } else { acc }
        });

    RealTranscriptScrollReport {
        session_id: transcript.session_id.clone(),
        title: transcript.title.clone(),
        file_bytes: transcript.file_bytes,
        message_count: transcript.messages.len(),
        total_body_lines,
        max_scroll_lines,
        body_buffer_rebuilds,
        frame_samples,
        stage_totals_us,
        setup_full_relayout_ms,
        worst_stage_name,
        worst_stage_us,
        worst_rebuild_us,
        worst_rebuild_window_lines,
        worst_rebuild_max_line_chars,
        worst_rebuild_advanced_lines,
        worst_rebuild_segments,
    }
}

/// Profile a realistic mix of user *actions* (not just scrolling) against the
/// user's largest real on-disk transcripts. Each action phase is measured
/// separately as per-frame CPU samples and reported as p50/p95/p99/max, plus a
/// `passes_120fps_cpu_budget` flag against the existing frame budget. This is the
/// broad interaction-coverage companion to `--real-transcript-scroll-benchmark`.
fn run_real_transcript_action_benchmark(frames: usize) -> Result<()> {
    let frames = frames.max(1);
    let size = PhysicalSize::new(1200, 760);
    let (max_sessions, min_messages) = real_transcript_benchmark_selection();
    let transcripts = session_data::load_largest_real_transcripts(max_sessions, min_messages)
        .context("failed to load real transcripts for action benchmark")?;

    if transcripts.is_empty() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "frames": frames,
                "sessions": [],
                "note": "no real transcripts with >=24 messages found under ~/.jcode/sessions",
            }))?
        );
        return Ok(());
    }

    let budget_ms = duration_ms(DESKTOP_120FPS_FRAME_BUDGET);
    // phase name -> all per-frame samples across every session
    let mut phase_samples: std::collections::BTreeMap<&'static str, Vec<f64>> =
        std::collections::BTreeMap::new();
    let mut session_json = Vec::new();

    for transcript in &transcripts {
        let phases = benchmark_real_transcript_actions(transcript, size, frames);
        let phase_json = phases
            .iter()
            .map(|(name, samples)| {
                phase_samples
                    .entry(name)
                    .or_default()
                    .extend_from_slice(samples);
                action_phase_json(name, samples, budget_ms)
            })
            .collect::<Vec<_>>();
        session_json.push(serde_json::json!({
            "session_id": transcript.session_id,
            "title": transcript.title,
            "message_count": transcript.messages.len(),
            "phases": phase_json,
        }));
    }

    let mut aggregate = Vec::new();
    let mut slowest_phase = String::new();
    let mut slowest_p99 = 0.0_f64;
    let mut all_pass = true;
    for (name, samples) in &phase_samples {
        let value = action_phase_json(name, samples, budget_ms);
        let p99 = percentile_ms(samples, 0.99);
        if p99 > slowest_p99 {
            slowest_p99 = p99;
            slowest_phase = (*name).to_string();
        }
        if p99 > budget_ms {
            all_pass = false;
        }
        aggregate.push(value);
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "frames_per_phase": frames,
            "size": { "width": size.width, "height": size.height },
            "target_frame_budget_ms": budget_ms,
            "sessions_profiled": transcripts.len(),
            "aggregate_phases": aggregate,
            "slowest_phase": { "name": slowest_phase, "p99_ms": slowest_p99 },
            "passes_120fps_cpu_budget": all_pass,
            "sessions": session_json,
        }))?
    );
    Ok(())
}

fn action_phase_json(name: &str, samples: &[f64], budget_ms: f64) -> serde_json::Value {
    let frames = samples.len().max(1);
    let total_ms = samples.iter().sum::<f64>();
    let p99 = percentile_ms(samples, 0.99);
    serde_json::json!({
        "name": name,
        "frames": samples.len(),
        "mean_ms": total_ms / frames as f64,
        "p50_ms": percentile_ms(samples, 0.50),
        "p95_ms": percentile_ms(samples, 0.95),
        "p99_ms": p99,
        "max_ms": max_sample_ms(samples),
        "passes_budget": p99 <= budget_ms,
    })
}

/// Run every simulated action phase for one transcript, returning per-phase
/// per-frame CPU samples (milliseconds). Each phase reproduces the production
/// render path: cached/wrapped body lines, viewport extraction, a windowed body
/// text buffer that is reused across frames, text areas, and primitive geometry.
fn benchmark_real_transcript_actions(
    transcript: &session_data::BenchmarkTranscript,
    size: PhysicalSize<u32>,
    frames: usize,
) -> Vec<(&'static str, Vec<f64>)> {
    let base_app = real_transcript_scroll_app(transcript);
    let body_lines = single_session_rendered_body_lines_for_tick(&base_app, size, 0);
    let total_lines = body_lines.len();
    let max_scroll =
        single_session_body_scroll_metrics_for_total_lines(&base_app, size, total_lines)
            .map(|metrics| metrics.max_scroll_lines)
            .unwrap_or(0)
            .max(1);

    let mut phases: Vec<(&'static str, Vec<f64>)> = Vec::new();

    // 1. Smooth (fractional) scroll: scroll position advances a whole line per
    //    frame with a fractional offset, the common trackpad-scroll case.
    phases.push((
        "smooth_scroll",
        action_windowed_render_phase(&base_app, &body_lines, size, frames, |app, frame| {
            let phase = frame % (max_scroll * 2);
            let target = if phase <= max_scroll {
                phase
            } else {
                max_scroll * 2 - phase
            };
            app.body_scroll_lines = target as f32;
            benchmark_smooth_scroll_lines(frame)
        }),
    ));

    // 2. Whole-line scroll: integer line steps, no fractional offset.
    phases.push((
        "whole_line_scroll",
        action_windowed_render_phase(&base_app, &body_lines, size, frames, |app, frame| {
            let phase = frame % (max_scroll * 2);
            let target = if phase <= max_scroll {
                phase
            } else {
                max_scroll * 2 - phase
            };
            app.body_scroll_lines = target as f32;
            0.0
        }),
    ));

    // 3. Selection drag across the visible transcript while parked mid-scroll.
    //    This mirrors the real mouse-handler input path, which calls
    //    single_session_visible_body (a full transcript wrap, now memoized) and
    //    hit-tests the cursor on every pointer move, then redraws.
    {
        let mut app = base_app.clone();
        app.body_scroll_lines = (max_scroll / 2) as f32;
        let initial_visible = single_session_visible_body(&app, size);
        if let Some(point) =
            single_session_body_point_at_position(size, 40.0, 80.0, &initial_visible)
        {
            app.begin_selection(point);
        } else {
            app.begin_selection(SelectionPoint { line: 0, column: 0 });
        }
        let mut font_system = benchmark_font_system();
        let (mut buffers, mut window_start, mut window_end, mut last_start) =
            action_prime_window(&app, &body_lines, size, &mut font_system);
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            // Real input path: resolve the cursor against the visible body
            // (full-transcript wrap, memoized) and update the selection.
            let visible = single_session_visible_body(&app, size);
            let y = 80.0 + (frame % 600) as f32;
            let x = 40.0 + (frame % 400) as f32;
            if let Some(point) = single_session_body_point_at_position(size, x, y, &visible) {
                app.update_selection(point);
            }
            action_render_window(
                &app,
                &body_lines,
                size,
                frame as u64,
                0.0,
                &mut font_system,
                &mut buffers,
                &mut window_start,
                &mut window_end,
                &mut last_start,
            )
        });
        phases.push(("selection_drag", samples));
    }

    // 3b. Pure input-side selection hit-test cost (no redraw). This isolates the
    //     real per-mouse-move work the desktop selection handler does:
    //     single_session_visible_body (a full-transcript wrap, now memoized) plus
    //     cursor hit-testing. The redraw it triggers is separately cached, so this
    //     phase exposes the wrap/memo cost that the combined selection_drag phase
    //     hides behind geometry building.
    {
        let mut app = base_app.clone();
        app.body_scroll_lines = (max_scroll / 2) as f32;
        app.begin_selection(SelectionPoint { line: 0, column: 0 });
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            let visible = single_session_visible_body(&app, size);
            let y = 80.0 + (frame % 600) as f32;
            let x = 40.0 + (frame % 400) as f32;
            if let Some(point) = single_session_body_point_at_position(size, x, y, &visible) {
                app.update_selection(point);
            }
            visible.len()
        });
        phases.push(("selection_input_hittest", samples));
    }

    // 4. Typing in the composer while parked at the bottom of the transcript.
    {
        let mut app = base_app.clone();
        app.scroll_body_to_bottom();
        app.draft.clear();
        app.draft_cursor = 0;
        let mut font_system = benchmark_font_system();
        let (mut buffers, mut window_start, mut window_end, mut last_start) =
            action_prime_window(&app, &body_lines, size, &mut font_system);
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            app.draft.push(benchmark_typing_char(frame));
            app.draft_cursor = app.draft.len();
            action_render_window(
                &app,
                &body_lines,
                size,
                frame as u64,
                0.0,
                &mut font_system,
                &mut buffers,
                &mut window_start,
                &mut window_end,
                &mut last_start,
            )
        });
        phases.push(("composer_typing", samples));
    }

    // 5. Model picker open/close toggling over the transcript: every other frame
    //    opens the inline picker card, invalidating the inline-widget geometry.
    {
        let mut app = base_app.clone();
        app.body_scroll_lines = (max_scroll / 3) as f32;
        let mut font_system = benchmark_font_system();
        let (mut buffers, mut window_start, mut window_end, mut last_start) =
            action_prime_window(&app, &body_lines, size, &mut font_system);
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            app.model_picker.open = frame % 2 == 0;
            app.model_picker.loading = app.model_picker.open;
            action_render_window(
                &app,
                &body_lines,
                size,
                frame as u64,
                0.0,
                &mut font_system,
                &mut buffers,
                &mut window_start,
                &mut window_end,
                &mut last_start,
            )
        });
        app.model_picker.open = false;
        phases.push(("model_picker_toggle", samples));
    }

    // 6. Session switcher open/close toggling over the transcript.
    {
        let mut app = base_app.clone();
        app.body_scroll_lines = (max_scroll / 3) as f32;
        let mut font_system = benchmark_font_system();
        let (mut buffers, mut window_start, mut window_end, mut last_start) =
            action_prime_window(&app, &body_lines, size, &mut font_system);
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            app.session_switcher.open = frame % 2 == 0;
            action_render_window(
                &app,
                &body_lines,
                size,
                frame as u64,
                0.0,
                &mut font_system,
                &mut buffers,
                &mut window_start,
                &mut window_end,
                &mut last_start,
            )
        });
        app.session_switcher.open = false;
        phases.push(("session_switcher_toggle", samples));
    }

    // 7. Window resize sweep: each frame is a different surface size, forcing a
    //    body re-wrap + window rebuild (the worst non-scroll case).
    //
    //    Mirrors production (`cached_single_session_body_lines` non-streaming
    //    branch): the raw styled lines (markdown parse) are generated ONCE and
    //    cached across sizes; only the width-dependent wrap re-runs per resize.
    {
        let app = base_app.clone();
        let raw_lines = app.body_styled_lines_for_tick(0);
        let mut font_system = benchmark_font_system();
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            let resize = benchmark_resize_size(frame);
            let lines = single_session_rendered_body_lines_from_raw_ref(&app, resize, &raw_lines);
            let viewport = single_session_body_viewport_from_lines(&app, resize, 0.0, &lines);
            let key =
                single_session_text_key_for_tick_with_rendered_body(&app, resize, 0, 0.0, &lines);
            let mut buffers = single_session_text_buffers_from_key(&key, resize, &mut font_system);
            let (window_start, window_end) = single_session_body_text_window_bounds(&viewport);
            if let Some(body_buffer) = buffers.get_mut(1) {
                *body_buffer = single_session_body_text_buffer_from_lines(
                    &mut font_system,
                    &lines[window_start..window_end],
                    resize,
                    app.text_scale(),
                );
            }
            let areas = single_session_text_areas_for_app_with_cached_body_viewport(
                &app, &buffers, resize, 0.0, viewport,
            );
            let vertices = build_single_session_vertices_with_cached_body(
                &app,
                resize,
                0.0,
                frame as u64,
                0.0,
                1.0,
                &lines,
            );
            buffers.len() ^ areas.len() ^ vertices.len()
        });
        phases.push(("window_resize", samples));
    }

    // 8. Streaming response growth while scrolled near the bottom: a synthetic
    //    assistant reply grows by a chunk each frame, the live-streaming case.
    //
    //    This mirrors the production renderer's incremental path
    //    (`cached_single_session_body_lines` for the streaming branch): the
    //    static transcript body is wrapped ONCE, then each frame only truncates
    //    back to the static base and appends the wrapped streaming tail, rather
    //    than re-wrapping the whole transcript every frame.
    {
        let mut app = base_app.clone();
        app.scroll_body_to_bottom();
        app.streaming_response
            .push_str("Streaming response starting. ");
        let mut font_system = benchmark_font_system();
        let static_base = single_session_rendered_static_body_lines_for_streaming(&app, size, 0)
            .unwrap_or_else(|| single_session_rendered_body_lines_for_tick(&app, size, 0));
        let static_len = static_base.len();
        let mut stream_lines = static_base.clone();
        let (samples, _) = benchmark_frame_samples(frames, |frame| {
            app.streaming_response.push_str(
                "Streaming update chunk with `inline code` and prose that wraps across lines. ",
            );
            if frame % 9 == 0 {
                app.streaming_response.push('\n');
            }
            // Incremental: reuse the wrapped static base, only re-wrap the tail.
            stream_lines.truncate(static_len);
            append_single_session_streaming_response_rendered_body_lines(
                &app,
                size,
                &mut stream_lines,
            );
            let viewport = single_session_body_viewport_from_lines(&app, size, 0.0, &stream_lines);
            let key = single_session_text_key_for_tick_with_rendered_body(
                &app,
                size,
                0,
                0.0,
                &stream_lines,
            );
            let mut buffers = single_session_text_buffers_from_key(&key, size, &mut font_system);
            let (window_start, window_end) = single_session_body_text_window_bounds(&viewport);
            if let Some(body_buffer) = buffers.get_mut(1) {
                *body_buffer = single_session_body_text_buffer_from_lines(
                    &mut font_system,
                    &stream_lines[window_start..window_end],
                    size,
                    app.text_scale(),
                );
            }
            let areas = single_session_text_areas_for_app_with_cached_body_viewport(
                &app, &buffers, size, 0.0, viewport,
            );
            let vertices = build_single_session_vertices_with_cached_body(
                &app,
                size,
                0.0,
                frame as u64,
                0.0,
                1.0,
                &stream_lines,
            );
            buffers.len() ^ areas.len() ^ vertices.len()
        });
        phases.push(("streaming_growth", samples));
    }

    phases
}

/// Prime a reusable text-buffer set and its windowed body buffer for `app`,
/// matching how the production renderer seeds the sliding window. Returns the
/// buffers plus the current (window_start, window_end, last_scroll_start).
fn action_prime_window(
    app: &SingleSessionApp,
    body_lines: &[SingleSessionStyledLine],
    size: PhysicalSize<u32>,
    font_system: &mut FontSystem,
) -> (Vec<Buffer>, usize, usize, usize) {
    let viewport = single_session_body_viewport_from_lines(app, size, 0.0, body_lines);
    let key = single_session_text_key_for_tick_with_rendered_body(app, size, 0, 0.0, body_lines);
    let mut buffers = single_session_text_buffers_from_key(&key, size, font_system);
    let (window_start, window_end) = single_session_body_text_window_bounds(&viewport);
    if let Some(body_buffer) = buffers.get_mut(1) {
        *body_buffer = single_session_body_text_buffer_from_lines(
            font_system,
            &body_lines[window_start..window_end],
            size,
            app.text_scale(),
        );
        body_buffer.set_scroll(
            viewport
                .start_line
                .saturating_sub(window_start)
                .min(i32::MAX as usize) as i32,
        );
    }
    (buffers, window_start, window_end, viewport.start_line)
}

/// Render one frame through the production windowed path, reusing the body text
/// buffer and only rebuilding/rescrolling the window when the viewport leaves it.
#[allow(clippy::too_many_arguments)]
fn action_render_window(
    app: &SingleSessionApp,
    body_lines: &[SingleSessionStyledLine],
    size: PhysicalSize<u32>,
    tick: u64,
    smooth_scroll_lines: f32,
    font_system: &mut FontSystem,
    buffers: &mut [Buffer],
    window_start: &mut usize,
    window_end: &mut usize,
    last_scroll_start: &mut usize,
) -> usize {
    let viewport =
        single_session_body_viewport_from_lines(app, size, smooth_scroll_lines, body_lines);
    if !single_session_body_text_window_contains(*window_start, *window_end, &viewport) {
        let (start, end) = single_session_body_text_window_bounds(&viewport);
        *window_start = start;
        *window_end = end;
        if let Some(body_buffer) = buffers.get_mut(1) {
            *body_buffer = single_session_body_text_buffer_from_lines(
                font_system,
                &body_lines[start..end],
                size,
                app.text_scale(),
            );
        }
        *last_scroll_start = usize::MAX;
    }
    if viewport.start_line != *last_scroll_start {
        if let Some(body_buffer) = buffers.get_mut(1) {
            body_buffer.set_scroll(
                viewport
                    .start_line
                    .saturating_sub(*window_start)
                    .min(i32::MAX as usize) as i32,
            );
        }
        *last_scroll_start = viewport.start_line;
    }
    let areas = single_session_text_areas_for_app_with_cached_body_viewport(
        app,
        buffers,
        size,
        smooth_scroll_lines,
        viewport,
    );
    let vertices = build_single_session_vertices_with_cached_body(
        app,
        size,
        0.0,
        tick,
        smooth_scroll_lines,
        1.0,
        body_lines,
    );
    buffers.len() ^ areas.len() ^ vertices.len()
}

/// Drive a windowed-scroll render phase, calling `prepare` each frame to mutate
/// the app's scroll position (and return any fractional smooth-scroll offset).
fn action_windowed_render_phase(
    base_app: &SingleSessionApp,
    body_lines: &[SingleSessionStyledLine],
    size: PhysicalSize<u32>,
    frames: usize,
    mut prepare: impl FnMut(&mut SingleSessionApp, usize) -> f32,
) -> Vec<f64> {
    let mut app = base_app.clone();
    let mut font_system = benchmark_font_system();
    let (mut buffers, mut window_start, mut window_end, mut last_start) =
        action_prime_window(&app, body_lines, size, &mut font_system);
    let (samples, _) = benchmark_frame_samples(frames, |frame| {
        let smooth = prepare(&mut app, frame);
        action_render_window(
            &app,
            body_lines,
            size,
            frame as u64,
            smooth,
            &mut font_system,
            &mut buffers,
            &mut window_start,
            &mut window_end,
            &mut last_start,
        )
    });
    samples
}

fn run_stream_e2e_benchmark(raw_events: usize) -> Result<()> {
    let result = run_desktop_stream_end_to_end_benchmark(raw_events);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "target_frame_budget_ms": duration_ms(DESKTOP_120FPS_FRAME_BUDGET),
            "no_paint_budget_ms": duration_ms(DESKTOP_NO_PAINT_BUDGET),
            "event_delivery": {
                "backend_redraw_frame_interval_ms": duration_ms(BACKEND_REDRAW_FRAME_INTERVAL),
                "backend_event_forward_interval_ms": duration_ms(BACKEND_EVENT_FORWARD_INTERVAL),
                "backend_event_forward_max_raw_events": BACKEND_EVENT_FORWARD_MAX_RAW_EVENTS,
                "backend_event_forward_max_payload_bytes": BACKEND_EVENT_FORWARD_MAX_PAYLOAD_BYTES,
            },
            "passes_120fps_interaction_cpu_budget": result.passes_interaction_budget(),
            "passes_no_paint_watchdog_budget": result.passes_no_paint_budget(),
            "end_to_end_stream_flood": result.to_json(),
        }))?
    );
    Ok(())
}

fn benchmark_hero_boundary_scroll_lines(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    body_lines: &[SingleSessionStyledLine],
) -> f32 {
    let Some(metrics) =
        single_session_body_scroll_metrics_for_total_lines(app, size, body_lines.len())
    else {
        return 0.0;
    };
    let mut probe = app.clone();
    for scroll in 0..=metrics.max_scroll_lines {
        probe.body_scroll_lines = scroll as f32;
        let key =
            single_session_text_key_for_tick_with_rendered_body(&probe, size, 0, 0.0, body_lines);
        if key.fresh_welcome_visible {
            return scroll.saturating_sub(6) as f32;
        }
    }
    metrics.max_scroll_lines.saturating_sub(12) as f32
}

fn benchmark_font_system() -> FontSystem {
    create_desktop_font_system()
}

fn create_desktop_font_system() -> FontSystem {
    let mut font_system = FontSystem::new();
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/Kalam-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/ShadowsIntoLightTwo-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/HomemadeApple-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/PatrickHand-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/Gaegu-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/Caveat-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/IndieFlower-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/GloriaHallelujah-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/Handlee-Regular.ttf").to_vec());
    font_system
        .db_mut()
        .load_font_data(include_bytes!("../assets/fonts/ReenieBeanie-Regular.ttf").to_vec());
    font_system
}

fn spawn_desktop_font_system_loader() -> JoinHandle<FontSystem> {
    std::thread::spawn(create_desktop_font_system)
}

#[cfg(target_os = "linux")]
fn desktop_wgpu_startup_backends() -> Vec<wgpu::Backends> {
    vec![wgpu::Backends::PRIMARY]
}

#[cfg(not(target_os = "linux"))]
fn desktop_wgpu_startup_backends() -> Vec<wgpu::Backends> {
    vec![wgpu::Backends::PRIMARY]
}

async fn request_startup_adapter(
    window: Arc<Window>,
    backend_candidates: Vec<wgpu::Backends>,
    startup_trace: DesktopStartupTrace,
) -> Result<(wgpu::Surface<'static>, wgpu::Adapter)> {
    let mut last_error = None;
    for backends in backend_candidates {
        let backend_label = format!("{backends:?}");
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::empty().with_env(),
            ..Default::default()
        });
        startup_trace.mark(&format!("wgpu instance created ({backend_label})"));
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                last_error = Some(format!(
                    "{backend_label}: failed to create surface: {error:#}"
                ));
                continue;
            }
        };
        startup_trace.mark(&format!("wgpu surface created ({backend_label})"));
        if let Some(adapter) = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
        {
            startup_trace.mark(&format!("wgpu adapter selected ({backend_label})"));
            return Ok((surface, adapter));
        }
        last_error = Some(format!("{backend_label}: no compatible adapter"));
    }

    match last_error {
        Some(error) => anyhow::bail!("failed to find a compatible GPU adapter ({error})"),
        None => anyhow::bail!("failed to find a compatible GPU adapter"),
    }
}

fn desktop_scroll_benchmark_app() -> SingleSessionApp {
    desktop_scroll_benchmark_app_with_turns(320)
}

fn desktop_large_transcript_benchmark_app() -> SingleSessionApp {
    desktop_scroll_benchmark_app_with_turns(2_000)
}

fn benchmark_workspace_session_cards(count: usize) -> Vec<workspace::SessionCard> {
    (0..count)
        .map(|index| workspace::SessionCard {
            session_id: format!("benchmark-session-{index}"),
            title: format!("agent {index} · desktop benchmark"),
            subtitle: format!("workspace surface {index}"),
            detail: "rendering session metadata, preview lines, and detail transcript".to_string(),
            preview_lines: vec![
                "recent prompt: inspect render latency and input jank".to_string(),
                "assistant: caching text and geometry keeps navigation responsive".to_string(),
                format!("status: benchmark card {index}"),
            ],
            detail_lines: (0..16)
                .map(|line| {
                    format!(
                        "detail line {line}: this synthetic transcript line exercises zoom/detail rendering for card {index}"
                    )
                })
                .collect(),
            transcript_messages: Vec::new(),
        })
        .collect()
}

fn desktop_scroll_benchmark_app_with_turns(turns: usize) -> SingleSessionApp {
    let mut app = SingleSessionApp::new(None);
    app.draft = "summarize the latest changes and suggest the next optimization".to_string();
    app.draft_cursor = app.draft.len();
    for turn in 0..turns {
        app.messages.push(SingleSessionMessage::user(format!(
            "Prompt {turn}: inspect this desktop render path and explain where scroll jank may come from."
        )));
        app.messages.push(SingleSessionMessage::assistant(format!(
            "Assistant response {turn}: The render path includes markdown wrapping, transcript card geometry, scrollbar geometry, text buffer preparation, and GPU primitive upload. This paragraph is intentionally long enough to wrap across several desktop body lines so the benchmark exercises visible-line virtualization and wrapping behavior.\n\n- Check cached text keys.\n- Check smooth scroll fractional offsets.\n- Check whether geometry can update without reshaping text.\n\n```rust\nfn sample_{turn}() {{ println!(\"benchmark\"); }}\n```"
        )));
    }
    app.set_status_label("benchmark fixture");
    app
}

fn load_desktop_preferences() -> Option<workspace::DesktopPreferences> {
    match desktop_prefs::load_preferences() {
        Ok(preferences) => preferences,
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to load desktop preferences: {error:#}"
            ));
            None
        }
    }
}

fn fresh_single_session_app() -> DesktopApp {
    DesktopApp::SingleSession(SingleSessionApp::new(None))
}

fn fresh_desktop_app_for_worker_mode(mode: DesktopWorkerMode) -> DesktopApp {
    match mode {
        DesktopWorkerMode::SingleSession => fresh_single_session_app(),
        DesktopWorkerMode::Workspace => DesktopApp::Workspace(Workspace::loading_sessions()),
    }
}

fn initial_single_session_app(resume_session_id: Option<&str>) -> DesktopApp {
    let Some(session_id) = resume_session_id else {
        return fresh_single_session_app();
    };

    let mut app = SingleSessionApp::new(None);
    app.initialize_resumed_session(session_id);
    match session_data::load_session_card_by_id(session_id) {
        Ok(Some(card)) => {
            app.replace_session(Some(card));
            app.hydrate_resumed_session_from_disk(session_id);
        }
        Ok(None) => {
            app.set_status_label(format!("resumed session {session_id}"));
            app.hydrate_resumed_session_from_disk(session_id);
        }
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to load resumed session metadata for {session_id}: {error:#}"
            ));
            app.set_status_label(format!("resumed session {session_id}"));
            app.error = Some(format!("failed to load session metadata: {error:#}"));
        }
    }
    DesktopApp::SingleSession(app)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopMode {
    SingleSession,
    WorkspacePrototype,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopProcessRole {
    Standalone,
    StableHost,
    AppWorker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopReloadStrategy {
    FullProcessHandoff,
    AppWorkerRestart,
}

impl DesktopProcessRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::StableHost => "stable_host",
            Self::AppWorker => "app_worker",
        }
    }

    fn reload_strategy(self) -> DesktopReloadStrategy {
        match self {
            Self::Standalone | Self::AppWorker => DesktopReloadStrategy::FullProcessHandoff,
            Self::StableHost => DesktopReloadStrategy::AppWorkerRestart,
        }
    }
}

impl DesktopMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::SingleSession => "single_session",
            Self::WorkspacePrototype => "workspace",
        }
    }

    fn worker_mode(self) -> DesktopWorkerMode {
        match self {
            Self::SingleSession => DesktopWorkerMode::SingleSession,
            Self::WorkspacePrototype => DesktopWorkerMode::Workspace,
        }
    }
}

fn run_desktop_app_worker_process(desktop_mode: DesktopMode) -> Result<()> {
    desktop_log::info(format_args!(
        "jcode-desktop: app worker process started; pid={}",
        std::process::id()
    ));

    let mut stdout = std::io::stdout().lock();
    let ready = DesktopProtocolEnvelope::new(
        1,
        DesktopWorkerToHostMessage::Ready(DesktopWorkerReady {
            worker_pid: std::process::id(),
            mode: desktop_mode.worker_mode(),
        }),
    );
    write_desktop_ipc_frame(&mut stdout, &ready).context("failed to write worker ready frame")?;

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut runtime: Option<DesktopAppRuntime<DesktopApp>> = None;
    let mut latest_window = DesktopWindowState {
        width: DEFAULT_WINDOW_WIDTH as u32,
        height: DEFAULT_WINDOW_HEIGHT as u32,
        scale_factor: 1.0,
        focused: true,
    };
    let mut next_worker_sequence = 2;
    loop {
        let frame: Option<DesktopHostToWorkerEnvelope> =
            desktop_ipc::read_desktop_ipc_frame(&mut reader)
                .context("failed to read host frame")?;
        let Some(frame) = frame else {
            break;
        };
        frame
            .validate_version()
            .context("host sent incompatible protocol frame")?;
        match frame.payload {
            DesktopHostToWorkerMessage::Initialize(init) => {
                latest_window = init.window.clone();
                let mut app = fresh_desktop_app_for_worker_mode(init.mode);
                if let Some(snapshot) = init.snapshot.clone()
                    && let Err(error) = app.restore_snapshot(snapshot)
                {
                    desktop_log::error(format_args!(
                        "jcode-desktop: app worker failed to restore host snapshot: {error:#}"
                    ));
                }
                let app_runtime = DesktopAppRuntime::new(app);
                let scene = desktop_scene_for_worker_runtime(&app_runtime, &latest_window);
                runtime = Some(app_runtime);
                let scene_update = DesktopProtocolEnvelope::new(
                    next_worker_sequence,
                    DesktopWorkerToHostMessage::Scene(DesktopSceneUpdate {
                        animation_active: scene.metadata.animation_active,
                        scene,
                    }),
                );
                next_worker_sequence += 1;
                write_desktop_ipc_frame(&mut stdout, &scene_update)
                    .context("failed to write worker initial scene")?;
            }
            DesktopHostToWorkerMessage::SnapshotRequest { request_id } => {
                if let Some(runtime) = runtime.as_ref() {
                    let snapshot = DesktopProtocolEnvelope::new(
                        next_worker_sequence,
                        DesktopWorkerToHostMessage::Snapshot(DesktopSnapshotResponse {
                            request_id,
                            snapshot: runtime.snapshot(),
                        }),
                    );
                    next_worker_sequence += 1;
                    write_desktop_ipc_frame(&mut stdout, &snapshot)
                        .context("failed to write worker snapshot response")?;
                } else {
                    desktop_log::info(format_args!(
                        "jcode-desktop: app worker received snapshot request {request_id} before initialization"
                    ));
                }
            }
            DesktopHostToWorkerMessage::Shutdown {
                reason:
                    DesktopWorkerShutdownReason::HostExit
                    | DesktopWorkerShutdownReason::Reload
                    | DesktopWorkerShutdownReason::ProtocolMismatch,
            } => break,
            DesktopHostToWorkerMessage::Input(input) => {
                let mut changed = false;
                match input {
                    DesktopInputEvent::Key(key) => {
                        if key.pressed
                            && let Some(runtime) = runtime.as_mut()
                        {
                            let outcome =
                                runtime.handle_key_input(desktop_key_event_to_key_input(&key));
                            runtime
                                .driver_mut()
                                .service_pending_transcript_hydration_blocking();
                            if matches!(outcome, KeyOutcome::ForceReload) {
                                let reload_requested = DesktopProtocolEnvelope::new(
                                    next_worker_sequence,
                                    DesktopWorkerToHostMessage::ReloadRequested,
                                );
                                next_worker_sequence += 1;
                                write_desktop_ipc_frame(&mut stdout, &reload_requested)
                                    .context("failed to write worker reload request")?;
                            } else {
                                changed = true;
                            }
                        }
                    }
                    DesktopInputEvent::Window(DesktopWindowEvent::Resized {
                        width,
                        height,
                        scale_factor,
                    }) => {
                        latest_window.width = width;
                        latest_window.height = height;
                        latest_window.scale_factor = scale_factor;
                        changed = true;
                    }
                    DesktopInputEvent::Window(DesktopWindowEvent::Focused(focused)) => {
                        latest_window.focused = focused;
                    }
                    DesktopInputEvent::Mouse(_) => {}
                }
                if changed && let Some(runtime) = runtime.as_ref() {
                    write_worker_scene_update(
                        &mut stdout,
                        &mut next_worker_sequence,
                        runtime,
                        &latest_window,
                    )
                    .context("failed to write worker input scene")?;
                }
            }
            DesktopHostToWorkerMessage::SessionEvents(batch) => {
                let mut changed = false;
                if let Some(runtime) = runtime.as_mut() {
                    for event in batch.events {
                        if let Some(session_event) =
                            desktop_wire_session_event_to_runtime_event(event)
                        {
                            runtime.apply_session_event(session_event);
                            changed = true;
                        }
                    }
                }
                if changed && let Some(runtime) = runtime.as_ref() {
                    write_worker_scene_update(
                        &mut stdout,
                        &mut next_worker_sequence,
                        runtime,
                        &latest_window,
                    )
                    .context("failed to write worker session event scene")?;
                }
            }
            DesktopHostToWorkerMessage::MetricsAck { .. } => {}
        }
    }

    Ok(())
}

#[cfg(test)]
fn desktop_scene_for_worker_init(init: &DesktopWorkerInit) -> DesktopScene {
    let mut scene = DesktopScene::new(DesktopSceneViewport::new(
        init.window.width as f32,
        init.window.height as f32,
        init.window.scale_factor,
    ));
    scene.metadata.title = init
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.title.clone());
    scene.metadata.content_ready = init.snapshot.is_some();
    scene.push(DesktopDisplayCommand::Clear(DesktopColor::rgba(
        0.02, 0.024, 0.03, 1.0,
    )));
    scene
}

fn desktop_scene_for_worker_runtime(
    runtime: &DesktopAppRuntime<DesktopApp>,
    window: &DesktopWindowState,
) -> DesktopScene {
    let mut scene = DesktopScene::new(DesktopSceneViewport::new(
        window.width as f32,
        window.height as f32,
        window.scale_factor,
    ));
    scene.push(DesktopDisplayCommand::Clear(DesktopColor::rgba(
        0.02, 0.024, 0.03, 1.0,
    )));
    runtime.build_scene(scene)
}

fn write_worker_scene_update(
    stdout: &mut impl Write,
    next_worker_sequence: &mut u64,
    runtime: &DesktopAppRuntime<DesktopApp>,
    window: &DesktopWindowState,
) -> Result<()> {
    let scene = desktop_scene_for_worker_runtime(runtime, window);
    let scene_update = DesktopProtocolEnvelope::new(
        *next_worker_sequence,
        DesktopWorkerToHostMessage::Scene(DesktopSceneUpdate {
            animation_active: scene.metadata.animation_active,
            scene,
        }),
    );
    *next_worker_sequence += 1;
    write_desktop_ipc_frame(stdout, &scene_update)?;
    Ok(())
}

fn desktop_key_event_to_key_input(event: &DesktopKeyEvent) -> KeyInput {
    let modifiers = desktop_key_modifiers_to_winit(event.modifiers);
    let key = desktop_key_string_to_winit_key(&event.key, event.text.as_deref());
    to_key_input(&key, modifiers)
}

fn desktop_key_modifiers_to_winit(modifiers: DesktopKeyModifiers) -> ModifiersState {
    let mut state = ModifiersState::empty();
    if modifiers.shift {
        state |= ModifiersState::SHIFT;
    }
    if modifiers.ctrl {
        state |= ModifiersState::CONTROL;
    }
    if modifiers.alt {
        state |= ModifiersState::ALT;
    }
    if modifiers.super_key {
        state |= ModifiersState::SUPER;
    }
    state
}

fn desktop_key_string_to_winit_key(key: &str, text: Option<&str>) -> Key {
    match key {
        "Escape" => Key::Named(NamedKey::Escape),
        "Enter" => Key::Named(NamedKey::Enter),
        "Tab" => Key::Named(NamedKey::Tab),
        "Backspace" => Key::Named(NamedKey::Backspace),
        "Delete" => Key::Named(NamedKey::Delete),
        "PageUp" => Key::Named(NamedKey::PageUp),
        "PageDown" => Key::Named(NamedKey::PageDown),
        "ArrowUp" => Key::Named(NamedKey::ArrowUp),
        "ArrowDown" => Key::Named(NamedKey::ArrowDown),
        "ArrowLeft" => Key::Named(NamedKey::ArrowLeft),
        "ArrowRight" => Key::Named(NamedKey::ArrowRight),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        "Space" => Key::Named(NamedKey::Space),
        _ => Key::Character(text.unwrap_or(key).to_string().into()),
    }
}

fn desktop_wire_session_event_to_runtime_event(
    event: DesktopSessionEventWire,
) -> Option<session_launch::DesktopSessionEvent> {
    match event {
        DesktopSessionEventWire::Status { message } => Some(
            session_launch::DesktopSessionEvent::Status(DesktopSessionStatus::external(message)),
        ),
        DesktopSessionEventWire::AssistantTextDelta { text } => {
            Some(session_launch::DesktopSessionEvent::TextDelta(text))
        }
        DesktopSessionEventWire::ToolStarted { id, title } => {
            Some(session_launch::DesktopSessionEvent::ToolStarted {
                id: (!id.is_empty()).then_some(id),
                name: title,
            })
        }
        DesktopSessionEventWire::ToolFinished { id, title, success } => {
            Some(session_launch::DesktopSessionEvent::ToolFinished {
                id: (!id.is_empty()).then_some(id),
                name: title,
                summary: String::new(),
                is_error: !success,
            })
        }
        DesktopSessionEventWire::Error { message } => {
            Some(session_launch::DesktopSessionEvent::Error(message))
        }
        DesktopSessionEventWire::RawJson { .. } => None,
    }
}

fn desktop_mode_from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> DesktopMode {
    if args.into_iter().any(|arg| arg == "--workspace") {
        DesktopMode::WorkspacePrototype
    } else {
        DesktopMode::SingleSession
    }
}

fn desktop_process_role_from_args<'a>(
    args: impl IntoIterator<Item = &'a str>,
) -> DesktopProcessRole {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let role = arg
            .strip_prefix("--desktop-process-role=")
            .or_else(|| {
                (arg == "--desktop-process-role")
                    .then(|| args.next())
                    .flatten()
            })
            .or_else(|| {
                (arg == "--desktop-host")
                    .then_some("host")
                    .or_else(|| (arg == "--desktop-app-worker").then_some("worker"))
            });
        if let Some(role) = role {
            return match role {
                "host" | "stable-host" | "stable_host" => DesktopProcessRole::StableHost,
                "worker" | "app-worker" | "app_worker" => DesktopProcessRole::AppWorker,
                _ => DesktopProcessRole::Standalone,
            };
        }
    }
    DesktopProcessRole::StableHost
}

fn desktop_resume_session_id_from_args<'a>(
    args: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--resume" {
            return args.next().map(str::to_string);
        }
        if let Some(session_id) = arg.strip_prefix("--resume=") {
            return (!session_id.is_empty()).then(|| session_id.to_string());
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DesktopReloadWindowPlacement {
    position: Option<PhysicalPosition<i32>>,
    inner_size: PhysicalSize<u32>,
}

impl DesktopReloadWindowPlacement {
    fn from_window(window: &Window) -> Option<Self> {
        let inner_size = window.inner_size();
        if !desktop_reload_window_size_is_valid(inner_size) {
            return None;
        }
        Some(Self {
            position: window.outer_position().ok(),
            inner_size,
        })
    }

    fn from_env_value(raw: &str) -> Option<Self> {
        let parts = raw.split(',').collect::<Vec<_>>();
        if parts.len() != 4 {
            return None;
        }

        let position = match (parts[0], parts[1]) {
            ("_", "_") => None,
            (x, y) => Some(PhysicalPosition::new(x.parse().ok()?, y.parse().ok()?)),
        };
        let inner_size = PhysicalSize::new(parts[2].parse().ok()?, parts[3].parse().ok()?);
        if !desktop_reload_window_size_is_valid(inner_size) {
            return None;
        }
        Some(Self {
            position,
            inner_size,
        })
    }

    fn to_env_value(self) -> String {
        let (x, y) = match self.position {
            Some(position) => (position.x.to_string(), position.y.to_string()),
            None => ("_".to_string(), "_".to_string()),
        };
        format!(
            "{x},{y},{},{}",
            self.inner_size.width, self.inner_size.height
        )
    }

    fn apply_to_window_builder(self, mut window_builder: WindowBuilder) -> WindowBuilder {
        window_builder = window_builder.with_inner_size(self.inner_size);
        if let Some(position) = self.position {
            window_builder = window_builder.with_position(position);
        }
        window_builder
    }
}

fn desktop_reload_window_size_is_valid(size: PhysicalSize<u32>) -> bool {
    (1..=DESKTOP_RELOAD_MAX_RESTORED_DIMENSION).contains(&size.width)
        && (1..=DESKTOP_RELOAD_MAX_RESTORED_DIMENSION).contains(&size.height)
}

#[derive(Clone, Debug, Default)]
struct DesktopReloadStartup {
    window_placement: Option<DesktopReloadWindowPlacement>,
    handoff: Option<DesktopReloadStartupHandoff>,
}

impl DesktopReloadStartup {
    fn from_env() -> Self {
        let raw_window_placement = std::env::var(DESKTOP_RELOAD_WINDOW_ENV).ok();
        let ready_file = std::env::var_os(DESKTOP_RELOAD_HANDOFF_READY_ENV).map(PathBuf::from);
        let release_file = std::env::var_os(DESKTOP_RELOAD_HANDOFF_RELEASE_ENV).map(PathBuf::from);
        unsafe {
            std::env::remove_var(DESKTOP_RELOAD_WINDOW_ENV);
            std::env::remove_var(DESKTOP_RELOAD_HANDOFF_READY_ENV);
            std::env::remove_var(DESKTOP_RELOAD_HANDOFF_RELEASE_ENV);
        }

        let window_placement = raw_window_placement.as_deref().and_then(|raw| {
            let placement = DesktopReloadWindowPlacement::from_env_value(raw);
            if placement.is_none() {
                desktop_log::warn(format_args!(
                    "jcode-desktop: ignoring invalid reload window placement {raw:?}"
                ));
            }
            placement
        });
        let handoff = match (ready_file, release_file) {
            (Some(ready_file), Some(release_file)) => Some(DesktopReloadStartupHandoff {
                ready_file,
                release_file,
            }),
            (None, None) => None,
            _ => {
                desktop_log::warn(format_args!(
                    "jcode-desktop: ignoring incomplete reload handoff environment"
                ));
                None
            }
        };

        Self {
            window_placement,
            handoff,
        }
    }

    fn hidden_until_handoff_release(&self) -> bool {
        self.handoff.is_some()
    }
}

#[derive(Clone, Debug)]
struct DesktopReloadStartupHandoff {
    ready_file: PathBuf,
    release_file: PathBuf,
}

impl DesktopReloadStartupHandoff {
    fn signal_ready_and_wait_for_release(&self) {
        if let Err(error) = write_desktop_reload_marker(&self.ready_file) {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to signal reload readiness: {error:#}"
            ));
            return;
        }

        desktop_log::info(format_args!(
            "jcode-desktop: reload child ready, waiting for parent release"
        ));
        let deadline = Instant::now() + DESKTOP_RELOAD_STARTUP_RELEASE_TIMEOUT;
        while Instant::now() < deadline {
            if self.release_file.exists() {
                cleanup_desktop_reload_handoff_files(&self.ready_file, &self.release_file);
                return;
            }
            std::thread::sleep(DESKTOP_RELOAD_HANDOFF_POLL_INTERVAL);
        }

        desktop_log::warn(format_args!(
            "jcode-desktop: reload parent did not release handoff within {}ms; showing replacement window anyway",
            DESKTOP_RELOAD_STARTUP_RELEASE_TIMEOUT.as_millis()
        ));
        cleanup_desktop_reload_handoff_files(&self.ready_file, &self.release_file);
    }
}

#[derive(Clone, Debug)]
struct DesktopReloadHandoff {
    ready_file: PathBuf,
    release_file: PathBuf,
    window_placement: Option<DesktopReloadWindowPlacement>,
}

impl DesktopReloadHandoff {
    fn new(window: &Window) -> Result<Self> {
        let dir = desktop_reload_handoff_temp_dir();
        fs::create_dir_all(&dir).with_context(|| {
            format!(
                "failed to create desktop reload handoff directory {}",
                dir.display()
            )
        })?;
        Ok(Self {
            ready_file: dir.join("ready"),
            release_file: dir.join("release"),
            window_placement: DesktopReloadWindowPlacement::from_window(window),
        })
    }

    fn apply_to_command(&self, command: &mut Command) {
        if let Some(placement) = self.window_placement {
            command.env(DESKTOP_RELOAD_WINDOW_ENV, placement.to_env_value());
        }
        command.env(DESKTOP_RELOAD_HANDOFF_READY_ENV, &self.ready_file);
        command.env(DESKTOP_RELOAD_HANDOFF_RELEASE_ENV, &self.release_file);
    }

    fn watcher(&self) -> DesktopReloadHandoffWatcher {
        DesktopReloadHandoffWatcher {
            ready_file: self.ready_file.clone(),
            release_file: self.release_file.clone(),
            spawned_at: Instant::now(),
        }
    }

    fn cleanup(&self) {
        cleanup_desktop_reload_handoff_files(&self.ready_file, &self.release_file);
    }
}

#[derive(Clone, Debug)]
struct DesktopReloadHandoffWatcher {
    ready_file: PathBuf,
    release_file: PathBuf,
    spawned_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopReloadHandoffPoll {
    Waiting,
    Ready,
    TimedOut,
}

impl DesktopReloadHandoffWatcher {
    fn poll(&self) -> Result<DesktopReloadHandoffPoll> {
        if self.ready_file.exists() {
            write_desktop_reload_marker(&self.release_file)?;
            return Ok(DesktopReloadHandoffPoll::Ready);
        }
        if self.spawned_at.elapsed() >= DESKTOP_RELOAD_HANDOFF_TIMEOUT {
            return Ok(DesktopReloadHandoffPoll::TimedOut);
        }
        Ok(DesktopReloadHandoffPoll::Waiting)
    }

    fn cleanup(&self) {
        cleanup_desktop_reload_handoff_files(&self.ready_file, &self.release_file);
    }
}

fn desktop_reload_handoff_temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "jcode-desktop-reload-{}-{nonce}",
        std::process::id()
    ))
}

fn write_desktop_reload_marker(path: &Path) -> Result<()> {
    fs::write(path, format!("{}\n", std::process::id()))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn cleanup_desktop_reload_handoff_files(ready_file: &Path, release_file: &Path) {
    let _ = fs::remove_file(ready_file);
    let _ = fs::remove_file(release_file);
    if ready_file.parent() == release_file.parent()
        && let Some(parent) = ready_file.parent()
        && parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("jcode-desktop-reload-"))
    {
        let _ = fs::remove_dir(parent);
    }
}

struct DesktopHotReloader {
    relaunch: Option<DesktopRelaunch>,
    strategy: DesktopReloadStrategy,
    observed_modified: Option<std::time::SystemTime>,
    last_checked: Instant,
    pending_handoff: Option<DesktopReloadHandoffWatcher>,
    app_worker: Option<DesktopWorkerConnection>,
}

#[derive(Default)]
struct DesktopWorkerDrain {
    latest_scene: Option<DesktopScene>,
    reload_requested: bool,
}

impl DesktopHotReloader {
    const CHECK_INTERVAL: Duration = Duration::from_millis(750);

    fn new(strategy: DesktopReloadStrategy) -> Self {
        let relaunch = DesktopRelaunch::from_current_process();
        let observed_modified = relaunch.as_ref().and_then(|relaunch| {
            binary_modified_time(&desktop_reload_binary_candidate(&relaunch.binary))
        });
        Self {
            relaunch,
            strategy,
            observed_modified,
            last_checked: Instant::now(),
            pending_handoff: None,
            app_worker: None,
        }
    }

    fn next_wake(&self, now: Instant) -> Option<Instant> {
        if self.pending_handoff.is_some() {
            return Some(now + DESKTOP_RELOAD_HANDOFF_POLL_INTERVAL);
        }
        if self.app_worker.is_some() {
            return Some(now + DESKTOP_RELOAD_HANDOFF_POLL_INTERVAL);
        }
        self.relaunch.as_ref()?;
        Some(std::cmp::max(now, self.last_checked + Self::CHECK_INTERVAL))
    }

    fn drain_app_worker_messages(&mut self) -> DesktopWorkerDrain {
        let Some(worker) = self.app_worker.as_mut() else {
            return DesktopWorkerDrain::default();
        };
        let mut drained = DesktopWorkerDrain::default();
        let mut should_drop_worker = false;
        while let Some(message) = worker.try_recv() {
            match message {
                Ok(DesktopWorkerToHostMessage::Ready(ready)) => {
                    desktop_log::info(format_args!(
                        "jcode-desktop: app worker ready; pid={} mode={:?}",
                        ready.worker_pid, ready.mode
                    ));
                }
                Ok(DesktopWorkerToHostMessage::Scene(scene_update)) => {
                    drained.latest_scene = Some(scene_update.scene);
                }
                Ok(DesktopWorkerToHostMessage::ReloadRequested) => {
                    drained.reload_requested = true;
                }
                Ok(DesktopWorkerToHostMessage::Snapshot(snapshot)) => {
                    desktop_log::info(format_args!(
                        "jcode-desktop: app worker snapshot response {}; mode={}",
                        snapshot.request_id, snapshot.snapshot.mode
                    ));
                }
                Ok(DesktopWorkerToHostMessage::Metrics(metrics)) => {
                    desktop_log::info(format_args!(
                        "jcode-desktop: app worker reported {} metric(s)",
                        metrics.metrics.len()
                    ));
                }
                Ok(DesktopWorkerToHostMessage::Log(log)) => {
                    desktop_log::info(format_args!(
                        "jcode-desktop: app worker log {:?}: {}",
                        log.level, log.message
                    ));
                }
                Ok(DesktopWorkerToHostMessage::Exited(exit)) => {
                    desktop_log::warn(format_args!(
                        "jcode-desktop: app worker exited code={:?} reason={:?}",
                        exit.code, exit.reason
                    ));
                }
                Err(error) => {
                    desktop_log::error(format_args!(
                        "jcode-desktop: failed to read app worker message: {error:#}"
                    ));
                    should_drop_worker = true;
                    break;
                }
            }
        }
        if !should_drop_worker {
            match worker.try_wait() {
                Ok(Some(status)) => {
                    desktop_log::warn(format_args!(
                        "jcode-desktop: app worker process exited unexpectedly: {status}"
                    ));
                    should_drop_worker = true;
                }
                Ok(None) => {}
                Err(error) => {
                    desktop_log::warn(format_args!(
                        "jcode-desktop: failed to poll app worker process: {error:#}"
                    ));
                    should_drop_worker = true;
                }
            }
        }
        if should_drop_worker
            && let Some(worker) = self.app_worker.take()
            && let Err(error) = worker.kill()
        {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to clean up stopped app worker: {error:#}"
            ));
        }
        drained
    }

    fn has_app_worker(&self) -> bool {
        self.app_worker.is_some()
    }

    fn send_app_worker_input(&mut self, input: DesktopInputEvent) -> Result<()> {
        self.send_app_worker_message(DesktopHostToWorkerMessage::Input(input))
    }

    fn send_app_worker_message(&mut self, message: DesktopHostToWorkerMessage) -> Result<()> {
        let Some(worker) = self.app_worker.as_mut() else {
            return Ok(());
        };
        worker.send(message)
    }

    fn start_app_worker_for_current_binary(
        &mut self,
        app: &DesktopApp,
        window: &Window,
        reason: &'static str,
    ) {
        let Some(relaunch) = self.relaunch.clone() else {
            desktop_log::warn(format_args!(
                "jcode-desktop: cannot start app worker for {reason}; current process cannot be relaunched"
            ));
            return;
        };
        let binary = desktop_reload_binary_candidate(&relaunch.binary);
        self.restart_app_worker(app, window, &relaunch, binary, reason);
    }

    fn poll(&mut self, app: &DesktopApp, window: &Window) -> bool {
        if self.poll_pending_handoff() {
            return true;
        }
        if self.pending_handoff.is_some() {
            return false;
        }
        if self.last_checked.elapsed() < Self::CHECK_INTERVAL {
            return false;
        }
        self.last_checked = Instant::now();

        let Some(relaunch) = self.relaunch.clone() else {
            return false;
        };
        let binary = desktop_reload_binary_candidate(&relaunch.binary);
        let Some(current_modified) = binary_modified_time(&binary) else {
            return false;
        };
        let observed_modified = self.observed_modified;
        self.observed_modified = Some(current_modified);
        if observed_modified.is_some_and(|observed| current_modified > observed) {
            return self.reload_with_strategy(app, window, &relaunch, binary, "hot reload");
        }
        false
    }

    fn force_reload(&mut self, app: &DesktopApp, window: &Window) -> bool {
        if self.poll_pending_handoff() {
            return true;
        }
        if self.pending_handoff.is_some() {
            desktop_log::warn(format_args!(
                "jcode-desktop: force reload requested while another reload handoff is pending"
            ));
            return false;
        }
        let Some(relaunch) = self.relaunch.clone() else {
            desktop_log::warn(format_args!(
                "jcode-desktop: force reload requested but current process cannot be relaunched"
            ));
            return false;
        };
        let binary = desktop_reload_binary_candidate(&relaunch.binary);
        self.reload_with_strategy(app, window, &relaunch, binary, "force reload")
    }

    fn reload_with_strategy(
        &mut self,
        app: &DesktopApp,
        window: &Window,
        relaunch: &DesktopRelaunch,
        binary: PathBuf,
        reason: &'static str,
    ) -> bool {
        match self.strategy {
            DesktopReloadStrategy::FullProcessHandoff => {
                self.reload_full_process_handoff(app, window, relaunch, binary, reason)
            }
            DesktopReloadStrategy::AppWorkerRestart => {
                desktop_log::info(format_args!(
                    "jcode-desktop: {reason} requested app-worker restart; keeping stable host window alive"
                ));
                self.restart_app_worker(app, window, relaunch, binary, reason);
                false
            }
        }
    }

    fn restart_app_worker(
        &mut self,
        app: &DesktopApp,
        window: &Window,
        relaunch: &DesktopRelaunch,
        binary: PathBuf,
        reason: &'static str,
    ) {
        if let Some(worker) = self.app_worker.take()
            && let Err(error) = worker.kill()
        {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to stop previous app worker before {reason}: {error:#}"
            ));
        }

        let worker_relaunch = relaunch.for_app(app, binary).for_app_worker();
        match worker_relaunch.spawn_app_worker() {
            Ok(mut worker) => {
                if let Err(error) =
                    worker.send(DesktopHostToWorkerMessage::Initialize(DesktopWorkerInit {
                        mode: desktop_worker_mode_for_app(app),
                        snapshot: Some(app.snapshot()),
                        window: desktop_window_state(window),
                    }))
                {
                    desktop_log::error(format_args!(
                        "jcode-desktop: failed to initialize app worker for {reason}: {error:#}"
                    ));
                    if let Err(kill_error) = worker.kill() {
                        desktop_log::warn(format_args!(
                            "jcode-desktop: failed to kill uninitialized app worker: {kill_error:#}"
                        ));
                    }
                    return;
                }
                desktop_log::info(format_args!(
                    "jcode-desktop: app worker restarted for {reason}; pid={}",
                    worker.child_id()
                ));
                self.app_worker = Some(worker);
            }
            Err(error) => desktop_log::error(format_args!(
                "jcode-desktop: failed to restart app worker for {reason}: {error:#}"
            )),
        }
    }

    fn reload_full_process_handoff(
        &mut self,
        app: &DesktopApp,
        window: &Window,
        relaunch: &DesktopRelaunch,
        binary: PathBuf,
        reason: &'static str,
    ) -> bool {
        let relaunch = relaunch.for_app(app, binary);
        match relaunch.spawn_for_window(window) {
            Ok(Some(handoff)) => {
                self.pending_handoff = Some(handoff);
                false
            }
            Ok(None) => true,
            Err(error) => {
                desktop_log::error(format_args!(
                    "jcode-desktop: failed to {reason} desktop: {error:#}"
                ));
                false
            }
        }
    }

    fn poll_pending_handoff(&mut self) -> bool {
        let Some(pending_handoff) = self.pending_handoff.as_ref() else {
            return false;
        };
        match pending_handoff.poll() {
            Ok(DesktopReloadHandoffPoll::Waiting) => false,
            Ok(DesktopReloadHandoffPoll::Ready) => {
                desktop_log::info(format_args!(
                    "jcode-desktop: reload replacement is ready; exiting old process"
                ));
                true
            }
            Ok(DesktopReloadHandoffPoll::TimedOut) => {
                desktop_log::warn(format_args!(
                    "jcode-desktop: reload replacement did not become ready within {}ms; keeping old process alive",
                    DESKTOP_RELOAD_HANDOFF_TIMEOUT.as_millis()
                ));
                if let Some(pending_handoff) = self.pending_handoff.take() {
                    pending_handoff.cleanup();
                }
                false
            }
            Err(error) => {
                desktop_log::error(format_args!(
                    "jcode-desktop: failed to release reload replacement: {error:#}"
                ));
                true
            }
        }
    }
}

fn desktop_worker_mode_for_app(app: &DesktopApp) -> DesktopWorkerMode {
    match app {
        DesktopApp::SingleSession(_) => DesktopWorkerMode::SingleSession,
        DesktopApp::Workspace(_) => DesktopWorkerMode::Workspace,
    }
}

fn desktop_window_state(window: &Window) -> DesktopWindowState {
    let size = window.inner_size();
    DesktopWindowState {
        width: size.width,
        height: size.height,
        scale_factor: window.scale_factor() as f32,
        focused: window.has_focus(),
    }
}

#[derive(Clone, Debug)]
struct DesktopRelaunch {
    binary: PathBuf,
    args: Vec<OsString>,
}

impl DesktopRelaunch {
    fn from_current_process() -> Option<Self> {
        let mut args = std::env::args_os();
        let argv0 = args.next()?;
        let binary = match resolve_invoked_binary(&argv0) {
            Some(binary) => binary,
            None => match std::env::current_exe() {
                Ok(binary) => binary,
                Err(_) => return None,
            },
        };
        Some(Self {
            binary,
            args: args.collect(),
        })
    }

    fn spawn_for_window(&self, window: &Window) -> Result<Option<DesktopReloadHandoffWatcher>> {
        let handoff = match DesktopReloadHandoff::new(window) {
            Ok(handoff) => Some(handoff),
            Err(error) => {
                desktop_log::warn(format_args!(
                    "jcode-desktop: reload handoff unavailable, falling back to immediate relaunch: {error:#}"
                ));
                None
            }
        };
        desktop_log::info(format_args!(
            "jcode-desktop: hot reloading into {} with args {:?}{}",
            self.binary.display(),
            self.args,
            if handoff.is_some() {
                " using handoff"
            } else {
                ""
            }
        ));
        let mut command = Command::new(&self.binary);
        command.args(&self.args);
        command.env_remove(DESKTOP_RELOAD_WINDOW_ENV);
        command.env_remove(DESKTOP_RELOAD_HANDOFF_READY_ENV);
        command.env_remove(DESKTOP_RELOAD_HANDOFF_RELEASE_ENV);
        if let Some(handoff) = handoff.as_ref() {
            handoff.apply_to_command(&mut command);
        }
        if let Err(error) = command.spawn() {
            if let Some(handoff) = handoff.as_ref() {
                handoff.cleanup();
            }
            return Err(error)
                .with_context(|| format!("failed to spawn {}", self.binary.display()));
        }
        Ok(handoff.as_ref().map(DesktopReloadHandoff::watcher))
    }

    fn for_app(&self, app: &DesktopApp, binary: PathBuf) -> Self {
        if let DesktopApp::Workspace(workspace) = app
            && let Err(error) = desktop_prefs::save_preferences(&workspace.preferences())
        {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to persist workspace state before hot reload: {error:#}"
            ));
        }

        let mut args = desktop_args_without_resume(&self.args);
        match app {
            DesktopApp::Workspace(_) => ensure_desktop_workspace_arg(&mut args),
            DesktopApp::SingleSession(_) => {
                if let Some(session_id) = app.single_session_live_id() {
                    args.push(OsString::from("--resume"));
                    args.push(OsString::from(session_id));
                }
            }
        }
        Self { binary, args }
    }

    fn for_app_worker(&self) -> Self {
        let mut args = desktop_args_without_process_role(&self.args);
        args.push(OsString::from("--desktop-process-role"));
        args.push(OsString::from("app-worker"));
        Self {
            binary: self.binary.clone(),
            args,
        }
    }

    fn spawn_app_worker(&self) -> Result<DesktopWorkerConnection> {
        desktop_log::info(format_args!(
            "jcode-desktop: spawning app worker {} with args {:?}",
            self.binary.display(),
            self.args
        ));
        let mut command = Command::new(&self.binary);
        command.args(&self.args);
        command.env_remove(DESKTOP_RELOAD_WINDOW_ENV);
        command.env_remove(DESKTOP_RELOAD_HANDOFF_READY_ENV);
        command.env_remove(DESKTOP_RELOAD_HANDOFF_RELEASE_ENV);
        DesktopWorkerConnection::spawn(&mut command)
            .with_context(|| format!("failed to spawn app worker {}", self.binary.display()))
    }
}

fn ensure_desktop_workspace_arg(args: &mut Vec<OsString>) {
    let has_mode_arg = args.iter().any(|arg| {
        arg == "--workspace"
            || arg == "--new"
            || arg == "--resume"
            || arg.to_str().is_some_and(|value| {
                value.starts_with("--resume=") || value.starts_with("jcode://")
            })
    });
    if !has_mode_arg {
        args.push(OsString::from("--workspace"));
    }
}

fn desktop_args_without_resume(args: &[OsString]) -> Vec<OsString> {
    let mut filtered = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--resume" {
            skip_next = true;
            continue;
        }
        if arg
            .to_str()
            .is_some_and(|value| value.starts_with("--resume="))
        {
            continue;
        }
        filtered.push(arg.clone());
    }
    filtered
}

fn desktop_args_without_process_role(args: &[OsString]) -> Vec<OsString> {
    let mut filtered = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--desktop-process-role" {
            skip_next = true;
            continue;
        }
        if arg == "--desktop-host" || arg == "--desktop-app-worker" {
            continue;
        }
        if arg
            .to_str()
            .is_some_and(|value| value.starts_with("--desktop-process-role="))
        {
            continue;
        }
        filtered.push(arg.clone());
    }
    filtered
}

fn desktop_key_event_from_winit(
    key: &Key,
    modifiers: ModifiersState,
    pressed: bool,
) -> DesktopKeyEvent {
    DesktopKeyEvent {
        key: desktop_key_name(key),
        text: desktop_key_text(key),
        pressed,
        modifiers: desktop_key_modifiers(modifiers),
    }
}

fn desktop_key_name(key: &Key) -> String {
    match key {
        Key::Character(value) => value.to_string(),
        Key::Named(named) => format!("{named:?}"),
        other => format!("{other:?}"),
    }
}

fn desktop_key_text(key: &Key) -> Option<String> {
    match key {
        Key::Character(value) => Some(value.to_string()),
        _ => None,
    }
}

fn desktop_key_modifiers(modifiers: ModifiersState) -> DesktopKeyModifiers {
    DesktopKeyModifiers {
        shift: modifiers.shift_key(),
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key(),
        super_key: modifiers.super_key(),
    }
}

fn desktop_mouse_wheel_event(delta: MouseScrollDelta) -> DesktopMouseEvent {
    let (delta_x, delta_y) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (x, y),
        MouseScrollDelta::PixelDelta(position) => (position.x as f32, position.y as f32),
    };
    DesktopMouseEvent::Wheel { delta_x, delta_y }
}

fn forward_app_worker_input(hot_reloader: &mut DesktopHotReloader, input: DesktopInputEvent) {
    if let Err(error) = hot_reloader.send_app_worker_input(input) {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to forward input to app worker: {error:#}"
        ));
    }
}

fn forward_desktop_session_event_batch_to_worker(
    hot_reloader: &mut DesktopHotReloader,
    batch: &DesktopSessionEventBatch,
) {
    if !hot_reloader.has_app_worker() {
        return;
    }
    let wire = DesktopSessionEventBatchWire {
        events: batch
            .events
            .iter()
            .map(desktop_session_event_to_wire)
            .collect(),
        raw_event_count: batch.raw_event_count,
        raw_payload_bytes: batch.raw_payload_bytes,
    };
    if let Err(error) =
        hot_reloader.send_app_worker_message(DesktopHostToWorkerMessage::SessionEvents(wire))
    {
        desktop_log::error(format_args!(
            "jcode-desktop: failed to forward session events to app worker: {error:#}"
        ));
    }
}

fn desktop_session_event_to_wire(
    event: &session_launch::DesktopSessionEvent,
) -> DesktopSessionEventWire {
    match event {
        session_launch::DesktopSessionEvent::Status(status) => DesktopSessionEventWire::Status {
            message: status.label(),
        },
        session_launch::DesktopSessionEvent::TextDelta(text)
        | session_launch::DesktopSessionEvent::TextReplace(text) => {
            DesktopSessionEventWire::AssistantTextDelta { text: text.clone() }
        }
        session_launch::DesktopSessionEvent::ToolStarted { id, name }
        | session_launch::DesktopSessionEvent::ToolExecuting { id, name } => {
            DesktopSessionEventWire::ToolStarted {
                id: id.clone().unwrap_or_default(),
                title: name.clone(),
            }
        }
        session_launch::DesktopSessionEvent::ToolFinished {
            id, name, is_error, ..
        } => DesktopSessionEventWire::ToolFinished {
            id: id.clone().unwrap_or_default(),
            title: name.clone(),
            success: !*is_error,
        },
        session_launch::DesktopSessionEvent::Error(message) => DesktopSessionEventWire::Error {
            message: message.clone(),
        },
        other => DesktopSessionEventWire::RawJson {
            event_type: desktop_session_event_type_name(other).to_string(),
            payload: format!("{other:?}"),
        },
    }
}

fn desktop_session_event_type_name(event: &session_launch::DesktopSessionEvent) -> &'static str {
    match event {
        session_launch::DesktopSessionEvent::Status(_) => "status",
        session_launch::DesktopSessionEvent::SessionStarted { .. } => "session_started",
        session_launch::DesktopSessionEvent::SessionRenamed { .. } => "session_renamed",
        session_launch::DesktopSessionEvent::TextDelta(_) => "text_delta",
        session_launch::DesktopSessionEvent::TextReplace(_) => "text_replace",
        session_launch::DesktopSessionEvent::ToolStarted { .. } => "tool_started",
        session_launch::DesktopSessionEvent::ToolExecuting { .. } => "tool_executing",
        session_launch::DesktopSessionEvent::ToolInput { .. } => "tool_input",
        session_launch::DesktopSessionEvent::ToolFinished { .. } => "tool_finished",
        session_launch::DesktopSessionEvent::ModelChanged { .. } => "model_changed",
        session_launch::DesktopSessionEvent::ModelCatalog { .. } => "model_catalog",
        session_launch::DesktopSessionEvent::ModelCatalogError { .. } => "model_catalog_error",
        session_launch::DesktopSessionEvent::StdinRequest { .. } => "stdin_request",
        session_launch::DesktopSessionEvent::ReloadProgress { .. } => "reload_progress",
        session_launch::DesktopSessionEvent::RuntimeMetadata { .. } => "runtime_metadata",
        session_launch::DesktopSessionEvent::TokenUsage { .. } => "token_usage",
        session_launch::DesktopSessionEvent::SystemNotice { .. } => "system_notice",
        session_launch::DesktopSessionEvent::SessionCloseRequested { .. } => {
            "session_close_requested"
        }
        session_launch::DesktopSessionEvent::Reloading { .. } => "reloading",
        session_launch::DesktopSessionEvent::Reloaded { .. } => "reloaded",
        session_launch::DesktopSessionEvent::Done => "done",
        session_launch::DesktopSessionEvent::Error(_) => "error",
    }
}

fn desktop_reload_binary_candidate(invoked_binary: &Path) -> PathBuf {
    let Some(repo_dir) = find_desktop_repo_dir() else {
        return invoked_binary.to_path_buf();
    };
    desktop_reload_binary_candidate_from(invoked_binary, &repo_dir)
}

fn desktop_reload_binary_candidate_from(invoked_binary: &Path, repo_dir: &Path) -> PathBuf {
    let selfdev = desktop_selfdev_binary_path(repo_dir);
    if paths_refer_to_same_file(invoked_binary, &selfdev)
        || binary_is_newer_than(&selfdev, invoked_binary)
    {
        selfdev
    } else {
        invoked_binary.to_path_buf()
    }
}

fn desktop_selfdev_binary_path(repo_dir: &Path) -> PathBuf {
    repo_dir
        .join("target")
        .join("selfdev")
        .join(desktop_binary_name())
}

fn desktop_binary_name() -> &'static str {
    if cfg!(windows) {
        "jcode-desktop.exe"
    } else {
        "jcode-desktop"
    }
}

fn binary_is_newer_than(candidate: &Path, baseline: &Path) -> bool {
    let Some(candidate_modified) = binary_modified_time(candidate) else {
        return false;
    };
    match binary_modified_time(baseline) {
        Some(baseline_modified) => candidate_modified > baseline_modified,
        None => true,
    }
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn find_desktop_repo_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    find_desktop_repo_in_ancestors(&manifest_dir)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| find_desktop_repo_in_ancestors(&path))
        })
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| find_desktop_repo_in_ancestors(&path))
        })
}

fn find_desktop_repo_in_ancestors(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|candidate| is_jcode_desktop_repo(candidate))
        .map(Path::to_path_buf)
}

fn is_jcode_desktop_repo(candidate: &Path) -> bool {
    if !candidate.join("crates/jcode-desktop/Cargo.toml").is_file() {
        return false;
    }
    std::fs::read_to_string(candidate.join("Cargo.toml"))
        .map(|cargo_toml| cargo_toml.contains("name = \"jcode\""))
        .unwrap_or(false)
}

fn binary_modified_time(path: &Path) -> Option<std::time::SystemTime> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    metadata.modified().ok()
}

fn resolve_invoked_binary(argv0: &OsString) -> Option<PathBuf> {
    let path = PathBuf::from(argv0);
    if path.components().count() > 1 {
        return Some(path);
    }

    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(&path))
        .find(|candidate| candidate.is_file())
}

#[allow(clippy::large_enum_variant)]
enum DesktopApp {
    SingleSession(SingleSessionApp),
    Workspace(Workspace),
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopAppDebugSnapshot {
    mode: &'static str,
    title: String,
    live_session_id: Option<String>,
    status: Option<String>,
    is_processing: bool,
    body_text: String,
}

impl DesktopApp {
    fn mode(&self) -> &'static str {
        match self {
            Self::SingleSession(_) => "single_session",
            Self::Workspace(_) => "workspace",
        }
    }

    fn is_single_session(&self) -> bool {
        matches!(self, Self::SingleSession(_))
    }

    fn is_workspace(&self) -> bool {
        matches!(self, Self::Workspace(_))
    }

    fn has_background_work(&self) -> bool {
        matches!(self, Self::SingleSession(app) if app.has_background_work())
    }

    fn has_frame_animation(&self) -> bool {
        matches!(self, Self::SingleSession(app) if app.has_frame_animation())
    }

    fn status_title(&self) -> String {
        match self {
            Self::SingleSession(app) => app.status_title(),
            Self::Workspace(workspace) => workspace.status_title(),
        }
    }

    fn handle_key(&mut self, key: KeyInput) -> KeyOutcome {
        match self {
            Self::SingleSession(app) => app.handle_key(key),
            Self::Workspace(workspace) => workspace.handle_key(key),
        }
    }

    fn promote_focused_workspace_session(&mut self) -> bool {
        let Self::Workspace(workspace) = self else {
            return false;
        };
        let Some(card) = workspace.focused_session_card() else {
            return false;
        };
        let session_id = card.session_id.clone();
        let mut single_session = SingleSessionApp::new(Some(card));
        single_session.initialize_resumed_session(&session_id);
        single_session.request_transcript_hydration(&session_id);
        *self = Self::SingleSession(single_session);
        true
    }

    /// Take the session id queued for off-thread transcript hydration.
    fn take_pending_transcript_hydration(&mut self) -> Option<String> {
        match self {
            Self::SingleSession(app) => app.take_pending_transcript_hydration(),
            Self::Workspace(_) => None,
        }
    }

    /// Apply a transcript that finished loading off the UI thread.
    fn apply_hydrated_transcript(
        &mut self,
        session_id: &str,
        result: std::result::Result<Option<Vec<workspace::SessionTranscriptMessage>>, String>,
    ) -> bool {
        match self {
            Self::SingleSession(app) => app.apply_hydrated_transcript(session_id, result),
            Self::Workspace(_) => false,
        }
    }

    /// Service any queued transcript hydration synchronously. Used by the
    /// app-worker process, which has no event-loop proxy; the disk scan is
    /// bounded so the worst case stays small.
    fn service_pending_transcript_hydration_blocking(&mut self) {
        if let Self::SingleSession(app) = self
            && let Some(session_id) = app.take_pending_transcript_hydration()
        {
            app.hydrate_resumed_session_from_disk(&session_id);
        }
    }

    fn apply_session_event(&mut self, event: session_launch::DesktopSessionEvent) {
        if let Self::SingleSession(app) = self {
            app.apply_session_event(event);
        }
    }

    fn set_single_session_status_label(&mut self, label: impl Into<String>) {
        if let Self::SingleSession(app) = self {
            app.set_status_label(label);
        }
    }

    fn take_github_issue_sync_request(&mut self) -> bool {
        match self {
            Self::SingleSession(app) => app.take_github_issue_sync_request(),
            Self::Workspace(_) => false,
        }
    }

    fn note_github_issue_sync_already_running(&mut self) {
        if let Self::SingleSession(app) = self {
            app.note_github_issue_sync_already_running();
        }
    }

    fn apply_github_issue_sync_result(
        &mut self,
        result: std::result::Result<desktop_issue_cache::GitHubIssueSyncSummary, String>,
    ) {
        if let Self::SingleSession(app) = self {
            app.apply_github_issue_sync_result(result);
        }
    }

    fn preview_single_session_reasoning_effort_cycle(
        &mut self,
        direction: i8,
    ) -> ReasoningEffortCycleOutcome {
        match self {
            Self::SingleSession(app) => app.preview_reasoning_effort_cycle(direction),
            Self::Workspace(_) => ReasoningEffortCycleOutcome::Unavailable,
        }
    }

    fn preview_single_session_reasoning_effort_set(&mut self, effort: &str) -> Option<String> {
        match self {
            Self::SingleSession(app) => app.preview_reasoning_effort_set(effort),
            Self::Workspace(_) => None,
        }
    }

    fn set_reasoning_effort_via_active_session(&mut self, effort: String) -> anyhow::Result<()> {
        match self {
            Self::SingleSession(app) => app.set_reasoning_effort_via_active_session(effort),
            Self::Workspace(_) => {
                anyhow::bail!("reasoning effort changes require single-session mode")
            }
        }
    }

    fn set_single_session_handle(&mut self, handle: session_launch::DesktopSessionHandle) {
        if let Self::SingleSession(app) = self {
            app.set_session_handle(handle);
        }
    }

    fn apply_single_session_switcher_cards(&mut self, cards: Vec<workspace::SessionCard>) {
        if let Self::SingleSession(app) = self {
            app.apply_session_switcher_cards(cards);
        }
    }

    fn cancel_single_session_generation(&mut self) {
        if let Self::SingleSession(app) = self {
            app.cancel_generation();
        }
    }

    fn attach_clipboard_image(&mut self, media_type: String, base64_data: String) {
        match self {
            Self::SingleSession(app) => app.attach_image(media_type, base64_data),
            Self::Workspace(workspace) => {
                workspace.attach_image(media_type, base64_data);
            }
        }
    }

    fn accepts_clipboard_image_paste(&self) -> bool {
        match self {
            Self::SingleSession(app) => app.accepts_clipboard_image_paste(),
            Self::Workspace(workspace) => workspace.mode == InputMode::Insert,
        }
    }

    fn paste_text(&mut self, text: &str) {
        match self {
            Self::SingleSession(app) => app.paste_text(text),
            Self::Workspace(workspace) => {
                workspace.paste_text(text);
            }
        }
    }

    fn send_single_session_stdin_response(
        &mut self,
        request_id: String,
        input: String,
    ) -> anyhow::Result<()> {
        match self {
            Self::SingleSession(app) => app.send_stdin_response(request_id, input),
            Self::Workspace(_) => {
                anyhow::bail!("stdin responses are only supported in single-session mode")
            }
        }
    }

    fn take_next_queued_single_session_draft(&mut self) -> Option<(String, Vec<(String, String)>)> {
        match self {
            Self::SingleSession(app) => app.take_next_queued_draft(),
            Self::Workspace(_) => None,
        }
    }

    fn begin_single_session_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            if let Some(point) = single_session_body_point_at_position(size, x, y, &lines) {
                app.begin_selection(point);
                return true;
            }
        }
        false
    }

    fn update_single_session_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            if let Some(point) = single_session_body_point_at_position(size, x, y, &lines) {
                app.update_selection(point);
                return true;
            }
        }
        false
    }

    fn begin_single_session_draft_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self
            && let Some((line, column)) = single_session_draft_line_col_at_position(app, size, x, y)
        {
            app.begin_draft_selection(SelectionPoint { line, column });
            return true;
        }
        false
    }

    fn update_single_session_draft_selection_at(
        &mut self,
        x: f32,
        y: f32,
        size: PhysicalSize<u32>,
    ) -> bool {
        if let Self::SingleSession(app) = self
            && let Some((line, column)) = single_session_draft_line_col_at_position(app, size, x, y)
        {
            app.update_draft_selection(SelectionPoint { line, column });
            return true;
        }
        false
    }

    fn selected_single_session_draft_text(&mut self) -> Option<String> {
        if let Self::SingleSession(app) = self {
            return app.selected_draft_text();
        }
        None
    }

    fn selected_single_session_text(&mut self, size: PhysicalSize<u32>) -> Option<String> {
        if let Self::SingleSession(app) = self {
            let lines = single_session_visible_body(app, size);
            let selected = app.selected_text_from_lines(&lines);
            app.clear_selection();
            return selected;
        }
        None
    }

    fn scroll_single_session_body(
        &mut self,
        lines: impl Into<f64>,
        size: PhysicalSize<u32>,
        metrics_cache: &mut SingleSessionScrollMetricsCache,
    ) -> bool {
        if let Self::SingleSession(app) = self {
            let previous_scroll_lines = app.body_scroll_lines;
            app.scroll_body_lines(lines);
            if let Some(metrics) = metrics_cache.metrics(app, size) {
                app.body_scroll_lines = app.body_scroll_lines.min(metrics.max_scroll_lines as f32);
            } else {
                app.body_scroll_lines = 0.0;
            }
            return (app.body_scroll_lines - previous_scroll_lines).abs()
                >= SCROLL_FRACTIONAL_EPSILON;
        }
        false
    }

    fn single_session_smooth_scroll_lines(
        &self,
        pending_lines: f32,
        size: PhysicalSize<u32>,
        metrics_cache: &mut SingleSessionScrollMetricsCache,
    ) -> f32 {
        let Self::SingleSession(app) = self else {
            return 0.0;
        };
        let Some(metrics) = metrics_cache.metrics(app, size) else {
            return 0.0;
        };
        let base_scroll = app.body_scroll_lines.min(metrics.max_scroll_lines as f32);
        (base_scroll + pending_lines).clamp(0.0, metrics.max_scroll_lines as f32) - base_scroll
    }

    fn single_session_live_id(&self) -> Option<String> {
        match self {
            Self::SingleSession(app) => app.live_session_id.clone(),
            Self::Workspace(_) => None,
        }
    }

    #[cfg(test)]
    fn debug_snapshot(&self) -> DesktopAppDebugSnapshot {
        match self {
            Self::SingleSession(app) => DesktopAppDebugSnapshot {
                mode: "single_session",
                title: app.title(),
                live_session_id: app.live_session_id.clone(),
                status: app.status.clone(),
                is_processing: app.is_processing,
                body_text: app.body_lines().join("\n"),
            },
            Self::Workspace(workspace) => DesktopAppDebugSnapshot {
                mode: "workspace",
                title: workspace.status_title(),
                live_session_id: None,
                status: None,
                is_processing: false,
                body_text: workspace.status_title(),
            },
        }
    }
}

fn show_desktop_reload_notice(app: &mut DesktopApp) {
    app.set_single_session_status_label("desktop UI reloaded");
}

fn to_key_input(key: &Key, modifiers: ModifiersState) -> KeyInput {
    match key {
        Key::Named(NamedKey::Escape) => KeyInput::Escape,
        Key::Named(NamedKey::Space) => KeyInput::Character(" ".to_string()),
        Key::Named(NamedKey::Copy) => KeyInput::CopyLatestResponse,
        Key::Named(NamedKey::Cut) => KeyInput::CutInputLine,
        Key::Named(NamedKey::Paste) => KeyInput::PasteText,
        Key::Named(NamedKey::Undo) => KeyInput::UndoInput,
        Key::Named(NamedKey::Enter) if modifiers.control_key() => KeyInput::QueueDraft,
        Key::Named(NamedKey::Enter) if modifiers.shift_key() || modifiers.alt_key() => {
            KeyInput::Enter
        }
        Key::Named(NamedKey::Enter) => KeyInput::SubmitDraft,
        Key::Named(NamedKey::Tab) if modifiers.control_key() && modifiers.shift_key() => {
            KeyInput::CycleModel(-1)
        }
        Key::Named(NamedKey::Tab) if modifiers.control_key() => KeyInput::CycleModel(1),
        Key::Named(NamedKey::Tab) => KeyInput::Autocomplete,
        Key::Named(NamedKey::Backspace)
            if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() =>
        {
            KeyInput::DeletePreviousWord
        }
        Key::Named(NamedKey::Backspace) => KeyInput::Backspace,
        Key::Named(NamedKey::Delete) => KeyInput::DeleteNextChar,
        Key::Named(NamedKey::PageUp) => KeyInput::ScrollBodyPages(1),
        Key::Named(NamedKey::PageDown) => KeyInput::ScrollBodyPages(-1),
        Key::Named(NamedKey::ArrowUp) if modifiers.control_key() => KeyInput::RetrieveQueuedDraft,
        Key::Named(NamedKey::ArrowUp) if modifiers.alt_key() => KeyInput::JumpPrompt(-1),
        Key::Named(NamedKey::ArrowDown) if modifiers.alt_key() => KeyInput::JumpPrompt(1),
        Key::Named(NamedKey::ArrowUp) => KeyInput::ModelPickerMove(-1),
        Key::Named(NamedKey::ArrowDown) => KeyInput::ModelPickerMove(1),
        Key::Named(NamedKey::ArrowLeft) if modifiers.alt_key() => {
            KeyInput::CycleReasoningEffort(-1)
        }
        Key::Named(NamedKey::ArrowRight) if modifiers.alt_key() => {
            KeyInput::CycleReasoningEffort(1)
        }
        Key::Named(NamedKey::ArrowLeft) if modifiers.control_key() => KeyInput::MoveCursorWordLeft,
        Key::Named(NamedKey::ArrowRight) if modifiers.control_key() => {
            KeyInput::MoveCursorWordRight
        }
        Key::Named(NamedKey::ArrowLeft) => KeyInput::MoveCursorLeft,
        Key::Named(NamedKey::ArrowRight) => KeyInput::MoveCursorRight,
        Key::Named(NamedKey::Home) if modifiers.control_key() => KeyInput::ScrollBodyToTop,
        Key::Named(NamedKey::End) if modifiers.control_key() => KeyInput::ScrollBodyToBottom,
        Key::Named(NamedKey::Home) => KeyInput::MoveToLineStart,
        Key::Named(NamedKey::End) => KeyInput::MoveToLineEnd,
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("a") => {
            KeyInput::MoveToLineStart
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("e") => {
            KeyInput::MoveToLineEnd
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("b") => {
            KeyInput::MoveCursorWordLeft
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("f") => {
            KeyInput::MoveCursorWordRight
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("u") => {
            KeyInput::DeleteToLineStart
        }
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers)
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("k") =>
        {
            KeyInput::CopyLatestCodeBlock
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("k") => {
            KeyInput::DeleteToLineEnd
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("w") => {
            KeyInput::DeletePreviousWord
        }
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers) && text.eq_ignore_ascii_case("x") =>
        {
            KeyInput::CutInputLine
        }
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers) && text.eq_ignore_ascii_case("z") =>
        {
            KeyInput::UndoInput
        }
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers)
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("c") =>
        {
            KeyInput::CopyLatestResponse
        }
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers)
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("t") =>
        {
            KeyInput::CopyTranscript
        }
        Key::Character(text)
            if modifiers.control_key()
                && (text.eq_ignore_ascii_case("c") || text.eq_ignore_ascii_case("d")) =>
        {
            KeyInput::CancelGeneration
        }
        Key::Character(text) if modifiers.super_key() && text.eq_ignore_ascii_case("c") => {
            KeyInput::CopyLatestResponse
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("b") => {
            KeyInput::MoveCursorWordLeft
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("f") => {
            KeyInput::MoveCursorWordRight
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("d") => {
            KeyInput::DeleteNextWord
        }
        Key::Character(text) if modifiers.alt_key() && text.eq_ignore_ascii_case("v") => {
            KeyInput::PasteText
        }
        Key::Character(text) if modifiers.control_key() && text == "[" => KeyInput::JumpPrompt(-1),
        Key::Character(text) if modifiers.control_key() && text == "]" => KeyInput::JumpPrompt(1),
        Key::Character(text) if modifiers.super_key() && text.eq_ignore_ascii_case("k") => {
            KeyInput::JumpPrompt(-1)
        }
        Key::Character(text) if modifiers.super_key() && text.eq_ignore_ascii_case("j") => {
            KeyInput::JumpPrompt(1)
        }
        Key::Character(text)
            if (modifiers.control_key() || modifiers.super_key())
                && text.eq_ignore_ascii_case("q") =>
        {
            KeyInput::ExitApp
        }
        Key::Character(text) if modifiers.super_key() && text == ";" => {
            KeyInput::SpawnSelfDevSession
        }
        Key::Character(text) if modifiers.super_key() && text == "'" => KeyInput::SpawnHomeSession,
        Key::Character(text) if modifiers.control_key() && text == ";" => KeyInput::SpawnPanel,
        Key::Character(text) if modifiers.control_key() && (text == "?" || text == "/") => {
            KeyInput::HotkeyHelp
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("s") =>
        {
            KeyInput::ToggleSessionInfo
        }
        Key::Character(text)
            if modifiers.control_key()
                && (text.eq_ignore_ascii_case("p") || text.eq_ignore_ascii_case("o")) =>
        {
            KeyInput::OpenSessionSwitcher
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("r") => {
            KeyInput::RefreshSessions
        }
        Key::Character(text) if modifiers.control_key() && (text == "-" || text == "_") => {
            KeyInput::AdjustTextScale(-1)
        }
        Key::Character(text) if modifiers.control_key() && (text == "=" || text == "+") => {
            KeyInput::AdjustTextScale(1)
        }
        Key::Character(text) if modifiers.control_key() && text == "0" => KeyInput::ResetTextScale,
        Key::Character(text)
            if desktop_clipboard_shortcut_modifier(modifiers) && text.eq_ignore_ascii_case("v") =>
        {
            KeyInput::PasteText
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("i") =>
        {
            KeyInput::ClearAttachedImages
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("i") => {
            KeyInput::AttachClipboardImage
        }
        Key::Character(text)
            if modifiers.control_key()
                && modifiers.shift_key()
                && text.eq_ignore_ascii_case("m") =>
        {
            KeyInput::OpenModelPicker
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("m") => {
            KeyInput::CycleModel(1)
        }
        Key::Character(text) if modifiers.control_key() && text.eq_ignore_ascii_case("n") => {
            KeyInput::CycleModel(-1)
        }
        Key::Character(text) if modifiers.control_key() && text == "1" => {
            KeyInput::SetPanelSize(PanelSizePreset::Quarter)
        }
        Key::Character(text) if modifiers.control_key() && text == "2" => {
            KeyInput::SetPanelSize(PanelSizePreset::Half)
        }
        Key::Character(text) if modifiers.control_key() && text == "3" => {
            KeyInput::SetPanelSize(PanelSizePreset::ThreeQuarter)
        }
        Key::Character(text) if modifiers.control_key() && text == "4" => {
            KeyInput::SetPanelSize(PanelSizePreset::Full)
        }
        Key::Character(_)
            if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() =>
        {
            KeyInput::Other
        }
        Key::Character(text) => KeyInput::Character(text.to_string()),
        _ => KeyInput::Other,
    }
}

fn desktop_clipboard_shortcut_modifier(modifiers: ModifiersState) -> bool {
    modifiers.control_key() || modifiers.alt_key() || modifiers.super_key()
}

fn is_space_key(key: &Key) -> bool {
    matches!(key, Key::Named(NamedKey::Space)) || matches!(key, Key::Character(text) if text == " ")
}

fn workspace_space_hold_progress(
    app: &DesktopApp,
    started_at: Option<Instant>,
    consumed: bool,
) -> Option<f32> {
    let DesktopApp::Workspace(workspace) = app else {
        return None;
    };
    let started_at = started_at?;
    if consumed {
        return None;
    }
    let threshold = workspace.space_hold_toggle_duration();
    if threshold.is_zero() {
        return Some(1.0);
    }
    Some(
        (Instant::now()
            .saturating_duration_since(started_at)
            .as_secs_f32()
            / threshold.as_secs_f32())
        .clamp(0.0, 1.0),
    )
}

fn apply_desktop_session_event_batch(
    app: &mut DesktopApp,
    events: Vec<session_launch::DesktopSessionEvent>,
) -> bool {
    apply_desktop_session_event_batch_with_stats(app, events).visible_changed
}

#[derive(Debug, Clone)]
struct DesktopSessionApplyStats {
    visible_changed: bool,
    event_count: usize,
    text_delta_bytes: usize,
    session_card_refresh_requested: bool,
    elapsed: Duration,
}

fn apply_desktop_session_event_batch_with_stats(
    app: &mut DesktopApp,
    events: Vec<session_launch::DesktopSessionEvent>,
) -> DesktopSessionApplyStats {
    if events.is_empty() {
        return DesktopSessionApplyStats {
            visible_changed: false,
            event_count: 0,
            text_delta_bytes: 0,
            session_card_refresh_requested: false,
            elapsed: Duration::ZERO,
        };
    }
    let started = Instant::now();
    let event_count = events.len();
    let mut text_delta_bytes = 0usize;
    let mut visible_changed = false;
    let mut session_card_refresh_requested = false;
    for event in events {
        log_desktop_session_event_error(&event);
        if let session_launch::DesktopSessionEvent::TextDelta(text) = &event {
            text_delta_bytes += text.len();
        }
        session_card_refresh_requested |= desktop_session_event_refreshes_session_card(&event);
        visible_changed |= desktop_session_event_affects_visible_state(&event);
        app.apply_session_event(event);
    }
    let elapsed = started.elapsed();
    log_desktop_slow_interaction(
        "session_event_apply",
        elapsed,
        serde_json::json!({
            "events": event_count,
            "text_delta_bytes": text_delta_bytes,
        }),
    );
    DesktopSessionApplyStats {
        visible_changed,
        event_count,
        text_delta_bytes,
        session_card_refresh_requested,
        elapsed,
    }
}

fn log_desktop_session_event_error(event: &session_launch::DesktopSessionEvent) {
    match event {
        session_launch::DesktopSessionEvent::Error(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: session error event: {}",
                desktop_log::truncate_for_log(error, 2048)
            ));
        }
        session_launch::DesktopSessionEvent::ModelCatalogError { error } => {
            desktop_log::error(format_args!(
                "jcode-desktop: model catalog error event: {}",
                desktop_log::truncate_for_log(error, 2048)
            ));
        }
        session_launch::DesktopSessionEvent::ModelChanged {
            model,
            provider_name,
            error: Some(error),
        } => {
            desktop_log::error(format_args!(
                "jcode-desktop: model switch failed model={} provider={} error={}",
                desktop_log::truncate_for_log(model, 256),
                provider_name
                    .as_deref()
                    .map(|provider| desktop_log::truncate_for_log(provider, 256))
                    .unwrap_or_else(|| "<unknown>".to_string()),
                desktop_log::truncate_for_log(error, 2048)
            ));
        }
        session_launch::DesktopSessionEvent::ToolFinished {
            id: _,
            name,
            summary,
            is_error: true,
        } => {
            desktop_log::warn(format_args!(
                "jcode-desktop: tool failed name={} summary={}",
                desktop_log::truncate_for_log(name, 256),
                desktop_log::truncate_for_log(summary, 2048)
            ));
        }
        _ => {}
    }
}

fn desktop_session_event_refreshes_session_card(
    event: &session_launch::DesktopSessionEvent,
) -> bool {
    matches!(
        event,
        session_launch::DesktopSessionEvent::SessionStarted { .. }
            | session_launch::DesktopSessionEvent::SessionRenamed { .. }
            | session_launch::DesktopSessionEvent::Reloaded { .. }
            | session_launch::DesktopSessionEvent::Done
            | session_launch::DesktopSessionEvent::Error(_)
    )
}

#[allow(clippy::too_many_arguments)]
fn log_desktop_session_event_batch_profile(
    raw_event_count: usize,
    raw_payload_bytes: usize,
    accumulated_for: Duration,
    ui_queue_delay: Duration,
    apply_stats: &DesktopSessionApplyStats,
    redraw_requested: bool,
    redraw_deferred: bool,
    session_card_refresh_spawned: bool,
) {
    if raw_event_count < 128
        && raw_payload_bytes < 8 * 1024
        && accumulated_for < Duration::from_millis(40)
        && ui_queue_delay < DESKTOP_INPUT_LATENCY_BUDGET
        && apply_stats.elapsed < DESKTOP_120FPS_FRAME_BUDGET
        && !apply_stats.session_card_refresh_requested
        && !session_card_refresh_spawned
    {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-session-event-profile",
        serde_json::json!({
            "raw_events": raw_event_count,
            "coalesced_events": apply_stats.event_count,
            "raw_payload_bytes": raw_payload_bytes,
            "text_delta_bytes": apply_stats.text_delta_bytes,
            "forwarder_accumulated_ms": accumulated_for.as_secs_f64() * 1000.0,
            "ui_queue_delay_ms": ui_queue_delay.as_secs_f64() * 1000.0,
            "apply_ms": apply_stats.elapsed.as_secs_f64() * 1000.0,
            "visible_changed": apply_stats.visible_changed,
            "redraw_requested": redraw_requested,
            "redraw_deferred": redraw_deferred,
            "session_card_refresh_requested": apply_stats.session_card_refresh_requested,
            "session_card_refresh_spawned": session_card_refresh_spawned,
        }),
    );
}

fn log_desktop_session_card_refresh_profile(
    session_id: &str,
    loaded_in: Duration,
    card_found: bool,
    applied: bool,
) {
    if loaded_in < Duration::from_millis(40) && card_found {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-session-card-refresh-profile",
        serde_json::json!({
            "session_id": session_id,
            "loaded_in_ms": duration_ms(loaded_in),
            "card_found": card_found,
            "applied": applied,
            "ui_thread_blocking": false,
        }),
    );
}

fn log_desktop_session_cards_load_profile(
    purpose: DesktopSessionCardsPurpose,
    loaded_in: Duration,
    card_count: usize,
    applied: bool,
) {
    if loaded_in < Duration::from_millis(40) && applied {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-session-cards-load-profile",
        serde_json::json!({
            "purpose": format!("{purpose:?}"),
            "loaded_in_ms": duration_ms(loaded_in),
            "card_count": card_count,
            "applied": applied,
            "ui_thread_blocking": false,
        }),
    );
}

fn log_desktop_preferences_save_profile(
    saved_in: Duration,
    queued_for: Duration,
    coalesced_saves: usize,
    error: Option<&str>,
) {
    if saved_in < Duration::from_millis(40) && coalesced_saves <= 1 && error.is_none() {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-preferences-save-profile",
        serde_json::json!({
            "saved_in_ms": duration_ms(saved_in),
            "queued_for_ms": duration_ms(queued_for),
            "coalesced_saves": coalesced_saves,
            "error": error,
            "ui_thread_blocking": false,
        }),
    );
}

fn log_desktop_crashed_sessions_restore_profile(restored: usize, errors: usize, elapsed: Duration) {
    if elapsed < Duration::from_millis(40) && errors == 0 {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-crashed-sessions-restore-profile",
        serde_json::json!({
            "restored": restored,
            "errors": errors,
            "elapsed_ms": duration_ms(elapsed),
            "ui_thread_blocking": false,
        }),
    );
}

#[derive(Debug, Clone)]
struct DesktopStreamEndToEndBenchmark {
    raw_events: usize,
    batches: usize,
    coalesced_events: usize,
    paints: usize,
    max_batch_raw_events: usize,
    max_batch_payload_bytes: usize,
    total_wall: Duration,
    max_forwarder_accumulated: Duration,
    max_apply: Duration,
    max_no_paint_gap: Duration,
    max_batch_to_paint: Duration,
    stream_left_queued_after_first_batch: bool,
}

impl DesktopStreamEndToEndBenchmark {
    fn passes_no_paint_budget(&self) -> bool {
        self.max_no_paint_gap <= DESKTOP_NO_PAINT_BUDGET
    }

    fn passes_interaction_budget(&self) -> bool {
        self.max_apply <= DESKTOP_120FPS_FRAME_BUDGET
            && self.max_forwarder_accumulated <= DESKTOP_NO_PAINT_BUDGET
            && self.max_batch_to_paint <= DESKTOP_NO_PAINT_BUDGET
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "raw_events": self.raw_events,
            "batches": self.batches,
            "coalesced_events": self.coalesced_events,
            "paints": self.paints,
            "max_batch_raw_events": self.max_batch_raw_events,
            "max_batch_payload_bytes": self.max_batch_payload_bytes,
            "total_wall_ms": duration_ms(self.total_wall),
            "max_forwarder_accumulated_ms": duration_ms(self.max_forwarder_accumulated),
            "max_apply_ms": duration_ms(self.max_apply),
            "max_no_paint_gap_ms": duration_ms(self.max_no_paint_gap),
            "max_batch_to_paint_ms": duration_ms(self.max_batch_to_paint),
            "stream_left_queued_after_first_batch": self.stream_left_queued_after_first_batch,
            "passes_no_paint_budget": self.passes_no_paint_budget(),
            "passes_interaction_budget": self.passes_interaction_budget(),
        })
    }
}

fn run_desktop_stream_end_to_end_benchmark(raw_events: usize) -> DesktopStreamEndToEndBenchmark {
    let raw_events = raw_events.max(1);
    let (tx, rx) = mpsc::channel();
    for index in 0..raw_events {
        tx.send(session_launch::DesktopSessionEvent::TextDelta(format!(
            "{} ",
            index + 1
        )))
        .unwrap();
    }
    drop(tx);

    let started = Instant::now();
    let mut next_forward_at = started;
    let mut app = DesktopApp::SingleSession(SingleSessionApp::new(None));
    let mut batches = 0usize;
    let mut coalesced_events = 0usize;
    let mut paints = 0usize;
    let mut max_batch_raw_events = 0usize;
    let mut max_batch_payload_bytes = 0usize;
    let mut max_forwarder_accumulated = Duration::ZERO;
    let mut max_apply = Duration::ZERO;
    let mut max_no_paint_gap = Duration::ZERO;
    let mut max_batch_to_paint = Duration::ZERO;
    let mut last_paint_at = started;
    let mut pending_batch_since: Option<Instant> = None;
    let mut stream_left_queued_after_first_batch = false;

    while let Ok(first_event) = rx.try_recv() {
        let now = Instant::now();
        if now < next_forward_at {
            std::thread::sleep(next_forward_at.saturating_duration_since(now));
        }

        let batch = collect_desktop_session_event_batch(first_event, &rx);
        if batches == 0 {
            stream_left_queued_after_first_batch = batch.raw_event_count < raw_events;
        }
        let forwarded_at = Instant::now();
        next_forward_at = forwarded_at + BACKEND_EVENT_FORWARD_INTERVAL;

        batches += 1;
        coalesced_events += batch.events.len();
        max_batch_raw_events = max_batch_raw_events.max(batch.raw_event_count);
        max_batch_payload_bytes = max_batch_payload_bytes.max(batch.raw_payload_bytes);
        max_forwarder_accumulated = max_forwarder_accumulated.max(batch.accumulated_for());
        pending_batch_since.get_or_insert(batch.first_received_at);

        let apply_stats = apply_desktop_session_event_batch_with_stats(&mut app, batch.events);
        max_apply = max_apply.max(apply_stats.elapsed);
        if apply_stats.visible_changed {
            let paint_now = Instant::now();
            if paint_now.saturating_duration_since(last_paint_at) >= BACKEND_REDRAW_FRAME_INTERVAL {
                paints += 1;
                max_no_paint_gap =
                    max_no_paint_gap.max(paint_now.saturating_duration_since(last_paint_at));
                if let Some(pending_since) = pending_batch_since.take() {
                    max_batch_to_paint =
                        max_batch_to_paint.max(paint_now.saturating_duration_since(pending_since));
                }
                last_paint_at = paint_now;
            }
        }
    }

    if let Some(pending_since) = pending_batch_since.take() {
        let paint_now = Instant::now();
        paints += 1;
        max_no_paint_gap = max_no_paint_gap.max(paint_now.saturating_duration_since(last_paint_at));
        max_batch_to_paint =
            max_batch_to_paint.max(paint_now.saturating_duration_since(pending_since));
    }

    DesktopStreamEndToEndBenchmark {
        raw_events,
        batches,
        coalesced_events,
        paints,
        max_batch_raw_events,
        max_batch_payload_bytes,
        total_wall: started.elapsed(),
        max_forwarder_accumulated,
        max_apply,
        max_no_paint_gap,
        max_batch_to_paint,
        stream_left_queued_after_first_batch,
    }
}

fn desktop_session_event_affects_visible_state(
    event: &session_launch::DesktopSessionEvent,
) -> bool {
    !matches!(event, session_launch::DesktopSessionEvent::ToolInput { .. })
}

#[cfg(test)]
fn apply_pending_session_events(
    app: &mut DesktopApp,
    session_event_rx: &mpsc::Receiver<session_launch::DesktopSessionEvent>,
) -> bool {
    let mut events = Vec::new();
    while let Ok(event) = session_event_rx.try_recv() {
        events.push(event);
    }
    apply_desktop_session_event_batch(app, events)
}

impl DesktopAppDriver for DesktopApp {
    type KeyInput = KeyInput;
    type KeyOutcome = KeyOutcome;

    fn mode(&self) -> &'static str {
        DesktopApp::mode(self)
    }

    fn status_title(&self) -> String {
        DesktopApp::status_title(self)
    }

    fn live_session_id(&self) -> Option<String> {
        DesktopApp::single_session_live_id(self)
    }

    fn has_background_work(&self) -> bool {
        DesktopApp::has_background_work(self)
    }

    fn has_frame_animation(&self) -> bool {
        DesktopApp::has_frame_animation(self)
    }

    fn handle_key_input(&mut self, key: Self::KeyInput) -> Self::KeyOutcome {
        DesktopApp::handle_key(self, key)
    }

    fn apply_session_event(&mut self, event: session_launch::DesktopSessionEvent) {
        DesktopApp::apply_session_event(self, event);
    }

    fn build_scene(&self, context: DesktopSceneBuildContext) -> desktop_scene::DesktopScene {
        desktop_app_scene(self, context.scene)
    }

    fn snapshot(&self) -> DesktopUiSnapshot {
        DesktopUiSnapshot::new(
            DesktopApp::mode(self),
            DesktopApp::status_title(self),
            DesktopApp::single_session_live_id(self),
            desktop_surface_snapshot(self),
        )
    }

    fn restore_snapshot(
        &mut self,
        snapshot: DesktopUiSnapshot,
    ) -> Result<(), DesktopSnapshotRestoreError> {
        if snapshot.version != DESKTOP_UI_SNAPSHOT_VERSION {
            return Err(DesktopSnapshotRestoreError::UnsupportedVersion {
                version: snapshot.version,
            });
        }
        if snapshot.mode != DesktopApp::mode(self) {
            return Err(DesktopSnapshotRestoreError::UnsupportedMode {
                mode: snapshot.mode,
            });
        }
        Ok(())
    }
}

fn desktop_app_scene(app: &DesktopApp, mut scene: DesktopScene) -> DesktopScene {
    scene.metadata.title = Some(app.status_title());
    scene.metadata.animation_active = app.has_frame_animation();
    scene.metadata.content_ready = true;
    if scene.display_list.commands.is_empty() {
        scene.push(DesktopDisplayCommand::Clear(DesktopColor::from_array(
            BACKGROUND_TOP_LEFT,
        )));
    }
    scene
}

fn desktop_surface_snapshot(app: &DesktopApp) -> DesktopSurfaceSnapshot {
    match app {
        DesktopApp::SingleSession(single_session) => {
            DesktopSurfaceSnapshot::SingleSession(DesktopSingleSessionSnapshot {
                session_title: single_session
                    .session
                    .as_ref()
                    .map(|session| session.title.clone()),
                draft: single_session.draft.clone(),
                draft_cursor: single_session.draft_cursor,
                body_scroll_millis: (single_session.body_scroll_lines * 1000.0).round() as i32,
                detail_scroll: single_session.detail_scroll,
                show_help: single_session.show_help,
                show_session_info: single_session.show_session_info,
                pending_image_count: single_session.pending_images.len(),
                model_picker_open: single_session.model_picker.open,
                session_switcher_open: single_session.session_switcher.open,
                stdin_response_active: single_session.stdin_response.is_some(),
            })
        }
        DesktopApp::Workspace(workspace) => {
            let focused_session_id = workspace
                .surfaces
                .iter()
                .find(|surface| surface.id == workspace.focused_id)
                .and_then(|surface| surface.session_id.clone());
            DesktopSurfaceSnapshot::Workspace(DesktopWorkspaceSnapshot {
                input_mode: format!("{:?}", workspace.mode),
                focused_surface_id: workspace.focused_id,
                focused_session_id,
                zoomed: workspace.zoomed,
                detail_scroll: workspace.detail_scroll,
                draft: workspace.draft.clone(),
                draft_cursor: workspace.draft_cursor,
                pending_image_count: workspace.pending_images.len(),
                surfaces: workspace
                    .surfaces
                    .iter()
                    .map(|surface| DesktopWorkspaceSurfaceSnapshot {
                        id: surface.id,
                        kind: format!("{:?}", surface.kind),
                        title: surface.title.clone(),
                        session_id: surface.session_id.clone(),
                        lane: surface.lane,
                        column: surface.column,
                        color_index: surface.color_index,
                    })
                    .collect(),
            })
        }
    }
}

fn apply_single_session_error(app: &mut DesktopApp, error: anyhow::Error) {
    desktop_log::error(format_args!("jcode-desktop: UI action failed: {error:#}"));
    app.apply_session_event(session_launch::DesktopSessionEvent::Error(format!(
        "{error:#}"
    )));
}

#[derive(Default)]
struct DesktopClipboard {
    clipboard: Option<arboard::Clipboard>,
}

impl DesktopClipboard {
    fn clipboard(&mut self) -> Result<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new().context("failed to access clipboard")?);
        }
        self.clipboard
            .as_mut()
            .context("failed to retain clipboard handle")
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.with_clipboard_retry("failed to set clipboard text", |clipboard| {
            clipboard.set_text(text.to_string())
        })
    }

    fn get_text(&mut self) -> Result<String> {
        self.with_clipboard_retry("clipboard does not contain text", |clipboard| {
            clipboard.get_text()
        })
    }

    fn get_image(&mut self) -> Result<arboard::ImageData<'static>> {
        self.with_clipboard_retry("clipboard does not contain an image", |clipboard| {
            clipboard.get_image()
        })
    }

    fn with_clipboard_retry<T>(
        &mut self,
        context: &'static str,
        mut operation: impl FnMut(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
    ) -> Result<T> {
        const CLIPBOARD_RETRY_ATTEMPTS: usize = 3;
        const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(20);

        for attempt in 0..CLIPBOARD_RETRY_ATTEMPTS {
            let result = operation(self.clipboard()?);
            match result {
                Ok(value) => return Ok(value),
                Err(error)
                    if matches!(&error, arboard::Error::ClipboardOccupied)
                        && attempt + 1 < CLIPBOARD_RETRY_ATTEMPTS =>
                {
                    std::thread::sleep(CLIPBOARD_RETRY_DELAY);
                }
                Err(error) => {
                    if !matches!(
                        &error,
                        arboard::Error::ContentNotAvailable | arboard::Error::ClipboardOccupied
                    ) {
                        self.clipboard = None;
                    }
                    return Err(error).context(context);
                }
            }
        }

        anyhow::bail!("clipboard remained occupied after retrying")
    }
}

fn copy_text_to_clipboard(
    clipboard: &mut DesktopClipboard,
    text: &str,
    success_notice: &'static str,
    app: &mut DesktopApp,
) {
    match clipboard.set_text(text) {
        Ok(()) => app.set_single_session_status_label(success_notice),
        Err(error) => {
            desktop_log::error(format_args!(
                "jcode-desktop: failed to update clipboard after {success_notice}: {error:#}"
            ));
            app.apply_session_event(session_launch::DesktopSessionEvent::Error(format!(
                "failed to update clipboard after {success_notice}: {error:#}"
            )));
        }
    }
}

fn paste_clipboard_into_app(clipboard: &mut DesktopClipboard, app: &mut DesktopApp) -> Result<()> {
    match clipboard_text(clipboard) {
        Ok(text) => {
            if paste_clipboard_text(app, &text) || !app.accepts_clipboard_image_paste() {
                return Ok(());
            }
            paste_clipboard_image_into_app(clipboard, app)
                .with_context(|| "clipboard text was empty and no pasteable image was available")
        }
        Err(text_error) if app.accepts_clipboard_image_paste() => {
            paste_clipboard_image_into_app(clipboard, app)
                .with_context(|| format!("clipboard did not contain pasteable text: {text_error}"))
        }
        Err(error) => Err(error),
    }
}

fn paste_clipboard_text(app: &mut DesktopApp, text: &str) -> bool {
    let text = normalize_clipboard_text(text);
    if text.is_empty() {
        return false;
    }
    app.paste_text(&text);
    true
}

fn paste_clipboard_image_into_app(
    clipboard: &mut DesktopClipboard,
    app: &mut DesktopApp,
) -> Result<()> {
    let (media_type, base64_data) = clipboard_image_png_base64(clipboard)?;
    app.attach_clipboard_image(media_type, base64_data);
    Ok(())
}

fn normalize_clipboard_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn clipboard_image_png_base64(clipboard: &mut DesktopClipboard) -> Result<(String, String)> {
    let image = clipboard.get_image()?;
    let width = u32::try_from(image.width).context("clipboard image is too wide")?;
    let height = u32::try_from(image.height).context("clipboard image is too tall")?;
    let rgba = image.bytes.into_owned();
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .context("clipboard image data had unexpected dimensions")?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("failed to encode clipboard image as png")?;
    Ok((
        "image/png".to_string(),
        base64::engine::general_purpose::STANDARD.encode(cursor.into_inner()),
    ))
}

fn clipboard_text(clipboard: &mut DesktopClipboard) -> Result<String> {
    clipboard.get_text()
}

#[derive(Clone, Debug, Default)]
struct ScrollLineAccumulator {
    velocity_lines_per_second: f32,
    last_event_at: Option<Instant>,
    last_frame_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollAnimationFrame {
    scroll_lines: Option<f32>,
    active: bool,
}

impl ScrollLineAccumulator {
    fn scroll_lines(&mut self, delta: MouseScrollDelta, now: Instant) -> Option<f32> {
        if self
            .last_event_at
            .is_some_and(|last| now.saturating_duration_since(last) > SCROLL_GESTURE_IDLE_RESET)
        {
            self.stop();
        }
        self.last_event_at = Some(now);
        self.last_frame_at = Some(now);
        self.input_delta(mouse_scroll_delta_lines(delta))
    }

    fn frame(&mut self, now: Instant) -> ScrollAnimationFrame {
        let Some(last_frame_at) = self.last_frame_at else {
            self.last_frame_at = Some(now);
            return ScrollAnimationFrame {
                scroll_lines: None,
                active: self.is_active(),
            };
        };

        let dt = now
            .saturating_duration_since(last_frame_at)
            .as_secs_f32()
            .min(SCROLL_FRAME_MAX_DT_SECONDS);
        self.last_frame_at = Some(now);

        if dt <= 0.0 || !self.is_active() {
            return ScrollAnimationFrame {
                scroll_lines: None,
                active: self.is_active(),
            };
        }

        let scroll_lines = if self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
        {
            let lines = self.velocity_lines_per_second * dt;
            let decay = (-SCROLL_MOMENTUM_DECAY_PER_SECOND * dt).exp();
            self.velocity_lines_per_second *= decay;
            if self.velocity_lines_per_second.abs() < SCROLL_MOMENTUM_STOP_VELOCITY {
                self.velocity_lines_per_second = 0.0;
            }
            (lines.abs() >= SCROLL_FRACTIONAL_EPSILON).then_some(lines)
        } else {
            self.velocity_lines_per_second = 0.0;
            None
        };

        ScrollAnimationFrame {
            scroll_lines,
            active: self.is_active(),
        }
    }

    fn reset(&mut self) {
        self.stop();
        self.last_event_at = None;
        self.last_frame_at = None;
    }

    fn stop(&mut self) {
        self.velocity_lines_per_second = 0.0;
    }

    fn pending_lines(&self) -> f32 {
        0.0
    }

    fn is_active(&self) -> bool {
        self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
    }

    fn input_delta(&mut self, lines: f32) -> Option<f32> {
        if !lines.is_finite() || lines.abs() < SCROLL_FRACTIONAL_EPSILON {
            return None;
        }

        let lines = lines.clamp(
            -MAX_MOUSE_SCROLL_LINES_PER_EVENT,
            MAX_MOUSE_SCROLL_LINES_PER_EVENT,
        );
        if self.velocity_lines_per_second.abs() >= SCROLL_MOMENTUM_STOP_VELOCITY
            && self.velocity_lines_per_second.signum() != lines.signum()
        {
            self.stop();
        }

        self.velocity_lines_per_second = (self.velocity_lines_per_second
            + lines * SCROLL_MOMENTUM_GAIN)
            .clamp(-SCROLL_MOMENTUM_MAX_VELOCITY, SCROLL_MOMENTUM_MAX_VELOCITY);
        Some(lines)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SingleSessionScrollMotionFrame {
    visual_scroll_lines: f32,
    smooth_scroll_lines: f32,
    active: bool,
}

#[derive(Clone, Debug, Default)]
struct SingleSessionScrollMotion {
    initialized: bool,
    start_lines: f32,
    current_lines: f32,
    target_lines: f32,
    started_at: Option<Instant>,
}

impl SingleSessionScrollMotion {
    fn frame(&mut self, target_lines: f32, now: Instant) -> SingleSessionScrollMotionFrame {
        let target_lines = if target_lines.is_finite() {
            target_lines.max(0.0)
        } else {
            0.0
        };

        if !self.initialized || animation::desktop_reduced_motion_enabled() {
            self.initialized = true;
            self.start_lines = target_lines;
            self.current_lines = target_lines;
            self.target_lines = target_lines;
            self.started_at = None;
            return SingleSessionScrollMotionFrame {
                visual_scroll_lines: target_lines,
                smooth_scroll_lines: 0.0,
                active: false,
            };
        }

        if (self.target_lines - target_lines).abs() >= SCROLL_FRACTIONAL_EPSILON {
            self.start_lines = self.current_lines;
            self.target_lines = target_lines;
            self.started_at = Some(now);
        }

        if let Some(started_at) = self.started_at {
            let progress = (now.saturating_duration_since(started_at).as_secs_f32()
                / SINGLE_SESSION_SCROLL_ANIMATION_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            let eased = animation::ease_out_cubic(progress);
            self.current_lines = animation::lerp(self.start_lines, self.target_lines, eased);
            if progress >= 1.0
                || (self.current_lines - self.target_lines).abs() < SCROLL_FRACTIONAL_EPSILON
            {
                self.current_lines = self.target_lines;
                self.started_at = None;
            }
        }

        SingleSessionScrollMotionFrame {
            visual_scroll_lines: self.current_lines,
            smooth_scroll_lines: self.current_lines - target_lines,
            active: self.is_active(),
        }
    }

    fn is_active(&self) -> bool {
        self.started_at.is_some()
            || (self.current_lines - self.target_lines).abs() >= SCROLL_FRACTIONAL_EPSILON
    }

    fn clear(&mut self) {
        self.initialized = false;
        self.start_lines = 0.0;
        self.current_lines = 0.0;
        self.target_lines = 0.0;
        self.started_at = None;
    }
}

#[cfg(test)]
fn mouse_scroll_lines(delta: MouseScrollDelta) -> Option<f32> {
    ScrollLineAccumulator::default().scroll_lines(delta, Instant::now())
}

fn mouse_scroll_delta_lines(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y * MOUSE_WHEEL_LINES_PER_DETENT,
        MouseScrollDelta::PixelDelta(position) => position.y as f32 / body_scroll_line_pixels(),
    }
}

fn body_scroll_line_pixels() -> f32 {
    let typography = single_session_typography();
    typography.body_size * typography.body_line_height
}

fn desktop_spinner_tick(_now: Instant) -> u64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    (millis / DESKTOP_SPINNER_FRAME_MS) as u64
}

/// Continuous wall-clock seconds for smooth (unquantized) pulse animations.
/// Unlike `desktop_spinner_tick`, this is not stepped to 180ms frames, so
/// breathing cues animate fluidly at the paced 16ms redraw interval.
fn desktop_pulse_seconds() -> f32 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    // Wrap at a day to keep f32 precision; pulse phases only use fract().
    ((millis % 86_400_000) as f64 / 1000.0) as f32
}

fn single_session_text_buffer_cache_key(
    app: &SingleSessionApp,
    size: PhysicalSize<u32>,
    _tick: u64,
    rendered_body_key: u64,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    rendered_body_key.hash(&mut hasher);
    (size.width, size.height).hash(&mut hasher);
    app.is_welcome_timeline_visible().hash(&mut hasher);
    app.has_activity_indicator().hash(&mut hasher);
    app.text_scale().to_bits().hash(&mut hasher);
    app.header_title().hash(&mut hasher);
    app.welcome_hero_text().hash(&mut hasher);
    // Use the render-time styled lines (which honor the reveal animation) so the
    // text buffer cache key matches the content actually placed into the buffer.
    // Hashing the pre-reveal lines here while the buffer is built from the
    // reveal-aware lines causes a stale buffer: the picker chrome re-renders every
    // frame but the glyph text is never re-prepared as the reveal progresses.
    app.render_inline_widget_styled_lines().hash(&mut hasher);
    // The inline-widget card grows via a reveal animation, and the glyph text is
    // vertically clipped to the animating card bounds. Quantize the reveal
    // progress into the cache key so the text buffer is re-prepared across the
    // whole animation; otherwise the text is prepared once early (while the card
    // is still small and clips everything but the first line) and never refreshed,
    // leaving stale/partial text under fully-rendered chrome.
    if app.render_inline_widget_kind().is_some() {
        let reveal_bucket = (app.render_inline_widget_reveal_progress() * 32.0).round() as u32;
        reveal_bucket.hash(&mut hasher);
    }
    app.composer_text().hash(&mut hasher);
    hasher.finish()
}

fn single_session_body_text_window_bounds(viewport: &SingleSessionBodyViewport) -> (usize, usize) {
    let start = viewport
        .start_line
        .saturating_sub(SINGLE_SESSION_BODY_TEXT_WINDOW_BEFORE_LINES);
    let end = viewport
        .start_line
        .saturating_add(viewport.lines.len())
        .saturating_add(SINGLE_SESSION_BODY_TEXT_WINDOW_AFTER_LINES)
        .min(viewport.total_lines);
    (start, end.max(start))
}

fn single_session_body_text_window_contains(
    window_start: usize,
    window_end: usize,
    viewport: &SingleSessionBodyViewport,
) -> bool {
    let visible_end = viewport.start_line.saturating_add(viewport.lines.len());
    window_start <= viewport.start_line && visible_end <= window_end
}

#[derive(Default)]
struct SingleSessionScrollMetricsCache {
    key: Option<u64>,
    total_lines: usize,
    raw_body_key: Option<u64>,
    raw_body_lines: Vec<SingleSessionStyledLine>,
    streaming_base_key: Option<u64>,
    streaming_base_total_lines: usize,
}

impl SingleSessionScrollMetricsCache {
    fn metrics(
        &mut self,
        app: &SingleSessionApp,
        size: PhysicalSize<u32>,
    ) -> Option<SingleSessionBodyScrollMetrics> {
        let body_layout_size = single_session_body_layout_cache_size(app, size);
        let key = app.rendered_body_cache_key(body_layout_size);
        if self.key != Some(key) {
            if !app.streaming_response.is_empty() {
                let base_key = app.rendered_body_static_cache_key(body_layout_size);
                if self.streaming_base_key != Some(base_key) {
                    if let Some(base_lines) =
                        single_session_rendered_static_body_lines_for_streaming(app, size, 0)
                    {
                        self.streaming_base_total_lines = base_lines.len();
                        self.streaming_base_key = Some(base_key);
                    } else {
                        self.streaming_base_key = None;
                        self.streaming_base_total_lines = 0;
                    }
                }
                if self.streaming_base_key == Some(base_key) {
                    self.total_lines = self.streaming_base_total_lines
                        + single_session_streaming_response_rendered_body_line_count(app, size);
                } else {
                    self.total_lines =
                        single_session_rendered_body_lines_for_tick(app, size, 0).len();
                }
            } else {
                let raw_key = app.rendered_body_cache_key((0, 0));
                if self.raw_body_key != Some(raw_key) {
                    self.raw_body_lines = app.body_styled_lines_for_tick(0);
                    self.raw_body_key = Some(raw_key);
                }
                self.total_lines = single_session_rendered_body_lines_from_raw_ref(
                    app,
                    size,
                    &self.raw_body_lines,
                )
                .len();
                self.streaming_base_key = None;
                self.streaming_base_total_lines = 0;
            }
            self.key = Some(key);
        }
        single_session_body_scroll_metrics_for_total_lines(app, size, self.total_lines)
    }

    fn clear(&mut self) {
        self.key = None;
        self.total_lines = 0;
        self.raw_body_key = None;
        self.raw_body_lines.clear();
        self.streaming_base_key = None;
        self.streaming_base_total_lines = 0;
    }
}

#[derive(Clone)]
struct DesktopFrameStageProfile {
    name: &'static str,
    duration: Duration,
}

struct DesktopFrameProfile {
    started_at: Instant,
    last_checkpoint: Instant,
    stages: Vec<DesktopFrameStageProfile>,
}

impl DesktopFrameProfile {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_checkpoint: now,
            stages: Vec::with_capacity(20),
        }
    }

    fn checkpoint(&mut self, name: &'static str) {
        let now = Instant::now();
        self.stages.push(DesktopFrameStageProfile {
            name,
            duration: now.saturating_duration_since(self.last_checkpoint),
        });
        self.last_checkpoint = now;
    }

    fn total_duration(&self) -> Duration {
        self.last_checkpoint
            .saturating_duration_since(self.started_at)
    }

    fn stage_duration(&self, name: &'static str) -> Duration {
        self.stages
            .iter()
            .filter(|stage| stage.name == name)
            .fold(Duration::ZERO, |total, stage| total + stage.duration)
    }

    fn cpu_duration(&self) -> Duration {
        self.stages
            .iter()
            .filter(|stage| !matches!(stage.name, "surface_acquire" | "queue_submit" | "present"))
            .fold(Duration::ZERO, |total, stage| total + stage.duration)
    }
}

#[derive(Clone, Copy)]
struct DesktopFrameContext {
    mode: &'static str,
    smooth_scroll_lines: f32,
    text_buffer_count: usize,
    text_area_count: usize,
    primitive_vertices: usize,
    body_line_count: usize,
    viewport_line_count: usize,
    body_text_window_line_count: usize,
    streaming_text_line_count: usize,
    inline_widget_line_count: usize,
    text_prepared: bool,
    primitive_geometry_cache_hit: bool,
}

#[derive(Clone)]
struct DesktopRenderFrameResult {
    animation_active: bool,
    content_ready: bool,
    frame_wall: Duration,
    frame_cpu: Duration,
    context: DesktopFrameContext,
    stages: Vec<DesktopFrameStageProfile>,
}

#[derive(Clone)]
struct DesktopFrameSlowSample {
    wall: Duration,
    cpu: Duration,
    surface_acquire: Duration,
    queue_submit: Duration,
    present: Duration,
    score: Duration,
    stages: Vec<DesktopFrameStageProfile>,
    context: DesktopFrameContext,
}

struct DesktopFrameProfiler {
    enabled: bool,
    log_all: bool,
    budget: Duration,
    report_interval: Duration,
    frames: usize,
    slow_cpu_frames: usize,
    present_stall_frames: usize,
    worst: Option<DesktopFrameSlowSample>,
    last_report: Option<Instant>,
}

impl DesktopFrameProfiler {
    fn new() -> Self {
        let mode = desktop_frame_profile_mode();
        let enabled = desktop_frame_profile_enabled(mode.as_deref());
        let log_all = desktop_frame_profile_log_all(mode.as_deref());
        let budget =
            duration_millis_env("JCODE_DESKTOP_FRAME_BUDGET_MS", DESKTOP_120FPS_FRAME_BUDGET);
        Self {
            enabled,
            log_all,
            budget,
            report_interval: DESKTOP_FRAME_PROFILE_REPORT_INTERVAL,
            frames: 0,
            slow_cpu_frames: 0,
            present_stall_frames: 0,
            worst: None,
            last_report: None,
        }
    }

    fn observe(&mut self, profile: DesktopFrameProfile, context: DesktopFrameContext) {
        if !self.enabled {
            return;
        }

        self.frames += 1;
        let wall = profile.total_duration();
        let cpu = profile.cpu_duration();
        let surface_acquire = profile.stage_duration("surface_acquire");
        let queue_submit = profile.stage_duration("queue_submit");
        let present = profile.stage_duration("present");
        let cpu_slow = cpu >= self.budget;
        let present_stall = surface_acquire >= DESKTOP_PRESENT_STALL_BUDGET
            || queue_submit >= DESKTOP_PRESENT_STALL_BUDGET
            || present >= DESKTOP_PRESENT_STALL_BUDGET;
        if cpu_slow || present_stall || self.log_all {
            if cpu_slow {
                self.slow_cpu_frames += 1;
            }
            if present_stall {
                self.present_stall_frames += 1;
            }
            let score = cpu.max(surface_acquire).max(queue_submit).max(present);
            let replace_worst = self
                .worst
                .as_ref()
                .is_none_or(|sample| score > sample.score);
            if replace_worst {
                self.worst = Some(DesktopFrameSlowSample {
                    wall,
                    cpu,
                    surface_acquire,
                    queue_submit,
                    present,
                    score,
                    stages: profile.stages,
                    context,
                });
            }
        }

        let now = Instant::now();
        let report_due = self.last_report.is_none_or(|last_report| {
            now.saturating_duration_since(last_report) >= self.report_interval
        });
        if report_due && (self.slow_cpu_frames > 0 || self.present_stall_frames > 0 || self.log_all)
        {
            self.report(now);
        }
    }

    fn report(&mut self, now: Instant) {
        if let Some(worst) = self.worst.as_ref() {
            emit_desktop_profile_event(
                "jcode-desktop-frame-profile",
                serde_json::json!({
                    "cpu_budget_ms": duration_ms(self.budget),
                    "present_stall_budget_ms": duration_ms(DESKTOP_PRESENT_STALL_BUDGET),
                    "window_frames": self.frames,
                    "slow_frames": self.slow_cpu_frames,
                    "slow_cpu_frames": self.slow_cpu_frames,
                    "present_stall_frames": self.present_stall_frames,
                    "worst_frame_ms": duration_ms(worst.wall),
                    "worst_wall_ms": duration_ms(worst.wall),
                    "worst_cpu_ms": duration_ms(worst.cpu),
                    "surface_acquire_ms": duration_ms(worst.surface_acquire),
                    "queue_submit_ms": duration_ms(worst.queue_submit),
                    "present_ms": duration_ms(worst.present),
                    "submit_present_ms": duration_ms(worst.queue_submit + worst.present),
                    "mode": worst.context.mode,
                    "smooth_scroll_lines": worst.context.smooth_scroll_lines,
                    "text_buffer_count": worst.context.text_buffer_count,
                    "text_area_count": worst.context.text_area_count,
                    "primitive_vertices": worst.context.primitive_vertices,
                    "body_line_count": worst.context.body_line_count,
                    "viewport_line_count": worst.context.viewport_line_count,
                    "body_text_window_line_count": worst.context.body_text_window_line_count,
                    "streaming_text_line_count": worst.context.streaming_text_line_count,
                    "inline_widget_line_count": worst.context.inline_widget_line_count,
                    "text_prepared": worst.context.text_prepared,
                    "primitive_geometry_cache_hit": worst.context.primitive_geometry_cache_hit,
                    "stages": worst.stages.iter().map(|stage| serde_json::json!({
                        "name": stage.name,
                        "ms": duration_ms(stage.duration),
                    })).collect::<Vec<_>>(),
                }),
            );
        }
        self.frames = 0;
        self.slow_cpu_frames = 0;
        self.present_stall_frames = 0;
        self.worst = None;
        self.last_report = Some(now);
    }
}

#[derive(Clone, Copy)]
struct DesktopPendingInteraction {
    kind: &'static str,
    started_at: Instant,
    count: usize,
}

struct DesktopInteractionLatencyProfiler {
    enabled: bool,
    log_all: bool,
    budget: Duration,
    pending: Option<DesktopPendingInteraction>,
}

impl DesktopInteractionLatencyProfiler {
    fn new() -> Self {
        let mode = desktop_frame_profile_mode();
        let enabled = desktop_frame_profile_enabled(mode.as_deref());
        let log_all = desktop_frame_profile_log_all(mode.as_deref());
        let budget = duration_millis_env(
            "JCODE_DESKTOP_INPUT_LATENCY_BUDGET_MS",
            DESKTOP_INPUT_LATENCY_BUDGET,
        );
        Self {
            enabled,
            log_all,
            budget,
            pending: None,
        }
    }

    fn mark(&mut self, kind: &'static str, started_at: Instant) {
        if !self.enabled {
            return;
        }
        match self.pending.as_mut() {
            Some(pending) => {
                if started_at < pending.started_at {
                    pending.started_at = started_at;
                }
                if pending.kind != kind {
                    pending.kind = "mixed";
                }
                pending.count += 1;
            }
            None => {
                self.pending = Some(DesktopPendingInteraction {
                    kind,
                    started_at,
                    count: 1,
                });
            }
        }
    }

    fn pending_kind(&self) -> Option<&'static str> {
        self.pending.as_ref().map(|pending| pending.kind)
    }

    fn observe_presented(&mut self, frame: &DesktopRenderFrameResult) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if !self.enabled {
            return;
        }
        let latency = Instant::now().saturating_duration_since(pending.started_at);
        if latency < self.budget && !self.log_all {
            return;
        }
        eprintln!(
            "jcode-desktop-latency-profile {}",
            serde_json::json!({
                "kind": pending.kind,
                "interaction_count": pending.count,
                "latency_budget_ms": duration_ms(self.budget),
                "latency_ms": duration_ms(latency),
                "frame_wall_ms": duration_ms(frame.frame_wall),
                "frame_cpu_ms": duration_ms(frame.frame_cpu),
                "mode": frame.context.mode,
                "smooth_scroll_lines": frame.context.smooth_scroll_lines,
                "text_buffer_count": frame.context.text_buffer_count,
                "text_area_count": frame.context.text_area_count,
                "primitive_vertices": frame.context.primitive_vertices,
                "body_line_count": frame.context.body_line_count,
                "viewport_line_count": frame.context.viewport_line_count,
                "body_text_window_line_count": frame.context.body_text_window_line_count,
                "streaming_text_line_count": frame.context.streaming_text_line_count,
                "inline_widget_line_count": frame.context.inline_widget_line_count,
                "text_prepared": frame.context.text_prepared,
                "stages": frame.stages.iter().map(|stage| serde_json::json!({
                    "name": stage.name,
                    "ms": duration_ms(stage.duration),
                })).collect::<Vec<_>>(),
            })
        );
    }
}

#[derive(Clone, Copy)]
struct NoPaintWatchdogContext {
    active: bool,
    mode: &'static str,
    has_background_work: bool,
    frame_animation_active: bool,
    pending_backend_redraw: bool,
    pending_interaction_kind: Option<&'static str>,
}

struct DesktopNoPaintWatchdog {
    enabled: bool,
    log_all: bool,
    budget: Duration,
    last_presented_at: Instant,
    last_reported_at: Option<Instant>,
    last_redraw_request_at: Option<Instant>,
}

impl DesktopNoPaintWatchdog {
    fn new() -> Self {
        let now = Instant::now();
        Self::new_with_start(now)
    }

    fn new_with_start(now: Instant) -> Self {
        let mode = desktop_frame_profile_mode();
        Self::new_with_start_and_mode(now, mode.as_deref())
    }

    fn new_with_start_and_mode(now: Instant, mode: Option<&str>) -> Self {
        let enabled = desktop_frame_profile_enabled(mode);
        let log_all = desktop_frame_profile_log_all(mode);
        let budget =
            duration_millis_env("JCODE_DESKTOP_NO_PAINT_BUDGET_MS", DESKTOP_NO_PAINT_BUDGET);
        Self {
            enabled,
            log_all,
            budget,
            last_presented_at: now,
            last_reported_at: None,
            last_redraw_request_at: None,
        }
    }

    fn observe_presented(&mut self, now: Instant, _frame: &DesktopRenderFrameResult) {
        self.last_presented_at = now;
        self.last_reported_at = None;
        self.last_redraw_request_at = None;
    }

    fn observe_active_tick(&mut self, now: Instant, context: NoPaintWatchdogContext) -> bool {
        if !self.enabled {
            return false;
        }
        if !context.active {
            self.last_reported_at = None;
            self.last_redraw_request_at = None;
            return false;
        }
        let gap = now.saturating_duration_since(self.last_presented_at);
        if gap < self.budget && !self.log_all {
            return false;
        }

        let report_due = self.last_reported_at.is_none_or(|last_reported| {
            now.saturating_duration_since(last_reported) >= DESKTOP_FRAME_PROFILE_REPORT_INTERVAL
        });
        if report_due {
            self.last_reported_at = Some(now);
            emit_desktop_profile_event(
                "jcode-desktop-no-paint-profile",
                serde_json::json!({
                    "budget_ms": duration_ms(self.budget),
                    "gap_ms": duration_ms(gap),
                    "mode": context.mode,
                    "has_background_work": context.has_background_work,
                    "frame_animation_active": context.frame_animation_active,
                    "pending_backend_redraw": context.pending_backend_redraw,
                    "pending_interaction_kind": context.pending_interaction_kind,
                }),
            );
        }

        let redraw_due = self.last_redraw_request_at.is_none_or(|last_request| {
            now.saturating_duration_since(last_request) >= BACKEND_REDRAW_FRAME_INTERVAL
        });
        if redraw_due {
            self.last_redraw_request_at = Some(now);
        }
        redraw_due
    }
}

#[cfg(test)]
mod desktop_no_paint_watchdog_tests {
    use super::*;

    #[test]
    fn no_paint_watchdog_requests_redraw_after_active_gap_budget() {
        let start = Instant::now();
        let mut watchdog = DesktopNoPaintWatchdog::new_with_start_and_mode(start, Some("1"));
        let context = NoPaintWatchdogContext {
            active: true,
            mode: "single_session",
            has_background_work: true,
            frame_animation_active: false,
            pending_backend_redraw: false,
            pending_interaction_kind: Some("backend_events"),
        };

        assert!(!watchdog.observe_active_tick(start + watchdog.budget / 2, context));
        assert!(watchdog.observe_active_tick(start + watchdog.budget, context));
        assert!(!watchdog.observe_active_tick(
            start + watchdog.budget + BACKEND_REDRAW_FRAME_INTERVAL / 2,
            context
        ));
        assert!(watchdog.observe_active_tick(
            start + watchdog.budget + BACKEND_REDRAW_FRAME_INTERVAL,
            context
        ));
    }

    #[test]
    fn no_paint_watchdog_resets_when_idle_or_presented() {
        let start = Instant::now();
        let mut watchdog = DesktopNoPaintWatchdog::new_with_start_and_mode(start, Some("1"));
        let active_context = NoPaintWatchdogContext {
            active: true,
            mode: "single_session",
            has_background_work: true,
            frame_animation_active: false,
            pending_backend_redraw: false,
            pending_interaction_kind: None,
        };
        let idle_context = NoPaintWatchdogContext {
            active: false,
            ..active_context
        };

        assert!(watchdog.observe_active_tick(start + watchdog.budget, active_context));
        assert!(
            !watchdog.observe_active_tick(start + watchdog.budget + watchdog.budget, idle_context)
        );
        assert!(watchdog.last_redraw_request_at.is_none());
    }

    #[test]
    fn no_paint_watchdog_is_off_without_explicit_frame_profile_mode() {
        let start = Instant::now();
        let mut watchdog = DesktopNoPaintWatchdog::new_with_start_and_mode(start, None);
        let context = NoPaintWatchdogContext {
            active: true,
            mode: "single_session",
            has_background_work: true,
            frame_animation_active: false,
            pending_backend_redraw: false,
            pending_interaction_kind: Some("backend_events"),
        };

        assert!(!watchdog.enabled);
        assert!(!watchdog.observe_active_tick(start + DESKTOP_NO_PAINT_BUDGET * 4, context));
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

static DESKTOP_PROFILE_LOG_TX: OnceLock<Option<mpsc::Sender<DesktopProfileLogLine>>> =
    OnceLock::new();
static DESKTOP_PROFILE_LAUNCH_ID: OnceLock<String> = OnceLock::new();

#[derive(Debug)]
struct DesktopProfileLogLine {
    stderr_line: String,
    jsonl_line: String,
}

fn desktop_profile_log_path() -> Option<PathBuf> {
    if std::env::var_os("JCODE_DESKTOP_PROFILE_LOG").is_some_and(|value| !env_flag_enabled(value)) {
        return None;
    }
    if let Some(path) = std::env::var_os("JCODE_DESKTOP_PROFILE_LOG_PATH") {
        if path.is_empty() {
            return None;
        }
        return Some(PathBuf::from(path));
    }
    let cache_root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(
        cache_root
            .join("jcode")
            .join("desktop")
            .join("performance.log"),
    )
}

fn desktop_profile_stderr_enabled() -> bool {
    std::env::var_os("JCODE_DESKTOP_PROFILE_STDERR").is_none_or(env_flag_enabled)
}

fn desktop_profile_launch_id() -> &'static str {
    DESKTOP_PROFILE_LAUNCH_ID
        .get_or_init(|| {
            let timestamp_unix_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
                .unwrap_or_default();
            format!("{timestamp_unix_ms}-{}", std::process::id())
        })
        .as_str()
}

fn desktop_profile_log_sender() -> Option<&'static mpsc::Sender<DesktopProfileLogLine>> {
    DESKTOP_PROFILE_LOG_TX
        .get_or_init(|| {
            let path = desktop_profile_log_path();
            let stderr_enabled = desktop_profile_stderr_enabled();
            if path.is_none() && !stderr_enabled {
                return None;
            }
            let (tx, rx) = mpsc::channel::<DesktopProfileLogLine>();
            match std::thread::Builder::new()
                .name("jcode-desktop-profile-log".to_string())
                .spawn(move || {
                    let mut file = path.and_then(|path| {
                        if let Some(parent) = path.parent()
                            && let Err(error) = std::fs::create_dir_all(parent)
                        {
                            desktop_log::error(format_args!(
                                "jcode-desktop: failed to create profile log directory {}: {error}",
                                parent.display()
                            ));
                            return None;
                        }
                        match OpenOptions::new().create(true).append(true).open(&path) {
                            Ok(file) => Some(file),
                            Err(error) => {
                                desktop_log::error(format_args!(
                                    "jcode-desktop: failed to open profile log {}: {error}",
                                    path.display()
                                ));
                                None
                            }
                        }
                    });
                    while let Ok(line) = rx.recv() {
                        if stderr_enabled {
                            eprintln!("{}", line.stderr_line);
                        }
                        if let Some(profile_file) = file.as_mut()
                            && let Err(error) = writeln!(profile_file, "{}", line.jsonl_line)
                        {
                            desktop_log::error(format_args!(
                                "jcode-desktop: failed to write profile log: {error}"
                            ));
                            file = None;
                        }
                    }
                }) {
                Ok(_) => Some(tx),
                Err(error) => {
                    desktop_log::error(format_args!(
                        "jcode-desktop: failed to start profile logger: {error:#}"
                    ));
                    None
                }
            }
        })
        .as_ref()
}

fn emit_desktop_profile_event(event: &'static str, payload: serde_json::Value) {
    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or_default();
    if let Some(tx) = desktop_profile_log_sender() {
        let stderr_line = format!("{event} {payload}");
        let jsonl_line = serde_json::json!({
            "timestamp_unix_ms": timestamp_unix_ms,
            "launch_id": desktop_profile_launch_id(),
            "build_hash": desktop_build_hash_label(),
            "pid": std::process::id(),
            "event": event,
            "payload": payload,
        })
        .to_string();
        if tx
            .send(DesktopProfileLogLine {
                stderr_line,
                jsonl_line,
            })
            .is_err()
        {
            desktop_log::warn(format_args!(
                "jcode-desktop: failed to queue profile event {event}, logger is closed"
            ));
        }
    }
}

fn log_desktop_slow_interaction(
    kind: &'static str,
    duration: Duration,
    details: serde_json::Value,
) {
    if duration < DESKTOP_120FPS_FRAME_BUDGET {
        return;
    }
    let mode = desktop_frame_profile_mode();
    if !desktop_frame_profile_enabled(mode.as_deref()) {
        return;
    }
    emit_desktop_profile_event(
        "jcode-desktop-interaction-profile",
        serde_json::json!({
            "kind": kind,
            "budget_ms": duration_ms(DESKTOP_120FPS_FRAME_BUDGET),
            "duration_ms": duration_ms(duration),
            "details": details,
        }),
    );
}




fn desktop_build_hash_label() -> &'static str {
    option_env!("JCODE_DESKTOP_GIT_HASH").unwrap_or("unknown")
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
