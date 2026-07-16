//! Config-driven global launch hotkeys on Windows.
//!
//! Windows registers global hotkeys through the Win32 `RegisterHotKey` API. We
//! keep the existing delivery mechanism (a hidden PowerShell listener started
//! at login) but generate it from the shared `[launch_hotkeys]` config instead
//! of hard-coding a single Alt+; binding, so per-repo hotkeys work on Windows
//! the same way they do on macOS and Linux.
//!
//! This module is the **pure** layer, mirroring `linux_niri`/`linux_env`:
//!
//! * [`chord_to_win32`] maps a jcode [`KeyChord`] onto `RegisterHotKey`
//!   modifier flags plus a virtual-key code. jcode's `cmd` modifier maps to
//!   **Alt** on Windows (matching the historical Alt+; hotkey): most Win+ key
//!   combos are reserved by the OS (Win+; opens the emoji panel), so mapping
//!   cmd -> Win would produce hotkeys that never fire.
//! * [`render_windows_listener_ps1`] renders the entire listener script from
//!   the resolved entries. Dynamic targets (`$LAST_DIR`/`$LAST_REPO`) are read
//!   from their files at keypress time, so "last project" tracks new launches
//!   without restarting the listener.
//!
//! The install glue (writing the script, startup shortcut, process restart)
//! stays in `windows_setup.rs`.

use crate::keymap::KeyChord;

/// `RegisterHotKey` modifier flags.
const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;

/// One launch hotkey resolved for the Windows listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsHotkey {
    pub chord: KeyChord,
    /// Configured directory target: an absolute path or one of the sentinels
    /// `$HOME`, `$LAST_DIR`, `$LAST_REPO` (resolved at keypress time).
    pub dir: String,
    /// Short human label, e.g. the repo's directory name.
    pub label: String,
    pub self_dev: bool,
}

/// Map a chord onto `(modifier_flags, virtual_key_code)` for `RegisterHotKey`.
/// Returns `None` for keys with no stable VK code. jcode `cmd` maps to Alt
/// (see module docs); a chord that is *both* cmd and alt still collapses onto
/// a single MOD_ALT, which is the closest expressible binding.
pub(crate) fn chord_to_win32(chord: &KeyChord) -> Option<(u32, u32)> {
    let vk = key_to_vk(&chord.key)?;
    let mut mods = 0u32;
    if chord.cmd || chord.alt {
        mods |= MOD_ALT;
    }
    if chord.ctrl {
        mods |= MOD_CONTROL;
    }
    if chord.shift {
        mods |= MOD_SHIFT;
    }
    if mods == 0 {
        // An unmodified global hotkey would swallow plain typing; refuse it.
        return None;
    }
    Some((mods, vk))
}

/// Translate a canonical jcode key token into a Win32 virtual-key code.
fn key_to_vk(key: &str) -> Option<u32> {
    let vk = match key {
        ";" => 0xBA,  // VK_OEM_1
        "=" => 0xBB,  // VK_OEM_PLUS
        "," => 0xBC,  // VK_OEM_COMMA
        "-" => 0xBD,  // VK_OEM_MINUS
        "." => 0xBE,  // VK_OEM_PERIOD
        "/" => 0xBF,  // VK_OEM_2
        "`" => 0xC0,  // VK_OEM_3
        "[" => 0xDB,  // VK_OEM_4
        "\\" => 0xDC, // VK_OEM_5
        "]" => 0xDD,  // VK_OEM_6
        "'" => 0xDE,  // VK_OEM_7
        "space" => 0x20,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        other => {
            let mut chars = other.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                if c.is_ascii_alphabetic() {
                    return Some(c.to_ascii_uppercase() as u32);
                }
                if c.is_ascii_digit() {
                    return Some(c as u32);
                }
            }
            // f1..f24 -> VK_F1 (0x70) ..
            if let Some(rest) = other.strip_prefix('f')
                && let Ok(n) = rest.parse::<u32>()
                && (1..=24).contains(&n)
            {
                return Some(0x70 + n - 1);
            }
            return None;
        }
    };
    Some(vk)
}

/// User-facing chord rendering for Windows notices: jcode's `cmd` modifier
/// shows as `Alt` because that is what [`chord_to_win32`] registers.
pub(crate) fn display_windows(chord: &KeyChord) -> String {
    let mut mapped = chord.clone();
    if mapped.cmd {
        mapped.cmd = false;
        mapped.alt = true;
    }
    mapped.display()
}

/// Escape a string for a single-quoted PowerShell literal.
fn ps_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "''"))
}

/// PowerShell expression resolving one entry's working directory at keypress
/// time, with a `$HOME` fallback for missing/stale targets.
fn ps_dir_expr(dir: &str, last_dir_file: &str, last_repo_file: &str) -> String {
    match dir {
        "$HOME" => "(Resolve-JcodeDir $null)".to_string(),
        "$LAST_DIR" => format!("(Resolve-JcodeDir {})", ps_quote(last_dir_file)),
        "$LAST_REPO" => format!("(Resolve-JcodeDir {})", ps_quote(last_repo_file)),
        path => format!("(Resolve-JcodeFixedDir {})", ps_quote(path)),
    }
}

/// Render the full hotkey-listener PowerShell script.
///
/// `launch_exe`/`launch_args` describe the terminal command that opens jcode
/// (e.g. `wt.exe` + profile args, or `alacritty -e jcode`); `{DIR}` never
/// appears in them because the working directory is passed via
/// `Start-Process -WorkingDirectory`. Entries whose chord cannot be expressed
/// are skipped. Returns `None` when nothing is registerable.
pub(crate) fn render_windows_listener_ps1(
    entries: &[WindowsHotkey],
    launch_exe: &str,
    launch_args_for: impl Fn(&WindowsHotkey) -> String,
    last_dir_file: &str,
    last_repo_file: &str,
) -> Option<String> {
    let mut registrations = String::new();
    let mut dispatch = String::new();
    let mut count = 0usize;
    for (index, entry) in entries.iter().enumerate() {
        let Some((mods, vk)) = chord_to_win32(&entry.chord) else {
            continue;
        };
        count += 1;
        // Ids only need to be process-unique; derive from the entry index.
        let id = 0x4A00 + index; // "J" namespace
        let label = display_windows(&entry.chord);
        registrations.push_str(&format!(
            "Register-JcodeHotkey -Id 0x{id:X} -Mods 0x{mods:X} -Vk 0x{vk:X} -Label {label}\n",
            label = ps_quote(&format!("{label} ({})", entry.label)),
        ));
        let dir_expr = ps_dir_expr(&entry.dir, last_dir_file, last_repo_file);
        let args = launch_args_for(entry);
        let args_part = if args.is_empty() {
            String::new()
        } else {
            format!(" -ArgumentList {}", ps_quote(&args))
        };
        dispatch.push_str(&format!(
            "            0x{id:X} {{ Start-Process {exe}{args_part} -WorkingDirectory {dir_expr} }}\n",
            exe = ps_quote(launch_exe),
        ));
    }
    if count == 0 {
        return None;
    }

    Some(format!(
        r#"# jcode global launch hotkey listener
# Auto-generated by jcode setup-hotkey from [launch_hotkeys] config. Runs at
# login via a startup shortcut. Do not edit; re-run `jcode setup-hotkey`.

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class HotKeyHelper {{
    [DllImport("user32.dll")]
    public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint fsModifiers, uint vk);
    [DllImport("user32.dll")]
    public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll")]
    public static extern int GetMessage(out MSG lpMsg, IntPtr hWnd, uint wMsgFilterMin, uint wMsgFilterMax);
    [StructLayout(LayoutKind.Sequential)]
    public struct MSG {{
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int pt_x;
        public int pt_y;
    }}
}}
"@

$MOD_NOREPEAT = 0x4000
$WM_HOTKEY = 0x0312
$script:RegisteredIds = @()

function Register-JcodeHotkey {{
    param([int]$Id, [uint32]$Mods, [uint32]$Vk, [string]$Label)
    if ([HotKeyHelper]::RegisterHotKey([IntPtr]::Zero, $Id, $Mods -bor $MOD_NOREPEAT, $Vk)) {{
        $script:RegisteredIds += $Id
    }} else {{
        Write-Warning "jcode: failed to register hotkey $Label (already claimed?)"
    }}
}}

# Resolve a dynamic launch dir from a tracking file, falling back to $HOME.
function Resolve-JcodeDir {{
    param([string]$File)
    if ($File) {{
        try {{
            $dir = (Get-Content -LiteralPath $File -ErrorAction Stop | Select-Object -First 1).Trim()
            if ($dir -and (Test-Path -LiteralPath $dir -PathType Container)) {{ return $dir }}
        }} catch {{}}
    }}
    return $env:USERPROFILE
}}

# A baked absolute dir, falling back to $HOME when it no longer exists.
function Resolve-JcodeFixedDir {{
    param([string]$Dir)
    if ($Dir -and (Test-Path -LiteralPath $Dir -PathType Container)) {{ return $Dir }}
    return $env:USERPROFILE
}}

{registrations}
if ($script:RegisteredIds.Count -eq 0) {{
    Write-Error "jcode: no launch hotkeys could be registered"
    exit 1
}}

try {{
    $msg = New-Object HotKeyHelper+MSG
    while ([HotKeyHelper]::GetMessage([ref]$msg, [IntPtr]::Zero, $WM_HOTKEY, $WM_HOTKEY) -ne 0) {{
        if ($msg.message -ne $WM_HOTKEY) {{ continue }}
        switch ($msg.wParam.ToInt32()) {{
{dispatch}        }}
    }}
}} finally {{
    foreach ($id in $script:RegisteredIds) {{
        [HotKeyHelper]::UnregisterHotKey([IntPtr]::Zero, $id) | Out-Null
    }}
}}
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    fn hk(chord_str: &str, dir: &str, label: &str, self_dev: bool) -> WindowsHotkey {
        WindowsHotkey {
            chord: chord(chord_str),
            dir: dir.to_string(),
            label: label.to_string(),
            self_dev,
        }
    }

    fn args_for(entry: &WindowsHotkey) -> String {
        let hotkey = entry.chord.canonical();
        if entry.self_dev {
            format!(r#"-e "C:\jcode.exe" --spawn-hotkey "{hotkey}" self-dev"#)
        } else {
            format!(r#"-e "C:\jcode.exe" --spawn-hotkey "{hotkey}""#)
        }
    }

    #[test]
    fn cmd_maps_to_alt_and_punctuation_maps_to_oem_vks() {
        assert_eq!(chord_to_win32(&chord("cmd+;")).unwrap(), (MOD_ALT, 0xBA));
        assert_eq!(
            chord_to_win32(&chord("cmd+shift+'")).unwrap(),
            (MOD_ALT | MOD_SHIFT, 0xDE)
        );
        assert_eq!(chord_to_win32(&chord("cmd+[")).unwrap(), (MOD_ALT, 0xDB));
        assert_eq!(chord_to_win32(&chord("cmd+]")).unwrap(), (MOD_ALT, 0xDD));
        assert_eq!(chord_to_win32(&chord("cmd+\\")).unwrap(), (MOD_ALT, 0xDC));
        assert_eq!(
            chord_to_win32(&chord("ctrl+alt+k")).unwrap(),
            (MOD_ALT | MOD_CONTROL, 'K' as u32)
        );
        assert_eq!(chord_to_win32(&chord("alt+f5")).unwrap(), (MOD_ALT, 0x74));
    }

    #[test]
    fn rejects_unmodified_and_unmappable_chords() {
        // A bare key must never become a global hotkey.
        assert!(chord_to_win32(&chord("k")).is_none());
        assert!(chord_to_win32(&chord("cmd+scrolllock")).is_none());
    }

    #[test]
    fn display_windows_renders_cmd_as_alt() {
        assert_eq!(display_windows(&chord("cmd+;")), "Alt+;");
        assert_eq!(display_windows(&chord("cmd+shift+'")), "Alt+Shift+'");
        assert_eq!(display_windows(&chord("ctrl+k")), "Ctrl+K");
    }

    #[test]
    fn listener_script_registers_each_entry_and_dispatches_dirs() {
        let entries = vec![
            hk("cmd+;", "C:\\Users\\u\\jcode", "jcode", false),
            hk("cmd+'", "$HOME", "home", false),
            hk("cmd+shift+'", "$LAST_REPO", "self-dev", true),
        ];
        let script = render_windows_listener_ps1(
            &entries,
            "wt.exe",
            args_for,
            "C:\\Users\\u\\.jcode\\hotkey\\last_dir",
            "C:\\Users\\u\\.jcode\\hotkey\\last_repo",
        )
        .unwrap();

        // Three registrations with distinct ids.
        assert_eq!(script.matches("Register-JcodeHotkey").count(), 3 + 1); // 3 calls + fn def
        assert!(script.contains("-Id 0x4A00"));
        assert!(script.contains("-Id 0x4A01"));
        assert!(script.contains("-Id 0x4A02"));

        // Fixed dir, home fallback, and dynamic repo file all present.
        assert!(script.contains("Resolve-JcodeFixedDir 'C:\\Users\\u\\jcode'"));
        assert!(script.contains("Resolve-JcodeDir $null"));
        assert!(script.contains("Resolve-JcodeDir 'C:\\Users\\u\\.jcode\\hotkey\\last_repo'"));

        // Self-dev entry passes the subcommand; others do not.
        assert_eq!(script.matches("self-dev").count(), 2); // label + args
        assert_eq!(script.matches("--spawn-hotkey").count(), 3);
        assert!(script.contains("Start-Process 'wt.exe'"));
        assert!(script.contains("-WorkingDirectory"));

        // Cleanup unregisters everything.
        assert!(script.contains("UnregisterHotKey"));
    }

    #[test]
    fn listener_script_skips_unmappable_entries() {
        let entries = vec![
            hk("cmd+scrolllock", "$HOME", "bad", false),
            hk("cmd+;", "$HOME", "home", false),
        ];
        let script = render_windows_listener_ps1(&entries, "wt.exe", args_for, "", "").unwrap();
        assert!(
            script.contains("-Id 0x4A01"),
            "kept entry keeps its slot id"
        );
        assert!(!script.contains("-Id 0x4A00"));
    }

    #[test]
    fn listener_script_none_when_nothing_registerable() {
        let entries = vec![hk("cmd+scrolllock", "$HOME", "bad", false)];
        assert!(render_windows_listener_ps1(&entries, "wt.exe", args_for, "", "").is_none());
        assert!(render_windows_listener_ps1(&[], "wt.exe", args_for, "", "").is_none());
    }

    #[test]
    fn ps_quoting_escapes_single_quotes() {
        assert_eq!(ps_quote("it's"), "'it''s'");
        let entries = vec![hk("cmd+;", "C:\\Users\\o'brien\\proj", "o'brien", false)];
        let script = render_windows_listener_ps1(&entries, "wt.exe", args_for, "", "").unwrap();
        assert!(script.contains("'C:\\Users\\o''brien\\proj'"));
    }
}
