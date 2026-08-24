//! OS-level sandbox for shell commands (macOS Seatbelt).
//!
//! Wraps a `bash -c` invocation in `sandbox-exec` with a deny-by-default
//! file-write profile: the command can read anywhere and use the network, but
//! can only write inside the declared writable roots (working directory, temp
//! dirs, and user-configured extras). This is the enforcement layer behind the
//! advisory `jcode-command-risk` classifier: risk classification asks, the
//! sandbox makes violations fail with EPERM at the OS.
//!
//! Design notes:
//! - Reads and network stay open on purpose. Builds resolve toolchains from
//!   `/usr`, `~/.cargo`, etc., and fetch dependencies; a read/network-closed
//!   profile breaks nearly every real workflow and would get turned off.
//!   Write confinement alone stops the damaging failure mode (an agent
//!   scribbling outside its workspace).
//! - `sandbox-exec` is deprecated in name but is the same Seatbelt mechanism
//!   Apple ships and Codex CLI uses in production; profiles are stable.
//! - Enforcement is reported, never assumed: [`enforcement_level`] tells the
//!   caller whether a command would actually be confined, so "sandbox
//!   configured" on Linux/Windows is surfaced as unenforced rather than
//!   silently pretending.

use std::path::{Path, PathBuf};

/// How a sandbox request will actually be honored on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxEnforcement {
    /// Sandbox off by configuration.
    Disabled,
    /// Requested and enforced via macOS Seatbelt.
    EnforcedSeatbelt,
    /// Requested but this platform has no implementation; runs unconfined.
    Unenforced,
}

impl SandboxEnforcement {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::EnforcedSeatbelt => "enforced (macOS Seatbelt)",
            Self::Unenforced => "UNENFORCED (no sandbox on this platform)",
        }
    }
}

/// Resolve how the configured sandbox mode will be enforced on this host.
pub fn enforcement_level(cfg: &crate::config::SandboxConfig) -> SandboxEnforcement {
    if !cfg.workspace_write_enabled() {
        return SandboxEnforcement::Disabled;
    }
    if cfg!(target_os = "macos") && sandbox_exec_available() {
        SandboxEnforcement::EnforcedSeatbelt
    } else {
        SandboxEnforcement::Unenforced
    }
}

fn sandbox_exec_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
}

/// The set of directories a workspace-write sandboxed command may write to.
///
/// Always includes the working directory and temp/scratch space. Roots are
/// canonicalized because Seatbelt matches on real paths: on macOS `/tmp` and
/// `/var` are symlinks into `/private`, and a non-canonical subpath rule
/// silently never matches.
pub fn writable_roots(
    working_dir: Option<&Path>,
    extra: &[String],
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        let canonical = path.canonicalize().unwrap_or(path);
        if canonical.as_os_str().is_empty() || canonical == Path::new("/") {
            return;
        }
        if !roots.iter().any(|existing| existing == &canonical) {
            roots.push(canonical);
        }
    };

    if let Some(dir) = working_dir {
        push(dir.to_path_buf());
    }
    push(std::env::temp_dir());
    if let Some(scratch) = std::env::var_os("JCODE_SCRATCH_DIR") {
        push(PathBuf::from(scratch));
    }
    // Dev null and shells' fd tricks live here; Seatbelt profiles allow
    // /dev/null explicitly below, but per-user darwin temp dirs are separate
    // from temp_dir() when TMPDIR is unset in the child.
    if let Some(darwin_tmp) = std::env::var_os("TMPDIR") {
        push(PathBuf::from(darwin_tmp));
    }
    for root in extra {
        let expanded = expand_home(root);
        if !expanded.as_os_str().is_empty() {
            push(expanded);
        }
    }
    roots
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Build the Seatbelt profile string for a workspace-write sandbox.
///
/// Deny-by-default on file writes with subpath allowances; everything else
/// (reads, network, process exec, signals) is allowed. `(allow default)` plus
/// a targeted `(deny file-write*)` would be simpler but Seatbelt gives deny
/// precedence only within equal specificity, so the explicit structure below
/// mirrors the profiles Codex ships and Apple's own templates.
pub fn seatbelt_profile(writable_roots: &[PathBuf]) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*)\n\
         (allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\"))\n\
         (allow file-write-data (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/tty\"))\n\
         (allow file-write* (regex #\"^/private/var/folders/[^/]+/[^/]+/T(/|$)\"))\n",
    );
    for root in writable_roots {
        let escaped = root.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
        profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
    }
    profile
}

/// Wrap `bash -c <cmd>` arguments into a `sandbox-exec` invocation.
///
/// Returns the program and its arguments; the caller owns building the actual
/// process (stdio, cwd, env). Returns `None` when the sandbox is not enforced
/// on this host so callers fall back to plain bash explicitly.
pub fn wrap_command(
    cfg: &crate::config::SandboxConfig,
    working_dir: Option<&Path>,
    shell_command: &str,
) -> Option<(String, Vec<String>)> {
    if enforcement_level(cfg) != SandboxEnforcement::EnforcedSeatbelt {
        return None;
    }
    let roots = writable_roots(working_dir, &cfg.extra_writable_roots);
    let profile = seatbelt_profile(&roots);
    Some((
        "/usr/bin/sandbox-exec".to_string(),
        vec![
            "-p".to_string(),
            profile,
            "bash".to_string(),
            "-c".to_string(),
            shell_command.to_string(),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxConfig;

    fn cfg(mode: &str, extra: &[&str]) -> SandboxConfig {
        SandboxConfig {
            mode: mode.to_string(),
            extra_writable_roots: extra.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn disabled_by_default_and_off_mode() {
        assert_eq!(
            enforcement_level(&SandboxConfig::default()),
            SandboxEnforcement::Disabled
        );
        assert_eq!(
            enforcement_level(&cfg("off", &[])),
            SandboxEnforcement::Disabled
        );
        assert!(wrap_command(&cfg("off", &[]), None, "echo hi").is_none());
    }

    #[test]
    fn profile_contains_deny_default_and_each_root() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let roots = writable_roots(Some(temp.path()), &[]);
        let profile = seatbelt_profile(&roots);
        assert!(profile.contains("(deny file-write*)"));
        let canonical = temp.path().canonicalize().expect("canonicalize");
        assert!(
            profile.contains(&format!("(subpath \"{}\")", canonical.display())),
            "profile must allow the working dir, got:\n{profile}"
        );
    }

    #[test]
    fn writable_roots_canonicalize_and_dedupe() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let raw = temp.path().to_string_lossy().to_string();
        // Same dir passed as working dir AND extra root: one entry.
        let roots = writable_roots(Some(temp.path()), &[raw]);
        let canonical = temp.path().canonicalize().expect("canonicalize");
        assert_eq!(
            roots.iter().filter(|r| **r == canonical).count(),
            1,
            "duplicate roots must collapse: {roots:?}"
        );
    }

    #[test]
    fn profile_escapes_quotes_in_paths() {
        let tricky = PathBuf::from("/tmp/has\"quote");
        let profile = seatbelt_profile(&[tricky]);
        assert!(profile.contains("has\\\"quote"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn workspace_write_enforced_on_macos() {
        assert_eq!(
            enforcement_level(&cfg("workspace-write", &[])),
            SandboxEnforcement::EnforcedSeatbelt
        );
        let (program, args) =
            wrap_command(&cfg("workspace-write", &[]), None, "echo hi").expect("wrapped");
        assert_eq!(program, "/usr/bin/sandbox-exec");
        assert_eq!(args[0], "-p");
        assert_eq!(&args[2..], ["bash", "-c", "echo hi"]);
    }

    /// Real end-to-end enforcement check: inside the sandbox, writing to the
    /// allowed root succeeds and writing outside it fails. This is the actual
    /// acceptance behavior, not an inspection of the profile string.
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_blocks_writes_outside_roots_and_allows_inside() {
        let allowed = tempfile::TempDir::new().expect("allowed dir");
        let forbidden = tempfile::TempDir::new().expect("forbidden dir");
        let inside = allowed.path().join("ok.txt");
        let outside = forbidden.path().join("blocked.txt");

        // Only `allowed` is a writable root. `forbidden` is a sibling temp dir
        // NOT passed as a root -- but darwin per-user temp is allowed by the
        // profile's T-folder regex, so route the forbidden file through a path
        // outside temp entirely to make the test honest.
        // Fixed path outside temp and the allowed root; not derived from HOME
        // because other tests in this crate mutate HOME/JCODE_HOME.
        let outside_home = PathBuf::from(format!(
            "/Users/Shared/jcode-sandbox-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&outside_home);

        let profile = seatbelt_profile(&writable_roots(Some(allowed.path()), &[]));
        let script = format!(
            "echo ok > {inside:?} && echo denied > {outside_home:?}; echo exit=$?",
        );
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "bash", "-c", &script])
            .output()
            .expect("run sandbox-exec");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            std::fs::read_to_string(&inside).is_ok(),
            "write inside allowed root must succeed; stdout: {stdout}"
        );
        assert!(
            std::fs::metadata(&outside_home).is_err(),
            "write outside roots must be blocked; stdout: {stdout}"
        );
        let _ = std::fs::remove_file(&outside_home);
        drop(outside); // silence unused when the regex path is used
    }
}
