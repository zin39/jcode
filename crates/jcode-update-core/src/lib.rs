use anyhow::Result;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub const BACKGROUND_UPDATE_THRESHOLD: Duration = Duration::from_secs(15);
const DOWNLOAD_PROGRESS_BAR_WIDTH: usize = 24;

/// Summary emitted when `git pull` cannot reconcile the local and upstream
/// histories on its own (diverged branches, non-fast-forward, unrelated
/// histories). Callers use this to recognize a divergence and offer a merge
/// affordance instead of a generic failure.
pub const GIT_PULL_DIVERGED_SUMMARY: &str =
    "Local and upstream have diverged, so the update could not fast-forward.";

#[derive(Debug, Clone, Copy)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct UpdateEstimate {
    pub duration: Duration,
    pub summary: String,
    pub should_background: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(rename = "name")]
    pub _name: Option<String>,
    #[serde(rename = "html_url")]
    pub _html_url: String,
    #[serde(rename = "published_at")]
    pub _published_at: Option<String>,
    pub assets: Vec<GitHubAsset>,
    #[serde(default)]
    #[serde(rename = "target_commitish")]
    pub _target_commitish: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(rename = "size")]
    pub _size: u64,
}

pub enum PreparedUpdate {
    None {
        current: String,
    },
    Stable {
        release: GitHubRelease,
        estimate: UpdateEstimate,
    },
    MainSource {
        latest_sha: String,
        estimate: UpdateEstimate,
    },
}

pub enum UpdateCheckResult {
    NoUpdate,
    UpdateAvailable {
        current: String,
        latest: String,
        _release: GitHubRelease,
    },
    UpdateInstalled {
        version: String,
        path: PathBuf,
    },
    Error(String),
}

pub fn format_duration_estimate(duration: Duration) -> String {
    match duration.as_secs() {
        0..=15 => "under 15s".to_string(),
        16..=45 => "~30s".to_string(),
        46..=90 => "~1 min".to_string(),
        91..=180 => "~2-3 min".to_string(),
        181..=360 => "~3-6 min".to_string(),
        _ => "5+ min".to_string(),
    }
}

pub fn estimate_release_update_duration(
    asset_size_bytes: u64,
    historical_secs: Option<f64>,
) -> Duration {
    if let Some(previous) = historical_secs {
        return Duration::from_secs(previous.max(5.0).round() as u64);
    }

    let size_mb = asset_size_bytes as f64 / (1024.0 * 1024.0);
    let secs = if size_mb <= 15.0 {
        10
    } else if size_mb <= 35.0 {
        20
    } else if size_mb <= 60.0 {
        35
    } else {
        50
    };
    Duration::from_secs(secs)
}

pub fn estimate_source_update_duration(
    repo_exists: bool,
    has_previous_build: bool,
    historical_secs: Option<f64>,
) -> Duration {
    if let Some(previous) = historical_secs {
        return Duration::from_secs(previous.max(20.0).round() as u64);
    }

    let secs = if !repo_exists {
        420
    } else if has_previous_build {
        90
    } else {
        180
    };
    Duration::from_secs(secs)
}

pub fn update_estimate(summary: String, duration: Duration) -> UpdateEstimate {
    UpdateEstimate {
        duration,
        summary,
        should_background: duration >= BACKGROUND_UPDATE_THRESHOLD,
    }
}

pub fn get_asset_name() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "jcode-linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "jcode-linux-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "jcode-macos-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "jcode-macos-aarch64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "jcode-windows-x86_64.exe"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "jcode-windows-aarch64.exe"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
    )))]
    {
        "jcode-unknown"
    }
}

pub fn summarize_git_pull_failure(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let text = stderr.trim();
    if text.is_empty() {
        return "git pull failed".to_string();
    }

    if git_pull_failure_is_divergence(text) {
        return GIT_PULL_DIVERGED_SUMMARY.to_string();
    }

    if text.contains("There is no tracking information for the current branch") {
        return "git pull failed: current branch has no upstream tracking branch".to_string();
    }

    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("hint:"))
        .unwrap_or("git pull failed");
    let line = line.strip_prefix("fatal: ").unwrap_or(line);
    if line.eq_ignore_ascii_case("git pull failed") {
        "git pull failed".to_string()
    } else {
        format!("git pull failed: {}", line)
    }
}

/// Whether `git pull` stderr indicates the local and upstream branches have
/// diverged (and therefore need a manual merge/rebase, not a fast-forward).
pub fn git_pull_failure_is_divergence(stderr: &str) -> bool {
    stderr.contains("Need to specify how to reconcile divergent branches")
        || stderr.contains("Not possible to fast-forward")
        || stderr.contains("refusing to merge unrelated histories")
        || stderr.contains("have diverged")
}

/// Whether a `summarize_git_pull_failure` summary describes a divergence.
pub fn summary_is_divergence(summary: &str) -> bool {
    summary == GIT_PULL_DIVERGED_SUMMARY
}

pub fn parse_sha256sums(contents: &str) -> Result<HashMap<String, String>> {
    let mut checksums = HashMap::new();
    for (line_idx, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(checksum) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            anyhow::bail!("Invalid SHA256SUMS line {}: missing filename", line_idx + 1);
        };
        if parts.next().is_some() {
            anyhow::bail!(
                "Invalid SHA256SUMS line {}: expected '<sha256>  <filename>'",
                line_idx + 1
            );
        }
        if checksum.len() != 64 || !checksum.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!(
                "Invalid SHA256SUMS line {}: invalid SHA256 digest",
                line_idx + 1
            );
        }

        let name = name.trim_start_matches('*').to_string();
        let previous = checksums.insert(name.clone(), checksum.to_ascii_lowercase());
        if previous.is_some() {
            anyhow::bail!(
                "Invalid SHA256SUMS line {}: duplicate entry for {}",
                line_idx + 1,
                name
            );
        }
    }
    Ok(checksums)
}

pub fn verify_asset_checksum_text(contents: &str, asset_name: &str, bytes: &[u8]) -> Result<()> {
    let checksums = parse_sha256sums(contents)?;
    let expected = checksums
        .get(asset_name)
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS does not list {}", asset_name))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        anyhow::bail!(
            "Checksum mismatch for {}: expected {}, got {}",
            asset_name,
            expected,
            actual
        );
    }
    Ok(())
}

pub fn version_is_newer(release: &str, current: &str) -> bool {
    let parse = |v: &str| -> (u32, u32, u32) {
        let v = v.trim_start_matches('v');
        let parts: Vec<&str> = v.split('.').collect();
        let major = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };

    let r = parse(release);
    let c = parse(current);
    r > c
}

pub fn format_download_progress_bar(progress: DownloadProgress) -> String {
    let human_downloaded = format_bytes(progress.downloaded);
    let Some(total) = progress.total.filter(|total| *total > 0) else {
        return format!("Downloading update... {} downloaded", human_downloaded);
    };

    let ratio = (progress.downloaded as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (ratio * DOWNLOAD_PROGRESS_BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(DOWNLOAD_PROGRESS_BAR_WIDTH);
    let empty = DOWNLOAD_PROGRESS_BAR_WIDTH.saturating_sub(filled);
    let percent = (ratio * 100.0).round() as u64;
    format!(
        "Downloading update... [{}{}] {:>3}% ({}/{})",
        "█".repeat(filled),
        "░".repeat(empty),
        percent,
        human_downloaded,
        format_bytes(total)
    )
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.1} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.1} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.1} KiB", bytes_f / KIB)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_works() {
        assert!(version_is_newer("v0.2.0", "0.1.9"));
        assert!(!version_is_newer("v0.1.0", "0.1.0"));
    }

    #[test]
    fn asset_name_is_supported() {
        assert_ne!(get_asset_name(), "jcode-unknown");
    }

    #[test]
    fn progress_bar_known_total() {
        let text = format_download_progress_bar(DownloadProgress {
            downloaded: 512,
            total: Some(1024),
        });
        assert!(text.contains("50%"));
        assert!(text.contains("512 B/1.0 KiB"));
    }

    #[test]
    fn progress_bar_unknown_total() {
        let text = format_download_progress_bar(DownloadProgress {
            downloaded: 2048,
            total: None,
        });
        assert_eq!(text, "Downloading update... 2.0 KiB downloaded");
    }

    #[test]
    fn sha256sums_accepts_standard_and_binary_lines() {
        let checksums = parse_sha256sums(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  jcode-linux-x86_64\n\
             bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *jcode-macos-aarch64\n",
        )
        .unwrap();
        assert_eq!(
            checksums.get("jcode-linux-x86_64").map(String::as_str),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            checksums.get("jcode-macos-aarch64").map(String::as_str),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn checksum_verification_accepts_matching_digest() {
        let bytes = b"hello world";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let sums = format!("{}  jcode-linux-x86_64\n", digest);
        verify_asset_checksum_text(&sums, "jcode-linux-x86_64", bytes).unwrap();
    }

    #[test]
    fn checksum_verification_rejects_mismatch() {
        let sums = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  jcode-linux-x86_64\n";
        let err = verify_asset_checksum_text(sums, "jcode-linux-x86_64", b"hello")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Checksum mismatch"));
    }

    #[test]
    fn checksum_verification_requires_asset_entry() {
        let sums = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other\n";
        let err = verify_asset_checksum_text(sums, "jcode-linux-x86_64", b"hello")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not list"));
    }

    #[test]
    fn sha256sums_rejects_invalid_digest() {
        let err = parse_sha256sums("not-a-digest  jcode\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid SHA256 digest"));
    }

    #[test]
    fn git_pull_failure_summaries_are_stable() {
        assert_eq!(
            summarize_git_pull_failure(
                b"fatal: Need to specify how to reconcile divergent branches\n"
            ),
            GIT_PULL_DIVERGED_SUMMARY
        );
        assert!(summary_is_divergence(&summarize_git_pull_failure(
            b"fatal: Need to specify how to reconcile divergent branches\n"
        )));
        assert_eq!(
            summarize_git_pull_failure(b"hint: ignore me\nfatal: no upstream\n"),
            "git pull failed: no upstream"
        );
        assert!(!summary_is_divergence(&summarize_git_pull_failure(
            b"hint: ignore me\nfatal: no upstream\n"
        )));
    }

    #[test]
    fn update_duration_estimates_are_stable() {
        assert_eq!(
            estimate_release_update_duration(10 * 1024 * 1024, None),
            Duration::from_secs(10)
        );
        assert_eq!(
            estimate_source_update_duration(true, true, None),
            Duration::from_secs(90)
        );
    }
}
