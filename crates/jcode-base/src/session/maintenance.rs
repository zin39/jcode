//! Background maintenance for the on-disk session store.
//!
//! Session transcripts (`<id>.json`) are kept forever, but the atomic-write
//! layer also leaves a single rolling `<id>.bak` next to each file as a
//! crash-recovery copy (see `jcode_storage::write_bytes_inner`). That backup is
//! only ever consulted when the primary `.json` is found to be corrupt on the
//! very next read. For sessions that have not been touched in weeks the primary
//! is stable, so the stale `.bak` is pure disk overhead (these accumulate into
//! gigabytes over time).
//!
//! This module prunes `.bak` files that are older than a conservative window.
//! It never touches user-facing `.json` transcripts, so no session data the
//! user cares about is lost; at worst a very old, already-stable session loses
//! its redundant recovery copy.
//!
//! It also garbage-collects *worker* session transcripts: swarm/debug worker
//! sessions (`is_debug`) are spawned in large numbers, are hidden from the
//! session picker, and are almost never revisited, yet each leaves a full
//! transcript behind. Unsaved, terminally-ended worker transcripts older than
//! a short retention window are removed together with their sibling artifacts.

use super::SessionStatus;
use crate::storage;
use chrono::{DateTime, Duration, Local};
use serde::Deserialize;
use std::path::Path;
use std::time::{Duration as StdDuration, SystemTime};

/// Backups older than this are considered safe to remove. Chosen conservatively
/// so any realistic "crashed mid-write, reopened later" scenario still has its
/// recovery copy.
const BACKUP_RETENTION_DAYS: i64 = 30;

/// Worker (debug) session transcripts older than this are garbage-collected.
/// Workers are throwaway by design; a week is plenty of time to salvage
/// anything useful from one. Overridable via
/// `JCODE_WORKER_SESSION_RETENTION_DAYS` for installs that want more or less.
const WORKER_SESSION_RETENTION_DAYS: u64 = 7;

/// Minimum interval between prune passes across all jcode processes.
///
/// The prune walks the entire sessions directory (easily 100k+ entries on a
/// long-lived install), which profiles as the single largest CPU cost of TUI
/// startup when it runs unconditionally. Backups only need to be reclaimed
/// eventually, so one pass per interval per machine is plenty; a marker file's
/// mtime coordinates that across concurrently spawned processes.
const PRUNE_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Remove stale `<id>.bak` files from the sessions directory.
///
/// Best-effort: any I/O error is ignored so this can run on a background thread
/// at startup without ever affecting launch. Skips cheaply (one stat) unless
/// the machine-wide prune interval has elapsed, so spawning many jcode
/// processes at once does not trigger many full directory walks.
pub fn prune_old_session_backups() {
    if let Ok(base) = storage::jcode_dir() {
        let sessions_dir = base.join("sessions");
        if !claim_prune_slot(&base, "sessions-bak-prune.stamp") {
            return;
        }
        prune_old_session_backups_in(&sessions_dir, Local::now());
    }
}

/// Remove old, unsaved worker (debug) session transcripts and their siblings.
///
/// Best-effort like [`prune_old_session_backups`]; runs at most once per
/// [`PRUNE_INTERVAL_SECS`] per machine via its own stamp file so the two
/// passes rate-limit independently.
pub fn prune_old_worker_sessions() {
    if let Ok(base) = storage::jcode_dir() {
        let sessions_dir = base.join("sessions");
        if !claim_prune_slot(&base, "sessions-worker-prune.stamp") {
            return;
        }
        prune_old_worker_sessions_in(&sessions_dir, SystemTime::now());
    }
}

/// Returns true when this process should run the prune pass now, updating the
/// marker so other processes (and future spawns) skip until the next interval.
///
/// The marker touch happens before the walk, so a burst of simultaneous spawns
/// resolves to at most a couple of walkers (racing between the stat and the
/// touch) instead of one per process, and steady-state spawns do a single stat.
fn claim_prune_slot(base: &Path, marker_name: &str) -> bool {
    let marker = base.join(marker_name);
    if let Ok(metadata) = std::fs::metadata(&marker)
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age.as_secs() < PRUNE_INTERVAL_SECS
    {
        return false;
    }
    // Touch (create or refresh) the marker to claim the slot.
    std::fs::write(&marker, b"").is_ok()
}

/// Core of [`prune_old_session_backups`], parameterized on the directory and
/// "now" for unit testing.
fn prune_old_session_backups_in(sessions_dir: &Path, now: DateTime<Local>) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };
    let cutoff = now - Duration::days(BACKUP_RETENTION_DAYS);
    for entry in entries.flatten() {
        let path = entry.path();
        // Only prune the atomic-write backup files; never the .json transcripts
        // or anything else (journals, tmp files, etc.).
        if path.extension().map(|e| e == "bak").unwrap_or(false)
            && let Ok(metadata) = entry.metadata()
            && metadata.is_file()
            && let Ok(modified) = metadata.modified()
        {
            let modified: DateTime<Local> = modified.into();
            if modified < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Minimal view of a session transcript for the worker-session GC decision.
///
/// Defaults are chosen so that a transcript missing any of these fields is
/// NEVER deleted: `is_debug` defaults to false (only explicitly debug-marked
/// sessions qualify) and `status` defaults to `Active`, which is non-terminal.
/// Malformed JSON fails to parse and is likewise kept.
#[derive(Deserialize)]
struct WorkerSessionStub {
    #[serde(default)]
    is_debug: bool,
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    status: SessionStatus,
}

/// A session that has ended and will not be resumed by a live process.
fn is_terminal_status(status: &SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Closed | SessionStatus::Crashed { .. } | SessionStatus::Error { .. }
    )
}

fn worker_session_retention() -> StdDuration {
    let days = std::env::var("JCODE_WORKER_SESSION_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(WORKER_SESSION_RETENTION_DAYS);
    StdDuration::from_secs(days * 24 * 60 * 60)
}

/// Core of [`prune_old_worker_sessions`], parameterized on the directory and
/// "now" for unit testing.
///
/// A transcript is deleted only when it is explicitly a debug/worker session,
/// is not saved/bookmarked, has a terminal status, and its file mtime is older
/// than the retention window. The mtime is checked before any parsing so the
/// common case (recent files) costs a single stat, not a JSON deserialize.
fn prune_old_worker_sessions_in(sessions_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };
    let retention = worker_session_retention();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().map(|e| e == "json").unwrap_or(false) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        // Cheap age gate first; only old files pay for the parse below.
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < retention {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(stub) = serde_json::from_slice::<WorkerSessionStub>(&bytes) else {
            // Unparseable transcripts are kept: deletion requires positive
            // evidence that this is a throwaway worker session.
            continue;
        };
        if !stub.is_debug || stub.saved || !is_terminal_status(&stub.status) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok()
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            // Sibling artifacts are useless once the transcript is gone.
            for sibling in [
                format!("{stem}.bak"),
                format!("{stem}.journal.jsonl"),
                format!("{stem}.task-state.md"),
            ] {
                let _ = std::fs::remove_file(sessions_dir.join(sibling));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{Duration as StdDuration, SystemTime};

    #[test]
    fn claim_prune_slot_rate_limits_within_interval_and_reclaims_after() {
        let dir = std::env::temp_dir().join(format!(
            "jcode-bak-claim-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");

        // First claim wins and creates the marker.
        assert!(
            claim_prune_slot(&dir, "sessions-bak-prune.stamp"),
            "first claim should win"
        );
        let marker = dir.join("sessions-bak-prune.stamp");
        assert!(marker.exists(), "marker should be created");

        // A concurrent/subsequent spawn within the interval is rejected.
        assert!(
            !claim_prune_slot(&dir, "sessions-bak-prune.stamp"),
            "second claim within interval should be skipped"
        );

        // Once the marker is older than the interval the slot opens again.
        let old = SystemTime::now() - StdDuration::from_secs(PRUNE_INTERVAL_SECS + 60);
        File::options()
            .write(true)
            .open(&marker)
            .and_then(|f| f.set_modified(old))
            .expect("age the marker");
        assert!(
            claim_prune_slot(&dir, "sessions-bak-prune.stamp"),
            "claim should succeed after the interval elapses"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prunes_only_old_bak_files() {
        let dir = std::env::temp_dir().join(format!(
            "jcode-bak-prune-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");

        let write = |name: &str, age_days: u64| {
            let path = dir.join(name);
            let mut f = File::create(&path).expect("create");
            f.write_all(b"{}").ok();
            if age_days > 0 {
                let mtime = SystemTime::now() - StdDuration::from_secs(age_days * 24 * 60 * 60);
                f.set_modified(mtime).expect("set mtime");
            }
            path
        };

        // 60-day-old backup: should be pruned.
        let old_bak = write("session_old.bak", 60);
        // 5-day-old backup: within window, should survive.
        let recent_bak = write("session_recent.bak", 5);
        // Transcripts must never be removed, regardless of age.
        let old_json = write("session_old.json", 60);
        let recent_json = write("session_recent.json", 0);
        // Other artifacts must be left alone.
        let journal = write("session_old.journal.jsonl", 60);

        prune_old_session_backups_in(&dir, Local::now());

        assert!(!old_bak.exists(), "old .bak should be pruned");
        assert!(recent_bak.exists(), "recent .bak must survive");
        assert!(
            old_json.exists(),
            "old .json transcript must never be removed"
        );
        assert!(recent_json.exists(), "recent .json transcript must survive");
        assert!(journal.exists(), "journals are out of scope");

        fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jcode-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_aged(dir: &Path, name: &str, contents: &str, age_days: u64) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = File::create(&path).expect("create");
        f.write_all(contents.as_bytes()).ok();
        if age_days > 0 {
            let mtime = SystemTime::now() - StdDuration::from_secs(age_days * 24 * 60 * 60);
            f.set_modified(mtime).expect("set mtime");
        }
        path
    }

    #[test]
    fn old_closed_debug_unsaved_worker_session_is_deleted_with_siblings() {
        let dir = temp_dir("worker-gc-delete");
        let json = r#"{"is_debug":true,"saved":false,"status":"Closed"}"#;
        let transcript = write_aged(&dir, "worker_old.json", json, 30);
        let bak = write_aged(&dir, "worker_old.bak", json, 30);
        let journal = write_aged(&dir, "worker_old.journal.jsonl", "{}", 30);
        let task_state = write_aged(&dir, "worker_old.task-state.md", "# task", 30);
        // Unrelated sibling of a different stem must survive.
        let other = write_aged(&dir, "other_session.bak", "{}", 30);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(!transcript.exists(), "old worker transcript should be GC'd");
        assert!(!bak.exists(), "worker .bak sibling should be GC'd");
        assert!(!journal.exists(), "worker journal sibling should be GC'd");
        assert!(
            !task_state.exists(),
            "worker task-state sibling should be GC'd"
        );
        assert!(other.exists(), "unrelated files must survive");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recent_worker_session_is_kept() {
        let dir = temp_dir("worker-gc-recent");
        let json = r#"{"is_debug":true,"saved":false,"status":"Closed"}"#;
        let transcript = write_aged(&dir, "worker_recent.json", json, 1);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(transcript.exists(), "recent worker session must survive");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saved_worker_session_is_kept() {
        let dir = temp_dir("worker-gc-saved");
        let json = r#"{"is_debug":true,"saved":true,"status":"Closed"}"#;
        let transcript = write_aged(&dir, "worker_saved.json", json, 30);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(transcript.exists(), "saved worker session must survive");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_debug_session_is_kept() {
        let dir = temp_dir("worker-gc-nondebug");
        let json = r#"{"is_debug":false,"saved":false,"status":"Closed"}"#;
        let transcript = write_aged(&dir, "user_session.json", json, 90);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(transcript.exists(), "non-debug sessions must never be GC'd");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn active_debug_session_is_kept() {
        let dir = temp_dir("worker-gc-active");
        let json = r#"{"is_debug":true,"saved":false,"status":"Active"}"#;
        let transcript = write_aged(&dir, "worker_active.json", json, 30);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(
            transcript.exists(),
            "non-terminal worker sessions must survive"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_or_field_missing_json_is_kept() {
        let dir = temp_dir("worker-gc-malformed");
        // Broken JSON: parsing fails, so no positive evidence to delete.
        let broken = write_aged(&dir, "broken.json", "{not json", 90);
        // Valid JSON but missing all fields: serde defaults must keep it
        // (is_debug defaults false, status defaults Active).
        let empty = write_aged(&dir, "empty_fields.json", "{}", 90);

        prune_old_worker_sessions_in(&dir, SystemTime::now());

        assert!(broken.exists(), "unparseable transcripts must survive");
        assert!(empty.exists(), "field-missing transcripts must survive");
        fs::remove_dir_all(&dir).ok();
    }
}
