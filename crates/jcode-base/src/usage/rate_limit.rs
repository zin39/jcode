//! Cross-process backoff for the subscription usage endpoints.
//!
//! The in-memory backoff in [`super::usage_poller_backoff`] only slows the
//! process that saw the 429. It resets on every launch, so a machine that
//! starts jcode often re-probes a throttled endpoint from scratch each time:
//! measured on 2026-08-02, 123 of 139 usage 429s were a process's *first*
//! attempt, against 124 process starts that day. The per-process backoff never
//! got a chance to apply.
//!
//! Persist the window to disk instead, the same way update checks already
//! handle GitHub's shared per-IP bucket, so a 429 seen by one process silences
//! the rest until the window expires.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long to stop polling after a 429 with no usable server hint.
///
/// Matches the in-memory `RATE_LIMIT_BACKOFF` so behavior is unsurprising when
/// only one process is running.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(900);

/// Upper bound on a persisted window, so a bogus clock or header cannot
/// suppress usage display indefinitely.
const MAX_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

fn backoff_dir() -> Option<PathBuf> {
    Some(crate::storage::jcode_dir().ok()?.join("usage-backoff"))
}

/// Path of `provider`'s marker file inside `dir`.
///
/// Takes the directory rather than resolving it, so the window logic below is
/// testable against a tempdir without reaching for the process-wide jcode home.
fn backoff_path_in(dir: &Path, provider: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    // Provider labels come from a fixed internal set, but keep the filename
    // sanitized so a future caller cannot escape the directory.
    let safe: String = provider
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Some(dir.join(format!("{safe}.until")))
}

fn backoff_path(provider: &str) -> Option<PathBuf> {
    backoff_path_in(&backoff_dir()?, provider)
}

/// Record that `provider`'s usage endpoint is throttled until now + `backoff`.
///
/// A longer existing window is kept, so repeated 429s cannot shorten a backoff
/// another process already earned.
pub(super) fn record_rate_limited(provider: &str, backoff: Option<Duration>) {
    let Some(dir) = backoff_dir() else { return };
    record_rate_limited_in(&dir, provider, backoff);
}

fn record_rate_limited_in(dir: &Path, provider: &str, backoff: Option<Duration>) {
    let backoff = backoff.unwrap_or(DEFAULT_BACKOFF).min(MAX_BACKOFF);
    let until = SystemTime::now() + backoff;
    let Some(path) = backoff_path_in(dir, provider) else {
        return;
    };
    if let Some(existing) = read_until(&path)
        && existing > until
    {
        return;
    }
    if let Ok(epoch) = until.duration_since(SystemTime::UNIX_EPOCH) {
        let _ = std::fs::write(&path, epoch.as_secs().to_string());
    }
}

/// Clear the window once a request succeeds again.
pub(super) fn clear_rate_limited(provider: &str) {
    if let Some(path) = backoff_path(provider) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
fn clear_rate_limited_in(dir: &Path, provider: &str) {
    if let Some(path) = backoff_path_in(dir, provider) {
        let _ = std::fs::remove_file(path);
    }
}

/// True while `provider`'s usage endpoint is inside a persisted backoff window.
#[cfg(test)]
fn is_rate_limited_in(dir: &Path, provider: &str) -> bool {
    rate_limited_for_in(dir, provider).is_some()
}

/// Remaining backoff for `provider`, or `None` when polling may resume.
pub(super) fn rate_limited_for(provider: &str) -> Option<Duration> {
    rate_limited_for_in(&backoff_dir()?, provider)
}

fn rate_limited_for_in(dir: &Path, provider: &str) -> Option<Duration> {
    let path = backoff_path_in(dir, provider)?;
    let until = read_until(&path)?;
    match until.duration_since(SystemTime::now()) {
        Ok(remaining) if !remaining.is_zero() => Some(remaining),
        // Window elapsed (or the clock moved backwards past it): drop the file
        // so a stale marker cannot linger.
        _ => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

fn read_until(path: &Path) -> Option<SystemTime> {
    let raw = std::fs::read_to_string(path).ok()?;
    let secs: u64 = raw.trim().parse().ok()?;
    let until = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    // Guard against a corrupt or far-future value pinning the backoff forever.
    if until > SystemTime::now() + MAX_BACKOFF {
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(until)
}

/// Whether an error string describes rate limiting rather than a real failure.
pub(super) fn looks_rate_limited(error: &str) -> bool {
    error.contains("429")
        || error.contains("rate_limit")
        || error.to_ascii_lowercase().contains("rate limit")
}

/// Backoff requested by a `retry-after` header, clamped to a sane window.
pub(super) fn retry_after_backoff(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_BACKOFF))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window logic is exercised against a tempdir rather than the process
    /// jcode home, so these tests neither touch a developer's real `~/.jcode`
    /// nor race other tests over one shared directory under the parallel runner.
    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    const PROVIDER: &str = "anthropic";

    #[test]
    fn records_and_reports_a_window() {
        let d = dir();
        assert!(
            !is_rate_limited_in(d.path(), PROVIDER),
            "should start clear"
        );

        record_rate_limited_in(d.path(), PROVIDER, Some(Duration::from_secs(300)));
        assert!(
            is_rate_limited_in(d.path(), PROVIDER),
            "a recorded window must be visible"
        );

        let remaining = rate_limited_for_in(d.path(), PROVIDER).expect("window should remain");
        assert!(
            remaining <= Duration::from_secs(300) && remaining > Duration::from_secs(240),
            "unexpected remaining: {remaining:?}"
        );

        clear_rate_limited_in(d.path(), PROVIDER);
        assert!(
            !is_rate_limited_in(d.path(), PROVIDER),
            "success must clear the window"
        );
    }

    /// The point of persisting: the window lives on disk, so a process that
    /// never made the failing request still observes it.
    #[test]
    fn window_is_persisted_to_disk() {
        let d = dir();
        record_rate_limited_in(d.path(), PROVIDER, Some(Duration::from_secs(600)));

        let path = backoff_path_in(d.path(), PROVIDER).unwrap();
        assert!(path.exists(), "backoff must be persisted to disk");
        // Reading goes through the file, not in-memory state, so a fresh
        // process reaches the same answer.
        assert!(is_rate_limited_in(d.path(), PROVIDER));
    }

    #[test]
    fn expired_window_stops_blocking_and_is_removed() {
        let d = dir();
        let path = backoff_path_in(d.path(), PROVIDER).unwrap();
        let past = SystemTime::now() - Duration::from_secs(10);
        let epoch = past.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        std::fs::write(&path, epoch.as_secs().to_string()).unwrap();

        assert!(
            !is_rate_limited_in(d.path(), PROVIDER),
            "an elapsed window must not block"
        );
        assert!(!path.exists(), "elapsed marker should be cleaned up");
    }

    #[test]
    fn a_shorter_retry_does_not_shorten_an_existing_window() {
        let d = dir();
        record_rate_limited_in(d.path(), PROVIDER, Some(Duration::from_secs(3600)));
        record_rate_limited_in(d.path(), PROVIDER, Some(Duration::from_secs(5)));

        let remaining = rate_limited_for_in(d.path(), PROVIDER).expect("window should remain");
        assert!(
            remaining > Duration::from_secs(60),
            "the longer window must win, got {remaining:?}"
        );
    }

    #[test]
    fn corrupt_or_absurd_values_do_not_pin_the_backoff() {
        let d = dir();
        let path = backoff_path_in(d.path(), PROVIDER).unwrap();

        std::fs::write(&path, "not a number").unwrap();
        assert!(
            !is_rate_limited_in(d.path(), PROVIDER),
            "garbage must not block polling"
        );

        let absurd = SystemTime::now() + Duration::from_secs(365 * 24 * 3600);
        let epoch = absurd.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        std::fs::write(&path, epoch.as_secs().to_string()).unwrap();
        assert!(
            !is_rate_limited_in(d.path(), PROVIDER),
            "a far-future value must not disable usage forever"
        );
    }

    #[test]
    fn backoff_is_clamped_to_the_maximum() {
        let d = dir();
        record_rate_limited_in(
            d.path(),
            PROVIDER,
            Some(Duration::from_secs(30 * 24 * 3600)),
        );
        let remaining = rate_limited_for_in(d.path(), PROVIDER).expect("window should exist");
        assert!(
            remaining <= MAX_BACKOFF,
            "backoff must be clamped, got {remaining:?}"
        );
    }

    /// A provider label must not be able to write outside the backoff dir.
    #[test]
    fn provider_names_cannot_escape_the_directory() {
        let d = dir();
        let path = backoff_path_in(d.path(), "../../etc/passwd").unwrap();
        assert!(
            path.starts_with(d.path()),
            "sanitized name escaped the directory: {path:?}"
        );
    }

    #[test]
    fn identifies_rate_limit_errors() {
        assert!(looks_rate_limited(
            "Usage API error (429 Too Many Requests): {}"
        ));
        assert!(looks_rate_limited("{\"type\":\"rate_limit_error\"}"));
        assert!(!looks_rate_limited("Usage API error (403 Forbidden)"));
        assert!(!looks_rate_limited("Failed to fetch usage data: timeout"));
    }

    #[test]
    fn reads_retry_after_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert!(retry_after_backoff(&headers).is_none());

        headers.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
        assert_eq!(
            retry_after_backoff(&headers),
            Some(Duration::from_secs(120))
        );

        // A hostile value is clamped rather than trusted.
        headers.insert(reqwest::header::RETRY_AFTER, "999999999".parse().unwrap());
        assert_eq!(retry_after_backoff(&headers), Some(MAX_BACKOFF));
    }
}
