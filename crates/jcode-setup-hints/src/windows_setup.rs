use super::{SetupHintsState, StartupHints, read_choice};
use crate::windows_hotkeys::{self, WindowsHotkey};
use anyhow::Result;
use jcode_storage as storage;
use std::io::{self, Write};

fn detect_terminal() -> &'static str {
    if std::env::var("WT_SESSION").is_ok() {
        "windows-terminal"
    } else if std::env::var("WEZTERM_EXECUTABLE").is_ok() || std::env::var("WEZTERM_PANE").is_ok() {
        "wezterm"
    } else if std::env::var("ALACRITTY_WINDOW_ID").is_ok() {
        "alacritty"
    } else {
        "unknown"
    }
}

fn is_alacritty_installed() -> bool {
    std::process::Command::new("where")
        .arg("alacritty")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn is_winget_available() -> bool {
    std::process::Command::new("where")
        .arg("winget")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(super) fn find_alacritty_path() -> Option<String> {
    let candidates = [
        r"C:\Program Files\Alacritty\alacritty.exe",
        r"C:\Program Files (x86)\Alacritty\alacritty.exe",
    ];
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return Some(c.to_string());
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = format!(r"{}\Microsoft\WinGet\Links\alacritty.exe", local);
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let output = std::process::Command::new("where")
        .arg("alacritty")
        .output()
        .ok()?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().next() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Resolve the `[launch_hotkeys]` config into Windows listener entries.
/// Empty config reproduces the built-in three hotkeys, matching macOS/Linux.
fn resolve_windows_hotkeys() -> Vec<WindowsHotkey> {
    let config = super::load_launch_hotkeys_config();
    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "jcode".to_string());
    let last_dir = super::mac_hotkey_last_dir_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let last_repo = super::mac_hotkey_last_repo_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::launch_hotkeys::resolve_launch_hotkeys(&config, &exe_path, &last_dir, &last_repo)
        .into_iter()
        .filter_map(|entry| {
            let chord = crate::keymap::KeyChord::parse(&entry.chord)?;
            Some(WindowsHotkey {
                chord,
                dir: entry.dir,
                self_dev: entry.args.iter().any(|a| a == "self-dev"),
                label: entry.label,
            })
        })
        .collect()
}

pub(super) fn primary_hotkey_display() -> Option<(String, String)> {
    resolve_windows_hotkeys()
        .into_iter()
        .find(|entry| !entry.self_dev && windows_hotkeys::chord_to_win32(&entry.chord).is_some())
        .map(|entry| {
            (
                entry.chord.canonical(),
                windows_hotkeys::display_windows(&entry.chord),
            )
        })
}

fn create_hotkey_shortcut(use_alacritty: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let exe_path = exe.to_string_lossy();

    let entries = resolve_windows_hotkeys();
    let last_dir = super::mac_hotkey_last_dir_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let last_repo = super::mac_hotkey_last_repo_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (launch_exe, launch_args_for): (String, Box<dyn Fn(&WindowsHotkey) -> String>) =
        if use_alacritty {
            let alacritty_path = find_alacritty_path().unwrap_or_else(|| "alacritty".to_string());
            let exe = exe_path.to_string();
            (
                alacritty_path,
                Box::new(move |hk: &WindowsHotkey| {
                    let hotkey = hk.chord.canonical();
                    if hk.self_dev {
                        format!("-e \"{exe}\" --spawn-hotkey \"{hotkey}\" self-dev")
                    } else {
                        format!("-e \"{exe}\" --spawn-hotkey \"{hotkey}\"")
                    }
                }),
            )
        } else {
            let exe = exe_path.to_string();
            (
                "wt.exe".to_string(),
                Box::new(move |hk: &WindowsHotkey| {
                    let hotkey = hk.chord.canonical();
                    if hk.self_dev {
                        format!(
                            "-p \"Command Prompt\" \"{exe}\" --spawn-hotkey \"{hotkey}\" self-dev"
                        )
                    } else {
                        format!("-p \"Command Prompt\" \"{exe}\" --spawn-hotkey \"{hotkey}\"")
                    }
                }),
            )
        };

    let Some(ps1_content) = windows_hotkeys::render_windows_listener_ps1(
        &entries,
        &launch_exe,
        |hk| launch_args_for(hk),
        &last_dir,
        &last_repo,
    ) else {
        anyhow::bail!("no registerable launch hotkeys in config");
    };

    let hotkey_dir = storage::jcode_dir()?.join("hotkey");
    std::fs::create_dir_all(&hotkey_dir)?;

    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-Process powershell, pwsh -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -like '*jcode-hotkey*' } | Stop-Process -Force -ErrorAction SilentlyContinue",
        ])
        .output();

    let ps1_path = hotkey_dir.join("jcode-hotkey.ps1");
    std::fs::write(&ps1_path, &ps1_content)?;
    let _ = std::fs::remove_file(hotkey_dir.join("jcode-hotkey-launcher.vbs"));

    let startup_dir = format!(
        "{}\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
        std::env::var("APPDATA").unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Roaming".into())
    );

    // Point the Startup shortcut directly at PowerShell instead of adding a
    // hidden VBScript trampoline. The listener file is generated locally, so
    // RemoteSigned is sufficient and avoids the broad ExecutionPolicy Bypass
    // behavior that endpoint security products reasonably treat as suspicious.
    let ps1_path_for_powershell = ps1_path.to_string_lossy().replace('\'', "''");

    let create_startup_lnk = format!(
        r#"
$ErrorActionPreference = "Stop"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut("{startup_dir}\jcode-hotkey.lnk")
$shortcut.TargetPath = "powershell.exe"
$shortcut.Arguments = '-NoProfile -ExecutionPolicy RemoteSigned -WindowStyle Hidden -File "{ps1_path}"'
$shortcut.Description = "jcode Alt+; hotkey listener"
$shortcut.WindowStyle = 7
$shortcut.Save()
Write-Output "OK"
"#,
        startup_dir = startup_dir,
        ps1_path = ps1_path_for_powershell,
    );

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &create_startup_lnk])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create startup shortcut: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("OK") {
        anyhow::bail!("Startup shortcut creation did not confirm success");
    }

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let start_output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "RemoteSigned",
            "-WindowStyle",
            "Hidden",
            "-File",
            &ps1_path.to_string_lossy(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();

    if let Err(e) = start_output {
        eprintln!(
            "  \x1b[33m⚠\x1b[0m  Could not start hotkey listener now: {}",
            e
        );
        eprintln!("    It will start automatically on next login.");
    }

    Ok(())
}

/// Build the TUI startup notice for the Windows launch hotkeys (or `None` when
/// there is nothing to show). Mirrors the macOS/Linux notices with Alt-style
/// chords. Only shown once the listener is configured, since Windows needs the
/// interactive `jcode setup-hotkey` flow to install it.
pub(super) fn windows_launch_hotkeys_notice(state: &SetupHintsState) -> Option<StartupHints> {
    if !state.hotkey_configured {
        return None;
    }
    let config = super::load_launch_hotkeys_config();
    if config.enabled == Some(false) {
        return None;
    }

    let last_dir = super::mac_hotkey_last_dir_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let last_repo = super::mac_hotkey_last_repo_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let rows: Vec<super::LaunchHotkeyRow> = resolve_windows_hotkeys()
        .into_iter()
        .filter(|hk| windows_hotkeys::chord_to_win32(&hk.chord).is_some())
        .map(|hk| {
            let cwd = crate::launch_hotkeys::resolve_target_dir(&hk.dir, &last_dir, &last_repo);
            super::LaunchHotkeyRow {
                chord: hk.chord.canonical(),
                display: windows_hotkeys::display_windows(&hk.chord),
                label: hk.label.clone(),
                cwd_display: cwd.display().to_string(),
                self_dev: hk.self_dev,
            }
        })
        .collect();

    let lines =
        super::launch_hotkey_notice_lines(&rows, &state.launch_hotkey_usage, state.launch_count)?;

    Some(StartupHints::with_status_and_display(
        "Launch hotkeys available".to_string(),
        "Launch hotkeys",
        format!(
            "Configured Jcode launch hotkeys:\n{}\n\nThese fire system-wide.",
            lines.join("\n")
        ),
    ))
}

/// Reinstall the Windows hotkey listener after the `[launch_hotkeys]` config
/// changed. No-op unless the user already configured the hotkey (we never
/// install behind someone who opted out). Best-effort.
pub(super) fn reinstall_windows_launch_hotkeys() {
    let state = SetupHintsState::load();
    if !state.hotkey_configured {
        return;
    }
    match refresh_windows_launch_hotkeys() {
        Ok(()) => jcode_logging::info("Reinstalled Windows launch hotkeys after config change"),
        Err(err) => jcode_logging::warn(&format!(
            "failed to reinstall Windows launch hotkeys: {err}"
        )),
    }
}

pub(super) fn refresh_windows_launch_hotkeys() -> Result<()> {
    let use_alacritty = detect_terminal() == "alacritty" || is_alacritty_installed();
    create_hotkey_shortcut(use_alacritty)
}

fn install_alacritty() -> Result<()> {
    eprintln!("  Installing Alacritty via winget...");
    eprintln!("  (Windows may ask for permission to install)\n");

    let status = std::process::Command::new("winget")
        .args([
            "install",
            "-e",
            "--id",
            "Alacritty.Alacritty",
            "--accept-source-agreements",
        ])
        .status()?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("winget install failed (exit code: {:?})", status.code())
    }
}

fn nudge_hotkey(state: &mut SetupHintsState) -> bool {
    let terminal = detect_terminal();
    let using_alacritty = terminal == "alacritty" || is_alacritty_installed();

    let terminal_name = if using_alacritty {
        "Alacritty"
    } else {
        "Windows Terminal"
    };

    eprintln!("\x1b[36m┌─────────────────────────────────────────────────────────────┐\x1b[0m");
    eprintln!(
        "\x1b[36m│\x1b[0m \x1b[1m💡 Set up Alt+; to launch jcode from anywhere?\x1b[0m              \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    Creates a global hotkey - no extra software needed.       \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    Opens jcode in {:<39}    \x1b[36m│\x1b[0m",
        format!("{}.", terminal_name)
    );
    eprintln!(
        "\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    \x1b[32m[y]\x1b[0m Set up   \x1b[90m[n]\x1b[0m Not now   \x1b[90m[d]\x1b[0m Don't ask again        \x1b[36m│\x1b[0m"
    );
    eprintln!("\x1b[36m└─────────────────────────────────────────────────────────────┘\x1b[0m");
    eprint!("\x1b[36m  >\x1b[0m ");
    let _ = io::stderr().flush();

    let choice = read_choice();

    match choice.as_str() {
        "y" | "yes" => {
            eprint!("\n");
            match create_hotkey_shortcut(using_alacritty) {
                Ok(()) => {
                    state.hotkey_configured = true;
                    state.launch_hotkey_tracking_version = super::LAUNCH_HOTKEY_TRACKING_VERSION;
                    let _ = state.save();
                    eprintln!(
                        "  \x1b[32m✓\x1b[0m Created hotkey (\x1b[1mAlt+;\x1b[0m) → {} + jcode",
                        terminal_name
                    );
                    eprintln!();
                    true
                }
                Err(e) => {
                    eprintln!("  \x1b[31m✗\x1b[0m Failed to create hotkey: {}", e);
                    eprintln!(
                        "    You can set it up manually later with: \x1b[1mjcode setup-hotkey\x1b[0m"
                    );
                    eprintln!();
                    false
                }
            }
        }
        "d" | "dont" => {
            state.hotkey_dismissed = true;
            let _ = state.save();
            false
        }
        _ => false,
    }
}

fn nudge_alacritty(state: &mut SetupHintsState) -> bool {
    let terminal = detect_terminal();

    let current_terminal = match terminal {
        "windows-terminal" => "Windows Terminal",
        "wezterm" => "WezTerm",
        _ => "your current terminal",
    };

    eprintln!("\x1b[36m┌─────────────────────────────────────────────────────────────┐\x1b[0m");
    eprintln!(
        "\x1b[36m│\x1b[0m \x1b[1m💡 Alacritty: the fastest terminal for jcode\x1b[0m               \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    {:<55} \x1b[36m│\x1b[0m",
        format!("You're using {}.", current_terminal)
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    Alacritty is GPU-accelerated with the lowest latency.    \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m                                                             \x1b[36m│\x1b[0m"
    );
    eprintln!(
        "\x1b[36m│\x1b[0m    \x1b[32m[y]\x1b[0m Install   \x1b[90m[n]\x1b[0m Not now   \x1b[90m[d]\x1b[0m Don't ask again       \x1b[36m│\x1b[0m"
    );
    eprintln!("\x1b[36m└─────────────────────────────────────────────────────────────┘\x1b[0m");
    eprint!("\x1b[36m  >\x1b[0m ");
    let _ = io::stderr().flush();

    let choice = read_choice();

    match choice.as_str() {
        "y" | "yes" => {
            eprint!("\n");
            if !is_winget_available() {
                eprintln!("  \x1b[33m⚠\x1b[0m  winget not found. Install Alacritty manually:");
                eprintln!("     https://alacritty.org/");
                eprintln!();
                eprintln!("     Or install winget first: https://aka.ms/getwinget");
                eprintln!();
                return false;
            }

            match install_alacritty() {
                Ok(()) => {
                    state.alacritty_configured = true;
                    let _ = state.save();
                    eprintln!("  \x1b[32m✓\x1b[0m Alacritty installed!");

                    if state.hotkey_configured {
                        eprintln!("  Updating hotkey to use Alacritty...");
                        match create_hotkey_shortcut(true) {
                            Ok(()) => {
                                eprintln!(
                                    "  \x1b[32m✓\x1b[0m Hotkey updated: \x1b[1mAlt+;\x1b[0m → Alacritty + jcode"
                                );
                            }
                            Err(e) => {
                                eprintln!("  \x1b[33m⚠\x1b[0m  Could not update hotkey: {}", e);
                            }
                        }
                    }
                    eprintln!();
                    true
                }
                Err(e) => {
                    eprintln!("  \x1b[31m✗\x1b[0m Failed to install Alacritty: {}", e);
                    eprintln!("    Install manually: https://alacritty.org/");
                    eprintln!();
                    false
                }
            }
        }
        "d" | "dont" => {
            state.alacritty_dismissed = true;
            let _ = state.save();
            false
        }
        _ => false,
    }
}

fn prompt_try_it_out(installed_alacritty: bool) {
    eprintln!("\x1b[32m┌─────────────────────────────────────────────────────────────┐\x1b[0m");
    eprintln!(
        "\x1b[32m│\x1b[0m \x1b[1m✨ All set! Try it out:\x1b[0m                                     \x1b[32m│\x1b[0m"
    );
    eprintln!(
        "\x1b[32m│\x1b[0m                                                             \x1b[32m│\x1b[0m"
    );
    eprintln!(
        "\x1b[32m│\x1b[0m    Press \x1b[1mAlt+;\x1b[0m from anywhere to launch jcode.                \x1b[32m│\x1b[0m"
    );
    eprintln!(
        "\x1b[32m│\x1b[0m    Inside jcode, \x1b[1mAlt+Shift+;\x1b[0m opens a new session here.      \x1b[32m│\x1b[0m"
    );
    if installed_alacritty {
        eprintln!(
            "\x1b[32m│\x1b[0m    It will open in \x1b[1mAlacritty\x1b[0m for maximum performance.    \x1b[32m│\x1b[0m"
        );
    }
    eprintln!(
        "\x1b[32m│\x1b[0m                                                             \x1b[32m│\x1b[0m"
    );
    eprintln!(
        "\x1b[32m│\x1b[0m    \x1b[90m(Starting jcode normally in 3 seconds...)\x1b[0m                 \x1b[32m│\x1b[0m"
    );
    eprintln!("\x1b[32m└─────────────────────────────────────────────────────────────┘\x1b[0m");
    eprintln!();

    std::thread::sleep(std::time::Duration::from_secs(3));
}

pub(super) fn maybe_show_windows_setup_hints(
    state: &mut SetupHintsState,
    startup_hints: Option<StartupHints>,
) -> Option<StartupHints> {
    if state.launch_count % 3 != 0 {
        return startup_hints;
    }

    let terminal = detect_terminal();
    let already_using_alacritty = terminal == "alacritty";

    if already_using_alacritty {
        state.alacritty_configured = true;
        state.alacritty_dismissed = true;
        let _ = state.save();
    }

    let wants_hotkey_nudge = !state.hotkey_configured && !state.hotkey_dismissed;
    let wants_alacritty_nudge =
        !state.alacritty_configured && !state.alacritty_dismissed && !already_using_alacritty;

    // Stop pestering the user once we have shown the nudge prompt enough times,
    // even if they never explicitly chose "Don't ask again".
    if (wants_hotkey_nudge || wants_alacritty_nudge) && !state.nudge_budget_remaining() {
        return startup_hints;
    }

    let mut did_setup_hotkey = false;
    let mut did_install_alacritty = false;

    if wants_hotkey_nudge {
        state.record_nudge_shown();
        did_setup_hotkey = nudge_hotkey(state);
    }

    if wants_alacritty_nudge {
        state.record_nudge_shown();
        did_install_alacritty = nudge_alacritty(state);
    }

    if did_setup_hotkey || (did_install_alacritty && state.hotkey_configured) {
        prompt_try_it_out(did_install_alacritty);
    }

    startup_hints
}

pub(super) fn run_setup_hotkey_windows() -> Result<()> {
    let mut state = SetupHintsState::load();
    let terminal = detect_terminal();
    let already_using_alacritty = terminal == "alacritty";

    eprintln!("\x1b[1mjcode setup-hotkey\x1b[0m");
    eprintln!();

    eprintln!(
        "  Detected terminal: {}",
        match terminal {
            "windows-terminal" => "Windows Terminal",
            "wezterm" => "WezTerm",
            "alacritty" => "Alacritty",
            _ => "Unknown",
        }
    );

    if is_alacritty_installed() && !already_using_alacritty {
        eprintln!("  Alacritty: \x1b[32minstalled\x1b[0m");
    } else if already_using_alacritty {
        eprintln!("  Alacritty: \x1b[32mactive\x1b[0m");
    } else {
        eprintln!("  Alacritty: \x1b[90mnot installed\x1b[0m");
    }
    eprintln!();

    let mut installed_alacritty = false;
    if !already_using_alacritty && !is_alacritty_installed() {
        eprintln!(
            "  Alacritty is the fastest terminal emulator (GPU-accelerated, lowest latency)."
        );
        eprint!("  Install Alacritty? \x1b[32m[y]\x1b[0m/\x1b[90m[n]\x1b[0m: ");
        let _ = io::stderr().flush();
        let choice = read_choice();
        if choice == "y" || choice == "yes" {
            if !is_winget_available() {
                eprintln!("\n  \x1b[33m⚠\x1b[0m  winget not found. Install Alacritty manually:");
                eprintln!("     https://alacritty.org/\n");
            } else {
                match install_alacritty() {
                    Ok(()) => {
                        state.alacritty_configured = true;
                        installed_alacritty = true;
                        eprintln!("  \x1b[32m✓\x1b[0m Alacritty installed!\n");
                    }
                    Err(e) => {
                        eprintln!("  \x1b[31m✗\x1b[0m Install failed: {}\n", e);
                    }
                }
            }
        }
        eprintln!();
    }

    let use_alacritty = already_using_alacritty || is_alacritty_installed();
    let terminal_name = if use_alacritty {
        "Alacritty"
    } else {
        "Windows Terminal"
    };

    eprintln!(
        "  Setting up global launch hotkeys → {} + jcode...",
        terminal_name
    );

    match create_hotkey_shortcut(use_alacritty) {
        Ok(()) => {
            state.hotkey_configured = true;
            state.launch_hotkey_tracking_version = super::LAUNCH_HOTKEY_TRACKING_VERSION;
            let _ = state.save();
            eprintln!("  \x1b[32m✓\x1b[0m Created launch hotkeys");
            eprintln!();
            eprintln!("  Press these anywhere, system-wide:");
            for hk in resolve_windows_hotkeys() {
                if windows_hotkeys::chord_to_win32(&hk.chord).is_some() {
                    let suffix = if hk.self_dev { " [self-dev]" } else { "" };
                    eprintln!(
                        "    \x1b[1m{}\x1b[0m → {}{}",
                        windows_hotkeys::display_windows(&hk.chord),
                        hk.label,
                        suffix
                    );
                }
            }
            eprintln!();
            super::install_cli_launch_hints_notice();
            prompt_try_it_out(installed_alacritty);
        }
        Err(e) => {
            eprintln!("  \x1b[31m✗\x1b[0m Failed: {}", e);
        }
    }

    Ok(())
}

pub(super) fn create_windows_desktop_shortcut(state: &mut SetupHintsState) -> Result<()> {
    let exe = std::env::current_exe()?;
    let exe_path = exe.to_string_lossy();

    let (target, args) = if is_alacritty_installed() {
        let alacritty = find_alacritty_path().unwrap_or_else(|| "alacritty".to_string());
        (alacritty, format!("-e \"{}\"", exe_path))
    } else {
        (exe_path.to_string(), String::new())
    };

    let desktop_dir = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".into());
    let shortcut_path = format!("{}\\Desktop\\jcode.lnk", desktop_dir);

    let ps_script = format!(
        r#"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut("{shortcut_path}")
$shortcut.TargetPath = "{target}"
$shortcut.Arguments = '{args}'
$shortcut.Description = "jcode - AI coding agent"
$shortcut.Save()
Write-Output "OK"
"#,
        shortcut_path = shortcut_path,
        target = target,
        args = args,
    );

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_script])
        .output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("OK") {
            state.desktop_shortcut_created = true;
            let _ = state.save();
            jcode_logging::info(&format!("Created desktop shortcut: {}", shortcut_path));
        }
    }

    Ok(())
}
