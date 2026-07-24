# jcode TUI Redesign Spec

Status: design-only. Implementers: see §6. Grounded in `docs/research/tui-ux-research.md`
(cited as R:<section>) and `docs/research/tui-current-state.md` (cited as CS:<section>).
No rendering-architecture changes: ratatui 0.30, crossterm, event loop, full-buffer draw,
and the light-mode luminance-inversion pipeline all stay (CS:1, CS:2).

## 1. Design principles

1. **Calm by default.** The resting screen is text, whitespace, and one accent. Motion and
   saturated color appear only when state changes or attention is required (R:color "keep
   accent colors sparse"; R:motion "DON'T animate decorative elements continuously").
2. **Hierarchy through spacing, not borders.** Group with blank lines, gutters, and labels.
   Borders are reserved for one edge: the side-panel rail (R:best "DON'T use borders
   everywhere"; btop grouped regions).
3. **Every state is semantic and labeled.** queued / running / needs-input / applying-patch /
   testing / blocked / done are words plus glyphs, never color alone (R:motion "make progress
   states semantic"; R:color "DON'T encode critical state only as red/green").
4. **Roles before colors.** A small semantic palette (accent, self, agent, muted, surface,
   warn, error, info) is defined once and mapped down to 256/16/plain tiers from one source
   (R:color "design from semantic roles first").
5. **Glanceable agent work.** Tool calls and edits are summarized inline with status glyphs;
   diffs are one keystroke away, never invisible background magic (R:best lazygit diffs).
6. **Degrade honestly.** Each tier is a designed experience, not a broken rich screen:
   plain tier keeps full labels and ASCII markers (R:capability "preserve plain tty
   usability", "DON'T let 16-color fallback be an afterthought").
7. **Stable chrome.** Status bar and input never shift position or width as state changes;
   reserved space prevents transcript jumpiness (R:motion "prefer stable row heights and
   reserved status space").

## 2. Visual system

### 2.1 Semantic palette (dark-native; light mode via existing luminance inversion, CS:2)

| Role | Truecolor | 256 | ANSI-16 slot | Used for |
|---|---|---|---|---|
| text-primary | #F5F5FF | 255 | Bright White | message text, values |
| text-secondary | #DCDCD7 | 253 | White | assistant body text |
| muted | #787878 | 243 | Bright Black | metadata, timestamps, line numbers |
| faint | #505050 | 239 | Bright Black + dim | decoration only, never information |
| surface-1 | #232832 | 235 | (default bg) | user-message surface |
| surface-2 | #171B23 | 234 | (default bg) | overlays, picker rows |
| accent | #BA8BFF | 141 | Bright Magenta | focus, selection, active pane edge |
| self | #8AB4F8 | 111 | Bright Blue | user identity, gutter bar |
| agent | #81C784 | 114 | Bright Green | assistant identity, success |
| warn | #FFC107 | 214 | Bright Yellow | queued, rate-limit, cache warnings |
| error | #FF8A80 | 210 | Bright Red | failures, destructive confirm |
| info | #6ED2FF | 81 | Bright Cyan | links, hints, transport notes |

Rules: 256 indices are fixed approximations of the truecolor values, verified distinct on
dark and light (R:color "provide 256-color approximations"). ANSI-16 uses named slots the
user's terminal theme owns; never assume their RGB (R:color jvns). Contrast of all
text/background pairs checked against WCAG 4.5:1 in both dark native and post-inversion
light form (R:color "check contrast ratios"). `NO_COLOR` forces plain tier, monochrome
(R:capability no-color).

### 2.2 Typography in terminal

- **Bold**: role labels, section headers, selected row, key names in hints. Max one bold
  span per line besides headers.
- **Italic**: ephemeral previews only (streaming draft, queued text). Never for content the
  user must read; italic is inconsistently rendered.
- **Dim attribute**: decorative de-emphasis only. Essential info uses `muted` color instead
  (R:color "DON'T use dim text for essential information").
- **Underline**: hyperlinks only; OSC 8 at rich tier, plain text URL otherwise.
- **Reverse video**: selection at plain/16 tier where hue count is limited
  (R:color "ANSI-16 fallback rely on fg/bg, bold, dim, reverse, and labels").
- Glyphs: rich/256 use ▌❯ ⚙ ✓ ✗ ⠋ ▸ ▾; plain tier ASCII fallbacks `| > * + x ! ~`.

### 2.3 Spacing

- 1 blank line between message blocks; 0 inside a block. Tool groups get 1 blank line
  above and below. No consecutive blank lines.
- 2-column left gutter carries role markers (▌/❯/⚙); body text starts col 4 everywhere.
- Full-width background bands are banned; surfaces end at content width (principle 1, 2).
- Status bar and input are position-fixed with reserved widths (principle 7).

## 3. Screen-by-screen redesign

### 3.1 Chat transcript (main screen)

Today (CS:3, ui_messages.rs): user messages are full-width `surface` bands with dim
`N›` numbers; tool calls collapse to one dim line ` tool: a · b · +N more`; expanded
tool blocks in gray; streaming indicated by status-line spinner only.

Changes:
- User message: 2-col gutter bar `▌` in `self`, header row `N › name · time`, surface-1
  background only behind text width. Why: full-width bands are visual shouting; gutter +
  spacing carries hierarchy (principles 1-2; R:best borders/btop).
- Assistant: no label per paragraph; one `agent`-colored model tag at block start,
  markdown body in text-secondary. Why: calm repetition, identity once per block.
- Tool calls: one line each, glyph + name + key argument + outcome:
  `⚙ read src/app.rs ✓ 0.3s`. Glyphs: ⠋ running, ✓ done, ✗ failed, ⊘ skipped. Beyond 3
  calls, fold to `▸ N more tool calls · ctrl+o expand`. Why: glanceable agent work
  (principle 5; R:best "DON'T let agent edits become invisible background magic";
  R:motion semantic states). `✓/✗` + words carry state without color (R:color use-of-color).
- Streaming: accent caret `▍` on the last line; no per-frame recolor of text.
- Mockup (100x30):

```
 ▌2 › karangupta · 2m ago
 │ Read the diff in src/app.rs and explain the state machine. Focus on what happens
 │ when the bus disconnects mid-stream.

 jcode · claude-fable-5
 I'll trace the reconnect path first.

 ⚙ read   src/app.rs                                                          ✓ 0.3s
 ⚙ bash   cargo test -p jcode-app-core                                        ✓ 2.1s

 The machine has 4 states: `Idle`, `Connecting`, `Streaming`, `Blocked`. On bus drop,
 `Streaming` re-enters `Connecting` with backoff (app.rs:212). After the 3rd failure
 the session parks in `Blocked` and raises the notification you saw.

 ▸ 3 more tool calls · ctrl+o expand

 ▌3 › fix the retry backoff while you're in there

 ⠋ applying patch · src/app.rs +14 -2 · 12s








 ⠋ streaming… 14s · 31.2 tps · ↑12.4k ↓1.1k · +2 queued          main · fable-5 · 82% ctx
 ❯ fix the retry backoff to be exponential, capped at 30s▍
   +1 bump the integration test timeout   +2 update retry docs
```

### 3.2 Input area

Today (CS:3, ui_input.rs): prompt colored by shell/composer mode, rainbow shimmer on
queued entries, prompt-entry pulse animation.

Changes:
- Prompt glyph `❯` in accent (plain: `>`), mode shown as a word chip before the glyph
  (`$` shell, `›` chat) instead of color-only mode. Why: mode must survive monochrome
  (R:color use-of-color; R:best "put persistent orientation cues in the chrome").
- Queued messages: numbered dim lines under the input (`+1 … +2 …`), static. Remove
  rainbow queue fade and prompt pulse. Why: decorative continuous motion violates
  principle 1 and R:motion; queue order is information, render it as numbers.
- Draft-in-progress italic preview only while typing a queued message.

### 3.3 Status bar (one row)

Today (ui_input.rs `build_status_line`): single line, spinner + label + tps + tokens +
queued suffix; width and content shift per state.

Changes: fixed 3 segments (principle 7). Left: spinner + semantic state word
(`connecting / thinking / streaming / applying patch / testing / blocked / rate limited
(retry in Ns)`). Center: session + model. Right: metrics `tps · ↑in ↓out · N% ctx`.
Widths reserved; metrics truncate before the state word moves. Warnings (cache miss) are
`⚠` + word in warn, replacing the longest metric, never appended. Why: R:motion "stable
row heights and reserved status space"; R:best "make agent status explicit in the
footer" with few high-value hints (OpenCode).

### 3.4 Session picker

Today (session_picker.rs): single list overlay, no preview.

Changes: at width ≥100, two panes: session list left (48%), transcript-tail preview
right (52%) separated by one dim vertical rail. Rows: status glyph (● active, ○ idle,
⠋ running), title, project, relative time. Selected row: reverse video + accent glyph,
not a bg band. Below 100 cols: list only. Footer: `↑↓ navigate · ⏎ open · / filter ·
? keys`. Why: previews where context switching is expensive (R:best Yazi); keyboard
help one keystroke away (R:best lazygit `?`); footer not overfilled (R:best OpenCode).
Mockup (100x30):

```
┌ sessions ────────────────────────────────────────────────────────────────────────────────────┐
│ filter: /                                                                                    │
│                                                                                              │
│ ❯ ● main                    jcode-tui          2m │ ⠋ applying patch · src/app.rs +14 -2     │
│   ⠋ auth-refactor           jcode              1h │ ────────────────────────────────────     │
│   ○ docs sweep              notes              3h │ The machine has 4 states: `Idle`,        │
│   ○ reddit-dashboard        scripts            1d │ `Connecting`, `Streaming`, `Blocked`.    │
│   ○ atlas search            personal           2d │ On bus drop, `Streaming` re-enters       │
│   ○ provider doctor         jcode              3d │ `Connecting` with backoff (app.rs:212).  │
│   ○ windows terminal audit  jcode              4d │                                          │
│   ○ swarm task-graph        jcode              5d │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│                                                   │                                          │
│ ↑↓ navigate · ⏎ open · / filter · n new · ? keys                                             │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.5 Side panel

Today (ui_pinned.rs): right rail with left border, ` side ` title, border = tool-gray
when focused, dim otherwise.

Changes: keep the single left rail (only border in the app). Unfocused: rail in faint.
Focused: rail in accent. Title row gains semantic content name (` side · todos `,
` side · memory `) and the hide hint moves to the footer hint line. Why: one border
maximum (principle 2); focus shown by accent edge + title, not brightness of chrome
everywhere (R:best "DON'T make panes visually equal when attention is unequal").

### 3.6 Diff view

Today (ui_file_diff.rs): right-rail diff with +/- rows; no summary.

Changes: header = path + total `+N -M` + hunk count; each hunk gets a muted
`@@ start @@` separator line. Added rows: `agent` fg on `+`, surface-tinted bg at 256+.
Removed rows: `error` fg on `-`. Line numbers in muted. The `+`/`-` glyph and words
`added/removed` in the header carry meaning at plain tier where no bg tint exists
(R:color "never red/green only"). Why: glanceable edits (principle 5; R:best lazygit
diffs). Mockup (100x30), side panel focused:

```
 transcript                                              │ side · file diff · 1/2 hunks        ↕
                                                         │ src/app.rs  +14 -2                 ⇧Tab
 ▌3 › fix the retry backoff while you're in there        │ ──────────────────────────────────────
                                                         │ @@ fn next_backoff (app.rs:198) @@
 jcode · claude-fable-5                                  │  196      jitter: f32,
 Patch applied. Running the retry tests now.             │  197      attempt: u32,
                                                         │  198  ) -> Duration {
 ⚙ edit src/app.rs +14 -2                                │  199 -    sleep(500);
 ⚙ bash cargo test -p app ⠋ 4.6s                         │  199 +    let base = 2u64.pow(attempt)
                                                         │  200 +        .min(30_000);
                                                         │  201 +    sleep(jitter * base as f32);
                                                         │  202  }
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
                                                         │
 ⠋ testing… 5s · cargo test -p app                       main · fable-5 · 82% ctx
 ❯ ▍
```

## 4. Motion design

Keep (motion that explains state): single-cell braille spinner at 12.5fps via the existing
fast patch path (CS:1, CS:4); streaming caret; determinate progress bars whenever progress
is knowable, e.g. build %, test counts (R:motion "DON'T hide latency with endless
spinners"); tail-follow scroll catch-up.

Remove or make opt-in: 3D idle donut/orbit/gyroscope (decorative, opt-in at rich tier,
off elsewhere); rainbow prompt/queue fades; cyan↔purple tool pulse → static accent
(R:motion "motion should explain progress or focus changes").

Restraint rules (hard):
- `JCODE_REDUCED_MOTION=1`, `NO_COLOR`, or Minimal/SSH/WSL performance tier: freeze all
  decorative animation; spinner becomes ASCII `- \ | /` at 1.5fps (CS:2 liveness path).
- Plain tier: no animation beyond the ASCII spinner.
- Background/unfocused sessions: passive-liveness 1Hz only (CS:1 governor already does
  this; keep).
- During heavy streaming (>40 tps or SSH): pause spinner to 1Hz; text is the signal
  (R:motion "pause or reduce animation when there is meaningful text streaming").
- Never animate on resize; land one coherent frame (R:motion resize guidance).

## 5. Degradation tiers (per R:capability matrix)

| Feature | plain-16 | 256 | rich (WezTerm/Kitty/Ghostty/iTerm2) |
|---|---|---|---|
| Palette | ANSI-16 slots, labels carry state | 256 approximations (§2.1) | truecolor roles (§2.1) |
| Glyphs | ASCII (`| > * + x !`) | Unicode safe set | Unicode + OSC 8 links |
| User msg surface | gutter bar only, no bg | surface-1 bg to text width | surface-1 bg to text width |
| Diff emphasis | `+/-` glyphs + bold + words | fg + tinted bg | fg + tinted bg |
| Borders | ASCII `|` rail | `│` | `│` |
| Spinner | ASCII 1.5fps | braille 12.5fps | braille 12.5fps |
| Idle 3D anim | off | off | opt-in |
| Images | off | off | opt-in Kitty (existing) |
| Detection | NO_COLOR, dumb, SSH doubt, override | TERM *-256color, no COLORTERM | COLORTERM=truecolor/24bit or known TERM_PROGRAM (CS:2 order unchanged) |

Every tier renders identical text content and labels; only chrome depth changes.
macOS VS Code/AppleTerminal stay forced to 256 (glyph atlas #330, CS:5).

## 6. Implementation notes (smallest first)

1. **Semantic roles module** in `jcode-tui-style`: `enum Role {…12 roles}` + `role_color(Role, Tier)`; re-express existing fns (user_color→Self, etc.) as shims. AC: `debug_palette_json` prints roles × tiers; no widget file changed yet.
2. **Tier tables + plain tier**: add ANSI-16 mapping and `Plain` tier (NO_COLOR, dumb TERM, `JCODE_TIER` override). AC: with tier forced, rendered buffer contains no RGB/Indexed escapes; all status words present in plain snapshot test.
3. **Transcript gutter + spacing**: gutter bar, header row, surface to text width, blank-line rules. AC: snapshot tests at 60/100/140 cols; zero full-width bg cells in fixtures; light-mode inversion snapshots pass unchanged.
4. **Tool-call status lines**: per-call glyph+arg+duration line, fold after 3, `ctrl+o` expand. AC: state distinguishable in monochrome snapshot; fold/expand round-trips.
5. **Status bar segments**: reserved-width left/center/right, semantic state words. AC: state word x-position constant across all `ProcessingStatus` variants in tests; warning replaces metric, never appends.
6. **Input area**: `❯` glyph + mode chip, numbered queued lines; delete shimmer/pulse code paths. AC: mode readable in plain tier; queued order shown as numbers.
7. **Diff header/hunks**: summary row `path +N -M`, `@@` separators. AC: counts correct on fixture diffs; plain tier readable without bg.
8. **Session picker preview**: right preview pane ≥100 cols, row glyphs, footer hints. AC: preview hidden <100 cols; selection uses reverse+accent, no bg band.
9. **Motion restraint**: `JCODE_REDUCED_MOTION`, ASCII spinner fallback, rich-only idle anim behind config (default off). AC: with flag set, only ASCII spinner ticks; idle-anim crate not polled.
10. **Docs**: theme/tier config keys and tier behavior in user docs. AC: every config key in §5 documented with detection order.
