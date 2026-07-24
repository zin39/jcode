# TUI Polish Research for Jcode

Compiled 2026-07-24. DO/DON'T bullets with source URLs, grouped by area.

---

## 1. Motion Restraint (Spinners, Animations, Reduced Motion)

- Terminals lack `prefers-reduced-motion`; closest analogs are `NO_COLOR`, `isatty()`.
  Docker drops progress bars without TTY; cargo uses determinate bars, not spinners.
  `gh` uses indeterminate spinners for downloads and has an open issue (#8536) for real progress.
- WCAG 2.2.2: motion >5s must be stoppable. WCAG 2.3.3: interaction-triggered animation must be disable-able.

**DO**
- Gate spinners behind `JCODE_NO_ANIMATION` env var or `--no-animation` flag.
  [WCAG 2.3.3](https://www.disabilityworld.org/toolkit/standards/wcag/2-3-3-animation-from-interactions/)
- Use determinate progress ("3/10") when total work is known; indeterminate only for unbounded waits (LLM).
  [gh #8536](https://github.com/cli/cli/issues/8536)
- Keep transitions under 300ms; spinner refresh at 100-150ms intervals.
  [Smashing Magazine](https://www.smashingmagazine.com/2023/11/creating-accessible-ui-animations/)
- Show static text alongside spinner: "Thinking…", "Building…".
  [MFA11y](https://www.modern-framework-accessibility.com/core-accessibility-principles-for-modern-frameworks/reduced-motion-and-animation-accessibility/accessible-loading-skeletons-and-spinners)
- Disable typewriter streaming when reduced-motion is set; render text instantly.
- Provide `Ctrl+S` to pause streaming output in the transcript.

**DON'T**
- Don't run infinite decorative spinners; every spinner must be tied to a named, completable operation.
  [SmoothUI](https://smoothui.dev/docs/guides/animation-best-practices)
- No bouncing, parallax, or dancing effects in a TUI.
  [72Tech](https://www.72technologies.com/blog/skeleton-screens-vs-spinners-when-each-wins)
- Don't let streaming cause viewport jumps when user scrolled up (scroll-anchor lock).
- Don't flash faster than 3Hz (WCAG 2.3.1 seizure risk).

---

## 2. First-Run / Empty State

- Lazygit shows a welcome message on first open but has a bug (#4052) where it persists across restarts.
  `gh auth login` uses step-by-step device-flow prompts; never leaves user at a blank prompt.
  Helix opens an empty buffer with no welcome—`hx --tutor` handles onboarding separately.
- NNGroup: empty states must provide help cues and a direct path to populate content.

**DO**
- Compact welcome panel: "Jcode is your AI coding agent. Start by asking me to write, fix, or explain code."
  [72Tech](https://www.72technologies.com/blog/empty-states-as-onboarding-surface)
- Show ghost-row example prompts ("e.g., 'Add error handling to src/api.rs'").
  [UserOnboard](https://www.useronboard.com/onboarding-ux-patterns/empty-states/)
- Defer API key / config setup until the first action that needs it.
  [UX Encyclopedia](https://ux.detroit3d.com/patterns/onboarding-empty-states.html)
- Distinguish first-use, no-results, and user-cleared states with different copy.
  [SetProduct](https://www.setproduct.com/blog/empty-state-ui-design)
- Track `has_completed_onboarding` in config; never show full welcome twice.

**DON'T**
- Don't show a blank screen on first launch.
  [NNGroup](https://www.nngroup.com/articles/empty-state-interface-design/)
- Don't use generic "No conversations" or "Empty" copy without a next-action affordance.
- Don't demand registration/keys before showing any value.
- Don't show >1 primary CTA per empty state.

---

## 3. Transcript Economy / Long-Scroll Performance

- Alternate screen buffers (fullscreen TUI) disable terminal scrollback by design (xterm spec).
  Claude Code / Ink-based TUIs have well-known complaints about clipped responses.
  Workaround: separate transcript logs + internal pager views.
  [Codex TUI docs](https://fossies.org/linux/codex-rust/docs/tui-alternate-screen.md)
  [Reddit](https://www.reddit.com/r/ClaudeAI/comments/1t6fwhx/claude_code_the_only_cli_where_scrolling_up_is_a/)
- `less`/`bat` handle large files by seeking and rendering only the visible region—the gold standard.

**DO**
- Virtual scrolling: render only visible lines + overscan. Never render full history into buffer.
  [AppMaster](https://appmaster.io/blog/swiftui-performance-long-lists)
- Internal pager (`Ctrl+T`) for full-history review.
- Compact old turns: collapse tool-call blocks to summary line ("Ran `cargo build` — 142 lines").
- Scroll-anchor: when user scrolls up, pin position; show "↓ New output" indicator.
  [Reddit scroll jump](https://www.reddit.com/r/ClaudeCode/comments/1rztl6x/anyone_else_getting_the_scroll_position_bug/)
- Log full conversation to `~/.jcode/logs/`; TUI is a viewport, not canonical record.
- After ~50 turns, offer "Archive and start fresh."

**DON'T**
- Don't render all history into terminal buffer; use lazy/virtual rendering.
- Don't rely on terminal native scrollback in alternate-screen mode.
- Don't auto-scroll to bottom on every token; batch at ~100ms or paragraph boundaries.
- Don't lose content: every response must exist in the log file even if compacted in TUI.

---

## 4. Multi-Pane Focus Indication

- Tmux: active pane gets bright border, inactive gets dim. Simplest reliable pattern.
- Zellij: issue #2180—two panes indistinguishable when `pane_frame` is disabled.
  Advantage: persistent status bar with contextual keybindings (discoverability).
  [FOSSLinux](https://www.fosslinux.com/156189/zellij-vs-tmux-the-modern-terminal-multiplexer-for-linux.htm)
- Helix: open issue #1187 requesting inactive-window dimming; no differentiation today.
- Zellij supports `Alt+hjkl` for pane cycling and targeting panes by ID without focus change.
  [Zellij CLI Recipes](https://zellij.dev/documentation/cli-recipes.html)

**DO**
- Accent-color border on focused pane; gray thin border on inactive. 2-3x brightness difference.
- Status bar with pane-nav hints (`^W h/j/k/l`) when multi-pane is active. Hide when single pane.
- Dim inactive panes 15-20% (truecolor where available); border-only fallback for basic terminals.
  [Helix #1187](https://github.com/helix-editor/helix/issues/1187)
- Support both vim-style (`Ctrl+W h/j/k/l`) and arrow-key pane navigation.
- Label each pane: "Chat", "File Tree", "Terminal", "Diff".

**DON'T**
- Don't make panes look identical; focus must be identifiable at a glance.
  [Zellij #2180](https://github.com/zellij-org/zellij/issues/2180)
- Don't hide pane-switching shortcuts; show in status bar or `?` overlay.
- Don't over-dim inactive panes below WCAG AA (4.5:1 text contrast).
- Don't auto-focus panes without explicit user action.

---

## Summary: Recommendations for Jcode

| WP  | Area               | Top Recommendation                                                 |
|-----|--------------------|--------------------------------------------------------------------|
| WP9 | Motion Restraint   | `JCODE_NO_ANIMATION` env var; scroll-anchor lock during streaming |
| WP11| First-Run          | Ghost welcome panel with 3 example prompts; deferred API key setup |
| WP12| Transcript Economy | Virtual-list rendering; compact mode for tool-call blocks; `Ctrl+T` pager |
| WP13| Multi-Pane Focus   | Accent-color focused border; `Ctrl+W` pane cycling; Zellij-style hints bar |

_Sources: WCAG 2.2/2.3, NNGroup, Zellij docs, Helix issues, gh CLI issues,
Codex TUI docs, Smashing Magazine, MFA11y, Reddit ClaudeCode._
