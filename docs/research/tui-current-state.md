# jcode TUI Architecture: Current State

Survey as of 2026-07-24 (v0.55.19-dev). Crates: `jcode-tui`, `jcode-tui-style`, `jcode-tui-render`,
`jcode-tui-core`, `jcode-tui-anim`, `jcode-tui-markdown`, `jcode-tui-mermaid`, `jcode-tui-messages`,
`jcode-tui-workspace`, plus pickers/overlays.

## 1. Rendering Stack

- **ratatui 0.30** + **crossterm 0.29** (event-stream). `Cargo.toml:40-41`
- **Backend:** `DefaultTerminal` (CrosstermBackend<Stdout>), created in the binary, passed to `App::run()`.
- **Event loop:** `tokio::select!` with 4 arms: status-spinner patch (80ms), adaptive redraw tick,
  crossterm EventStream (drained in batches of 32), bus events. `app/run_shell.rs:399-520`
- **Redraw:** Full buffer clear (`Color::Reset`) every frame; no diff-based updates.
  `ui.rs:2504-2550`. Single `draw()` entry wrapped in `catch_unwind` + light-theme buffer adaptation.
- **Tick intervals (adaptive):** idle=1000ms, deep-idle=5000ms, streaming/processing=50ms,
  animation=~16.7ms, passive-liveness=1000ms. Adaptive governor caps based on recent avg draw cost
  (12-frame window, 40% duty cycle, max 250ms). `mod.rs:1550-1740`
- **Status spinner fast path:** Single-cell braille spinner patched at 80ms between full redraws
  (12.5fps). `app/run_shell.rs:8-9, ui.rs:3479-3540`
- **Render primitives** (`jcode-tui-render`): `chrome.rs` (clear, insets, right-rail), `layout.rs`
  (rect ops, area parsing), plus memory/swarm tile renderers.

## 2. Theme / Colour System

- **Crate:** `jcode-tui-style`. Colours defined as `rgb(r,g,b)` with auto truecolour→256 detection.
  `theme.rs:1-230`, `color.rs:1-130`
- **Capability detection order:** `JCODE_GLYPH_SAFE_MODE` → macOS+VS Code/AppleTerminal forced to 256
  (glyph atlas workaround, issue #330) → `COLORTERM` → `TERM_PROGRAM` → terminal-specific env vars
  → `TERM` parsing → default 256. `color.rs:40-130`
- **Palette (dark native):** Single set of named fn's: `user_color(138,180,248)`,
  `ai_color(129,199,132)`, `tool_color(120,120,120)`, `accent_color(186,139,255)`,
  `queued_color(255,193,7)`, `user_bg(35,40,50)`, `user_text(245,245,255)`, `ai_text(220,220,215)`,
  etc. `theme.rs:1-50`
- **Dynamic colours:** `animated_tool_color()` (cyan↔purple pulse), `prompt_entry_shimmer_color()`,
  `rainbow_prompt_color()` (multi-hue queue fade). `theme.rs:110-230`
- **Light mode:** Detected via `terminal-colorsaurus` OSC 11 query (400ms timeout) before raw mode,
  overridable via `JCODE_THEME`/`display.theme` config. Adapts rendered buffer per-cell by flipping
  luminance (HSL, hue-preserving) — no per-widget code changes needed.
  `theme_mode.rs:1-185`, `theme_detect.rs:1-176`
- **Synchronized output:** Not used.

## 3. Layout

- **Top-level zones** (vertical Layout, packed vs scroll):
  Messages (scrollable/Min(3)) → swarm strip → queued → status(1row) → notification → inline UI →
  inline gap → input → overscroll → donut. `ui.rs:2700-2950`
- **Diagram pane split:** Right column (`DiagramPanePosition::Right`) or top row
  (`DiagramPanePosition::Top`). Ratio via `diagram_pane_ratio()` (configurable %). `ui.rs:2620-2770`
- **Side panel/diff/pinned** right split: 25-100% ratio, adaptive wider for images.
  `ui.rs:2780-2900`
- **Borders:** 1-cell native scrollbar column (Unicode: ╷╵•). `ui.rs:3479-3540`. Right-rail
  left-border between chat and side panel. `jcode-tui-render/chrome.rs:35-70`
- **Centred mode:** Content inset with left-padding on message lines. `chrome.rs:8-30`
- **Info widgets** overlaid on chat area (memory, swarm, todos, usage, tips, timeline, git graph).
  `info_widget.rs` (75KB). Avoidance via margin reservation. `ui.rs:3310-3420`
- **Resize:** Debounced with `last_resize_redraw` timer + `resize_redraw_pending` flag.
  `app.rs:925-929`
- **Key view files:** Session picker (`session_picker.rs`, 96KB), changelog/help overlays,
  file diff view (`ui_file_diff.rs`), onboarding (`ui_onboarding.rs`).

## 4. Polish Features

- **Animations:** Braille spinner (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) at 12.5fps. 3D idle animations (donut,
  orbit_rings, gyroscope, black_hole) via `jcode-tui-anim` crate (930 lines, pinned `opt-level=3`),
  subpixel sampling at 3×3, per-cell hue cycling. Performance tiers disable decorative animations
  (Minimal tier for SSH/WSL). `ui_animations.rs:1-502`, `mod.rs:1550-1590`
- **Markdown:** `jcode-tui-markdown` crate. Supports headers, code blocks, lists, blockquotes,
  tables, inline code, links. Code blocks syntax-highlighted via **syntect 5** (`default-syntaxes`,
  `default-themes`, `regex-onig`). `markdown.rs:10-75`, `jcode-tui-markdown/Cargo.toml:18`
  Incremental streaming renderer. Diagram display modes: None/Margin/Pinned.
  Spacing: Compact/Document. LaTeX: None/Unicode/Image.
- **Mermaid:** `jcode-tui-mermaid` crate renders diagrams via external CLI, displayed inline or as
  Kitty-protocol pixel images in pinned pane. `mermaid.rs`, `ui_diagram_pane.rs`
- **Images:** Kitty terminal graphics protocol (via `jcode-terminal-image`). Inline image rendering
  (`ui_inline_image.rs`, 67KB). Anchor-based memoization for scroll stability.
- **Mouse:** `MouseEvent`/`MouseEventKind` handled. Scroll with trackpad velocity detection and
  animation smoothing. Copy selection via mouse drag with edge autoscroll.
  `app.rs:24-25,1484`, `mod.rs:338-339`
- **Scrolling:** Custom native scrollbar. Virtual scrolling with wrapped-line computation.
  Tail-follow catch-up animation (gradual slide). Prompt-jump via tracked line positions.
  `ui_viewport.rs`, `mod.rs:195-210`
- **Flicker detection:** Per-frame state hash tracking detects A→B→A→B oscillation patterns,
  logs warnings. `ui_frame_metrics.rs:890-1019`
- **Smoothness:** Anchor-stability recorder via row-hash comparison, feeds `smoothness` debug cmd.
  `ui_smoothness.rs:1-106`
- **Kitty keyboard protocol:** `DISAMBIGUATE_ESCAPE_CODES | REPORT_EVENT_TYPES` for unambiguous keys.
  `mod.rs:85-107`

## 5. Pain Points

- **TuiState trait:** 114+ methods; decomposition planned (doc `TUISTATE_TRAIT_DECOMPOSITION.md`).
  `mod.rs:163-167`. **Moderate** maintenance burden.
- **Full-buffer clear:** Every frame clears to `Color::Reset` — required for macOS terminal
  correctness but no diff-based optimisation. `ui.rs:2545-2550`. **Minor** perf on large terminals.
- **Fragile glyph atlas (#330):** macOS VS Code / Apple Terminal forced to 256-colour mode.
  `jcode-tui-style/color.rs:40-65`. **Minor**, macOS-specific.
- **Double-prep hysteresis:** Scrollbar visibility toggling can require two transcript-prep passes;
  hysteresis limits this to transitions only. `ui.rs:2700-2755`. **Minor** complexity.
- **Large monoliths:** `ui.rs` (3,566 lines), `ui_messages.rs` (4,167 lines), `ui_input.rs`
  (3,013 lines), `session_picker.rs` (96KB), `info_widget.rs` (75KB), `ui_pinned.rs` (72KB).
  All single files. **Moderate** maintainability risk.
- **No synchronized output:** Flicker prevention relies on full-buffer-clear + detection instead
  of terminal sync escapes. **Minor** (design choice, not a bug).
- **Resize debounce:** `last_resize_redraw` + pending flag mechanism; fine but requires awareness.
  `app.rs:925-929`. **Minor**.
- **Zero TODO/FIXME/HACK in core render paths.** Code quality is high; pain points are architectural
  (monolith sizes, trait size) rather than bug-driven.
