# TUI UX research for a coding-agent TUI

## best-regarded TUI UX patterns

### DO
- DO make the current state visible in stable panes: lazygit succeeds by showing files, commits, branches, and diffs together so users act without reconstructing Git state. https://github.com/jesseduffield/lazygit
- DO keep keyboard help contextual and one keystroke away: lazygit documents `?` for current-panel keybindings and `Tab`/`shift+Tab` for pane traversal. https://lazygit.dev/docs/
- DO use a command palette or slash commands for recall-light action discovery: OpenCode documents `/` commands and `ctrl+p` customization/command palette access. https://opencode.ai/docs/tui
- DO expose modal/action maps as documentation, not folklore: Helix publishes its complete keymap and notes terminal key conflicts. https://docs.helix-editor.com/keymap.html
- DO put persistent orientation cues in the chrome: Helix statusline has configurable left/center/right sections for mode, file, diagnostics, position, and other state. https://docs.helix-editor.com/editor.html#editorstatusline-section
- DO favor task-shaped layouts over generic widgets: Zellij layouts let users start named pane arrangements for a workflow rather than rebuild splits manually. https://zellij.dev/documentation/creating-a-layout.html
- DO support previews where context switching is expensive: Yazi's file previewer gives rich context before opening files. https://yazi-rs.github.io/docs/configuration/yazi#preview
- DO show live system state densely but grouped: btop clusters CPU, memory, disks, network, and processes into visually distinct regions. https://github.com/aristocratos/btop
- DO make agent status explicit in the footer: OpenCode examples surface interrupt, agent switching, and command palette affordances near the input. https://opencode.ai/docs/tui
- DO let users tune interaction speed and mouse capture: OpenCode exposes `scroll_speed` and `mouse` TUI options. https://opencode.ai/docs/tui
- DO provide terminal-specific setup docs for input and notifications: Claude Code documents Shift+Enter, bell, tmux, Vim mode, and theme matching. https://code.claude.com/docs/en/terminal-config

### DON'T
- DON'T hide primary actions behind memorized chords only: lazygit's panel-local `?` help is the counterexample for complex keyboard UIs. https://lazygit.dev/docs/
- DON'T use borders everywhere when grouping, alignment, and whitespace can carry hierarchy: btop's value comes from grouped regions and labels, not decorative chrome alone. https://github.com/aristocratos/btop
- DON'T force every user into mouse-first GUI metaphors: OpenCode keeps mouse optional with `mouse` configuration. https://opencode.ai/docs/tui
- DON'T make panes visually equal when attention is unequal: Zellij supports focused/floating/tabbed layouts so active work can dominate. https://zellij.dev/documentation/layouts.html
- DON'T require users to leave the TUI for routine discovery: Helix command picker exposes static commands from the interface. https://docs.helix-editor.com/commands.html
- DON'T let agent edits become invisible background magic: lazygit-style glanceable diffs are especially valuable when AI modifies many files. https://dev.to/wonderlab/terminal-power-trio-ghostty-yazi-lazygit-for-efficient-development-3iop
- DON'T overfill the footer with every shortcut: OpenCode uses a few high-value hints and moves the rest to commands/help. https://opencode.ai/docs/tui

## capability detection and graceful degradation

### DO
- DO use layered detection instead of one flag: combine explicit app flags, `NO_COLOR`, `TERM`, `COLORTERM`, terminfo, and targeted probes. https://terminfo.dev/fundamentals/color-detection
- DO treat `NO_COLOR` as a hard opt-out for colored output when present. https://no-color.org/
- DO infer 256-color support from `TERM` values ending in `-256color`, then allow user override because SSH/sudo can lie. https://terminfo.dev/fundamentals/color-detection
- DO infer truecolor primarily from `COLORTERM=truecolor` or `COLORTERM=24bit`, with overrides for known terminals. https://terminfo.dev/extensions/24-bit-truecolor
- DO use terminfo for classic capabilities but assume modern features may need env heuristics or probes. https://terminfo.dev/fundamentals/term-detection
- DO handle missing terminfo entries gracefully: Ghostty documents `xterm-ghostty` and fallback problems on older ncurses/remote hosts. https://ghostty.org/docs/help/terminfo
- DO recognize rich-terminal families explicitly when safe: Kitty has its own graphics protocol and query mechanisms. https://sw.kovidgoyal.net/kitty/graphics-protocol/
- DO gate Sixel and image previews behind capability checks or user opt-in. https://terminaltrove.com/compare/terminals
- DO separate rendering tier from input tier: Claude Code documents terminal-specific newline and tmux behavior independent of color. https://code.claude.com/docs/en/terminal-config
- DO preserve plain tty usability with ASCII symbols, no required alternate fonts, and complete text labels. https://terminfo.dev/fundamentals/term-detection

### DON'T
- DON'T assume `TERM=xterm-256color` means every modern feature exists: it mainly signals a compatibility profile and color depth. https://terminfo.dev/fundamentals/term-detection
- DON'T rely on terminfo alone for truecolor, undercurl, graphics, hyperlinks, or synchronized output. https://terminfo.dev/extensions/24-bit-truecolor
- DON'T require Unicode box drawing for comprehension: plain consoles and remote sessions may lack font or width reliability. https://www.unicode.org/reports/tr11/
- DON'T auto-enable inline images just because the terminal is modern: Kitty graphics, Sixel, and iTerm images are distinct protocols. https://sw.kovidgoyal.net/kitty/graphics-protocol/
- DON'T break on unknown `TERM_PROGRAM`: use it as a hint, not a contract. https://terminfo.dev/fundamentals/term-detection
- DON'T let 16-color fallback be an afterthought: ANSI names are user-theme slots, not fixed RGB values. https://jvns.ca/blog/2024/10/01/terminal-colours

## smoothness, rendering, and motion

### DO
- DO wrap full-frame redraws in DEC synchronized update mode where supported: `ESC[?2026h` before and `ESC[?2026l` after. https://iterm2.com/documentation-synchronized-updates.html
- DO treat synchronized output as visual atomicity, not permission to redraw thousands of unchanged lines. https://news.ycombinator.com/item?id=46699072
- DO keep render output single-writer and flush once per frame to avoid interleaved corrupt frames. https://silvery.dev/guide/input-limitations.html
- DO use ratatui's retained previous buffer model and render through `Terminal::draw` rather than clearing manually. https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html
- DO minimize expensive layout/data recomputation during resize so the next coherent frame lands quickly. https://forum.ratatui.rs/t/how-to-debug-flickering-in-my-app/106
- DO pace animation at human-useful rates: progress, spinners, and streaming indicators should signal liveness without stealing attention. https://clig.dev/#output
- DO pause or reduce animation when there is meaningful text streaming, low bandwidth, SSH, or high CPU. https://clig.dev/#output
- DO make progress states semantic: queued, running, needs input, applying patch, testing, blocked, done. https://clig.dev/#interactivity
- DO prefer stable row heights and reserved status space to avoid transcript jumpiness during agent work. https://code.claude.com/docs/en/terminal-config
- DO test tmux and multiplexers separately because synchronized output support can differ or lag terminals. https://github.com/tmux/tmux/issues/5403

### DON'T
- DON'T clear the full screen between frames: it creates flicker and erases scrollback expectations. https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html
- DON'T animate decorative elements continuously in a coding agent: motion should explain progress or focus changes. https://clig.dev/#output
- DON'T hide latency with endless spinners when a step has knowable progress or log output. https://clig.dev/#output
- DON'T assume synchronized updates fix slow render design: oversized atomic frames can still cause lag or scroll problems. https://angular.schule/blog/2026-02-claude-code-scrolling/
- DON'T redraw during resize with inconsistent intermediate state: users perceive that as flicker even if the terminal draw call is correct. https://forum.ratatui.rs/t/how-to-debug-flickering-in-my-app/106
- DON'T write to stdout from background tasks while the TUI owns the screen: route logs into the app model first. https://silvery.dev/guide/input-limitations.html

## color, themes, and accessibility

### DO
- DO design from semantic roles first: Claude Code exposes named theme roles like accent, inverse text, prompt border, and message backgrounds. https://code.claude.com/docs/en/terminal-config
- DO map semantic roles to truecolor, 256-color, 16-color, and monochrome palettes from one source theme. https://terminfo.dev/fundamentals/color-detection
- DO use truecolor escapes only when capability detection or user config allows it. https://terminfo.dev/extensions/24-bit-truecolor
- DO provide 256-color approximations and verify they remain distinct in common dark and light terminals. https://jvns.ca/blog/2024/10/01/terminal-colours
- DO make ANSI-16 fallback rely on foreground/background, bold, dim, reverse, and labels rather than hue alone. https://jvns.ca/blog/2024/10/01/terminal-colours
- DO respect `NO_COLOR` and provide non-color status markers such as text, icons with ASCII fallback, and position. https://no-color.org/
- DO check contrast ratios for truecolor theme pairs against WCAG guidance even though terminal rendering varies. https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html
- DO support user-selected light/dark mode when automatic background detection is unavailable. https://code.claude.com/docs/en/terminal-config
- DO keep accent colors sparse: use them for active focus, destructive operations, progress, and errors. https://clig.dev/#output
- DO test blue, red, and dim text on black and white backgrounds because defaults often fail readability. https://jvns.ca/blog/2024/10/01/terminal-colours

### DON'T
- DON'T encode critical state only as red/green: colorblind and monochrome users need text or shape redundancy. https://www.w3.org/WAI/WCAG21/Understanding/use-of-color.html
- DON'T assume ANSI color names have fixed RGB values: users remap them per terminal theme. https://jvns.ca/blog/2024/10/01/terminal-colours
- DON'T use dim text for essential information: many terminals render dim as low contrast or ignore it. https://fishshell.com/docs/3.3/cmds/set_color.html
- DON'T let light themes inherit dark-theme accent choices without contrast review. https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html
- DON'T overuse bright colors in dense panes: reserve high saturation for focus and urgency. https://clig.dev/#output
- DON'T make theme customization require editing code: Claude Code documents user theme config for terminal matching. https://code.claude.com/docs/en/terminal-config

## capability tier matrix

| Tier | Detection hints | Enable | Degrade or disable |
|---|---|---|---|
| plain | `TERM=dumb`, unknown tty, no `-256color`, `NO_COLOR`, SSH/sudo uncertainty | ASCII borders or whitespace groups; monochrome labels; text-first status; no required mouse; no Unicode-only icons; low redraw rate | truecolor; 256-only palettes; inline images; undercurl-only diagnostics; decorative animation; assumptions about box drawing |
| 256 | `TERM=*-256color`, terminfo colors >= 256, no truecolor hint | ANSI-256 palette approximations; Unicode if width looks safe; subtle focus color; semantic status badges with labels; ratatui diff redraw; optional mouse | truecolor gradients; image protocols; undercurl unless proven; high-frequency animation on remote links |
| rich | `COLORTERM=truecolor/24bit`, known WezTerm/Kitty/Ghostty/iTerm2, successful probes/user opt-in | truecolor semantic theme; synchronized output mode 2026; undercurl diagnostics if supported; OSC 8 links if supported; Kitty/Sixel images only opt-in; smoother progress motion | any feature that fails probes; large atomic redraws; color-only meaning; terminal-specific assumptions without overrides |
