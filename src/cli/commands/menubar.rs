//! `jcode menubar` - a lightweight live indicator of how many jcode sessions
//! are running and how many are actively streaming a model response.
//!
//! On macOS this renders a native menu bar (`NSStatusItem`) item that updates
//! roughly once a second by reading the on-disk active-pid / streaming
//! registries (see `crate::session::session_counts`). On other platforms (and
//! with `--once` / `--json`) it just prints the current counts.

use anyhow::Result;
use serde::Serialize;

use crate::session::{self, SessionCounts};

#[derive(Debug, Serialize)]
struct CountsReport {
    total: usize,
    streaming: usize,
}

impl From<SessionCounts> for CountsReport {
    fn from(counts: SessionCounts) -> Self {
        Self {
            total: counts.total,
            streaming: counts.streaming,
        }
    }
}

/// Format the compact title shown next to the menu bar icon.
///
/// Always shows both the streaming and total counts directly in the menu bar
/// (e.g. "0/3" idle, "2/7" while streaming) so the live state is visible at a
/// glance without opening the dropdown. Icon-only when no sessions are running.
pub(crate) fn format_menubar_title(counts: SessionCounts) -> String {
    if counts.total == 0 {
        String::new()
    } else {
        format!("{}/{}", counts.streaming, counts.total)
    }
}

/// Human-readable one-line summary used for `--once` and the menu header.
pub(crate) fn format_menubar_summary(counts: SessionCounts) -> String {
    format!(
        "{} streaming · {} session{} running",
        counts.streaming,
        counts.total,
        if counts.total == 1 { "" } else { "s" }
    )
}

/// Title for one session row in the dropdown menu: the session's animal emoji
/// and short name, plus a streaming marker while a response is generating.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn format_session_menu_item_title(session_id: &str, streaming: bool) -> String {
    let display = crate::id::extract_session_name(session_id).unwrap_or(session_id);
    let icon = crate::id::session_icon(display);
    if streaming {
        format!("{icon} {display} · streaming")
    } else {
        format!("{icon} {display}")
    }
}

pub fn run_menubar_command(once: bool, json: bool) -> Result<()> {
    if json {
        let report = CountsReport::from(session::session_counts());
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    if once {
        println!("{}", format_menubar_summary(session::session_counts()));
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        macos::run_status_item_app();
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "The live menu bar indicator is only available on macOS. \
             Showing current counts instead (use --once or --json for scripting):"
        );
        println!("{}", format_menubar_summary(session::session_counts()));
        Ok(())
    }
}

/// Ensure a single background `jcode menubar` helper is running on macOS so the
/// session-count indicator shows up automatically for every macOS user without
/// them needing to run `jcode menubar` by hand.
///
/// This is a best-effort, fire-and-forget singleton: it records the helper's
/// PID in `~/.jcode/menubar.pid` and only spawns a new detached process when no
/// live helper is already running. Failures are silently ignored so they never
/// disrupt normal session startup.
#[cfg(target_os = "macos")]
pub fn ensure_menubar_helper_running() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    // Allow users to opt out entirely.
    if std::env::var_os("JCODE_NO_MENUBAR").is_some() {
        return;
    }

    let Ok(dir) = crate::storage::jcode_dir() else {
        return;
    };
    let pid_path = dir.join("menubar.pid");

    // If a recorded helper PID is still alive, do nothing.
    if let Ok(raw) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = raw.trim().parse::<u32>() {
            if crate::platform::is_process_running(pid) {
                return;
            }
        }
    }

    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    let mut command = Command::new(exe);
    command
        .arg("menubar")
        .env("JCODE_NO_MENUBAR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach from the parent's process group so the helper outlives this session.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    if let Ok(child) = command.spawn() {
        let _ = std::fs::write(&pid_path, child.id().to_string());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_menubar_helper_running() {}

#[cfg(target_os = "macos")]
mod macos {
    use super::{format_menubar_summary, format_menubar_title, format_session_menu_item_title};
    use crate::session::{self, SessionCounts, SessionPresence};

    use std::cell::RefCell;
    use std::cmp::Reverse;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSColor, NSFont,
        NSFontAttributeName, NSForegroundColorAttributeName, NSFontWeightRegular, NSImage, NSMenu,
        NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    };
    use objc2_foundation::{
        NSAttributedString, NSDictionary, NSObject, NSString, NSUserDefaults, ns_string,
    };

    /// Poll interval for refreshing the counts (milliseconds).
    const REFRESH_INTERVAL_MS: u64 = 1000;

    /// Autosave name under which macOS persists the status item's position.
    const STATUS_ITEM_AUTOSAVE: &str = "jcode-menubar";

    /// Number of fixed items at the end of the menu (separator, New Window,
    /// separator, Quit). Session rows are inserted between the summary header
    /// and this tail.
    const MENU_TAIL_ITEMS: isize = 4;

    define_class!(
           // SAFETY: NSObject has no subclassing requirements and MenuHandler
           // does not implement Drop.
           #[unsafe(super(NSObject))]
           #[thread_kind = MainThreadOnly]
           #[name = "JcodeMenubarHandler"]
           struct MenuHandler;

           impl MenuHandler {
               /// Open the clicked session (stored in the item's representedObject)
               /// in a new terminal window via `jcode --resume <id>`.
               #[unsafe(method(openSession:))]
               fn open_session(&self, sender: &NSMenuItem)
    {
                   let Some(object) = sender.representedObject() else {
                       return;
                   };
                   let Ok(session_id) = object.downcast::<NSString>() else {
                       return;
                   };
                   launch_jcode_window(vec!["--resume".to_string(), session_id.to_string()]);
               }

               /// Launch a brand-new jcode session in a new terminal window.
               #[unsafe(method(newWindow:))]
               fn new_window(&self, _sender: &NSMenuItem) {
                   launch_jcode_window(Vec::new());
               }
           }
       );

    impl MenuHandler {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Launch a jcode window off the main thread so slow terminal startup
    /// (osascript / `open`) never blocks the menu bar UI.
    fn launch_jcode_window(args: Vec<String>) {
        std::thread::spawn(move || {
            if let Err(err) = crate::setup_hints::launch_jcode_in_macos_terminal(&args) {
                crate::logging::warn(&format!(
                    "menubar: failed to launch jcode window ({args:?}): {err}"
                ));
            }
        });
    }

    pub(super) fn run_status_item_app() {
        let Some(mtm) = MainThreadMarker::new() else {
            crate::logging::error(
                "menubar: must run on the main thread (the process entry point); not starting",
            );
            return;
        };

        let app = NSApplication::sharedApplication(mtm);
        // Accessory: no Dock icon, no main menu, just a menu bar item.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let status_bar = NSStatusBar::systemStatusBar();
        let status_item: Retained<NSStatusItem> =
            status_bar.statusItemWithLength(NSVariableStatusItemLength);

        // Give the item a persistent identity and seed a sane preferred
        // position the first time. Without this, macOS appends brand-new
        // status items at the far left of the status area; if another app owns
        // an oversized status item (or the menu bar is crowded), a freshly
        // created item can be pushed completely off-screen and the user never
        // sees it. Seeding "NSStatusItem Preferred Position <name>" (distance
        // in points from the right screen edge) before the item is realized
        // places it among the system icons; afterwards macOS keeps tracking
        // the user's chosen position under the same key.
        unsafe {
            let defaults = NSUserDefaults::standardUserDefaults();
            let pos_key = NSString::from_str(&format!(
                "NSStatusItem Preferred Position {STATUS_ITEM_AUTOSAVE}"
            ));
            if defaults.objectForKey(&pos_key).is_none() {
                defaults.setInteger_forKey(550, &pos_key);
            }
            status_item.setAutosaveName(Some(&NSString::from_str(STATUS_ITEM_AUTOSAVE)));
        }

        // Style the button like a native menu bar extra: a template SF Symbol
        // (auto-adapts to light/dark menu bars and tinting) plus a compact
        // monospaced-digit count. Keeping the item narrow matters: macOS hides
        // wide status items first whenever the frontmost app's menus need the
        // space, which is why a verbose title appears and disappears depending
        // on which app is focused.
        let menu_bar_font_size = NSFont::menuBarFontOfSize(0.0).pointSize();
        let title_font =
            NSFont::monospacedDigitSystemFontOfSize_weight(menu_bar_font_size, unsafe {
                NSFontWeightRegular
            });
        if let Some(button) = status_item.button(mtm) {
            let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("terminal.fill"),
                Some(ns_string!("jcode sessions")),
            );
            if let Some(icon) = icon.as_deref() {
                icon.setTemplate(true);
                button.setImage(Some(icon));
                // Title on the left, icon on the right.
                button.setImagePosition(NSCellImagePosition::ImageTrailing);
            }
            button.setFont(Some(&title_font));
        }

        // The target of menu item actions. NSMenuItem holds its target weakly,
        // so keep a strong reference alive in the refresh closure below.
        let handler = MenuHandler::new(mtm);

        // Build the dropdown menu: summary header, dynamic session rows, then
        // a fixed tail (New Window / Quit).
        let menu = NSMenu::new(mtm);
        let summary_item = NSMenuItem::new(mtm);
        summary_item.setEnabled(false);
        menu.addItem(&summary_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let new_window_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("New jcode Window"),
                Some(sel!(newWindow:)),
                ns_string!("n"),
            )
        };
        unsafe { new_window_item.setTarget(Some(&handler)) };
        menu.addItem(&new_window_item);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Quit jcode menu bar"),
                Some(objc2::sel!(terminate:)),
                ns_string!("q"),
            )
        };
        menu.addItem(&quit_item);
        status_item.setMenu(Some(&menu));

        let last_sessions: RefCell<Vec<SessionPresence>> = RefCell::new(Vec::new());
        let refresh = move || {
            let mut sessions = session::session_presence();
            sessions.sort_by_key(|s| (Reverse(s.streaming), s.session_id.clone()));

            let counts = SessionCounts {
                total: sessions.len(),
                streaming: sessions.iter().filter(|s| s.streaming).count(),
            };
            if let Some(button) = status_item.button(mtm) {
                let title = format_menubar_title(counts);
                let attributed = attributed_title(&title, &title_font, counts.streaming > 0);
                button.setAttributedTitle(&attributed);
                // Tint the template icon to match: accent green while any
                // session is streaming, default (nil) otherwise so it follows
                // the menu bar's normal appearance.
                let tint: Option<Retained<NSColor>> = if counts.streaming > 0 {
                    Some(streaming_color())
                } else {
                    None
                };
                button.setContentTintColor(tint.as_deref());
            }
            summary_item.setTitle(&NSString::from_str(&format_menubar_summary(counts)));

            // Only touch the menu structure when the session set changed, so
            // an open menu is not visually disturbed every second.
            if *last_sessions.borrow() != sessions {
                rebuild_session_items(&menu, &handler, mtm, &sessions);
                *last_sessions.borrow_mut() = sessions;
            }
        };

        // Initial render before the run loop starts spinning.
        refresh();

        spawn_refresh_timer(refresh);

        // Run the Cocoa event loop. `terminate:` (the Quit item) exits the process.
        app.run();
    }

    /// Color used for the count (and icon tint) while any session is actively
    /// streaming a response. A slightly muted system green that reads well in
    /// both light and dark menu bars.
    fn streaming_color() -> Retained<NSColor> {
        NSColor::systemGreenColor()
    }

    /// Build the colored menu bar title. While streaming, the count is drawn in
    /// the streaming color; when idle it uses the standard menu bar text color
    /// (secondary, so the static total reads as quiet status rather than an
    /// alert). The monospaced-digit font is applied so the width stays stable.
    fn attributed_title(
        title: &str,
        font: &NSFont,
        streaming: bool,
    ) -> Retained<NSAttributedString> {
        let string = NSString::from_str(title);
        let color = if streaming {
            streaming_color()
        } else {
            NSColor::secondaryLabelColor()
        };
        let keys: [&NSString; 2] =
            [unsafe { NSForegroundColorAttributeName }, unsafe {
                NSFontAttributeName
            }];
        let color_obj: &AnyObject = &color;
        let font_obj: &AnyObject = font;
        let values: [&AnyObject; 2] = [color_obj, font_obj];
        let attrs = NSDictionary::from_slices(&keys, &values);
        unsafe { NSAttributedString::new_with_attributes(&string, &attrs) }
    }

    /// Replace the dynamic session rows between the summary header (index 0)
    /// and the fixed tail with one clickable row per running session.
    fn rebuild_session_items(
        menu: &NSMenu,
        handler: &MenuHandler,
        mtm: MainThreadMarker,
        sessions: &[SessionPresence],
    ) {
        while menu.numberOfItems() > 1 + MENU_TAIL_ITEMS {
            menu.removeItemAtIndex(1);
        }

        let mut index = 1;
        if !sessions.is_empty() {
            menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), index);
            index += 1;
        }
        for presence in sessions {
            let title = format_session_menu_item_title(&presence.session_id, presence.streaming);
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(&title),
                    Some(sel!(openSession:)),
                    ns_string!(""),
                )
            };
            let target: &AnyObject = handler;
            unsafe {
                item.setTarget(Some(target));
                item.setRepresentedObject(Some(&NSString::from_str(&presence.session_id)));
            }
            item.setToolTip(Some(&NSString::from_str(&format!(
                "Open {} in a new terminal window",
                presence.session_id
            ))));
            menu.insertItem_atIndex(&item, index);
            index += 1;
        }
    }

    /// Schedule a repeating timer on the main run loop that re-renders the item.
    fn spawn_refresh_timer<F>(refresh: F)
    where
        F: Fn() + 'static,
    {
        use std::ptr::NonNull;

        use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};

        let interval = REFRESH_INTERVAL_MS as f64 / 1000.0;
        let block = block2::RcBlock::new(move |_timer: NonNull<NSTimer>| {
            refresh();
        });

        unsafe {
            let timer = NSTimer::timerWithTimeInterval_repeats_block(interval, true, &block);
            let run_loop = NSRunLoop::currentRunLoop();
            // Common modes (not just the default mode) so the counts and the
            // session list keep updating while the dropdown menu is open
            // (menu tracking runs the loop in NSEventTrackingRunLoopMode).
            run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes);
            // The run loop retains the timer; keep our reference alive too so the
            // owned closure (and its captured `status_item`) lives for the whole
            // process lifetime.
            std::mem::forget(timer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionCounts;

    #[test]
    fn title_no_sessions_is_icon_only() {
        let title = format_menubar_title(SessionCounts {
            total: 0,
            streaming: 0,
        });
        assert_eq!(title, "");
    }

    #[test]
    fn title_idle_shows_zero_streaming_and_total() {
        let title = format_menubar_title(SessionCounts {
            total: 5,
            streaming: 0,
        });
        assert_eq!(title, "0/5");
    }

    #[test]
    fn title_streaming_shows_compact_ratio() {
        let title = format_menubar_title(SessionCounts {
            total: 7,
            streaming: 2,
        });
        assert_eq!(title, "2/7");
    }

    #[test]
    fn summary_pluralizes_sessions() {
        assert_eq!(
            format_menubar_summary(SessionCounts {
                total: 1,
                streaming: 0,
            }),
            "0 streaming · 1 session running"
        );
        assert_eq!(
            format_menubar_summary(SessionCounts {
                total: 3,
                streaming: 1,
            }),
            "1 streaming · 3 sessions running"
        );
    }

    #[test]
    fn session_menu_item_title_shows_icon_name_and_streaming() {
        assert_eq!(
            format_session_menu_item_title("session_buffalo_1781229104969_6d487ff77287de4f", false),
            "🐃 buffalo"
        );
        assert_eq!(
            format_session_menu_item_title("session_buffalo_1781229104969_6d487ff77287de4f", true),
            "🐃 buffalo · streaming"
        );
        // Unparseable IDs fall back to the raw ID with the generic icon.
        assert_eq!(
            format_session_menu_item_title("weird-id", false),
            "💫 weird-id"
        );
    }

    #[test]
    fn counts_report_serializes_to_json() {
        let report = CountsReport::from(SessionCounts {
            total: 4,
            streaming: 2,
        });
        let json = serde_json::to_string(&report).unwrap();
        assert_eq!(json, r#"{"total":4,"streaming":2}"#);
    }
}
