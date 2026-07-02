//! Verification-before-done: run configured check commands after a turn
//! that edited files; failures are fed back to the model (see spec).

use crate::config::VerifyConfig;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// Outcome of running verification checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyOutcome {
    /// Whether all verification checks passed
    pub passed: bool,
    /// Human-readable report of what was checked and any failures
    pub report: String,
}

/// Resolve verification config from global + project overrides.
///
/// Starts from global config; if `working_dir` contains `.jcode/verify.toml`,
/// parses it as `ProjectVerifyFile { verify: VerifyConfig }` and replaces
/// the entire section. Parse errors log a warning and keep global config.
pub fn resolve_verify_config(working_dir: Option<&Path>) -> VerifyConfig {
    let mut config = crate::config::config().verify.clone();

    if let Some(working_dir) = working_dir {
        let project_verify_path = working_dir.join(".jcode").join("verify.toml");
        if project_verify_path.exists() {
            match std::fs::read_to_string(&project_verify_path) {
                Ok(content) => {
                    match toml::from_str::<ProjectVerifyFile>(&content) {
                        Ok(project_file) => {
                            config = project_file.verify;
                        }
                        Err(e) => {
                            crate::logging::warn(&format!(
                                "Failed to parse .jcode/verify.toml: {}",
                                e
                            ));
                        }
                    }
                }
                Err(e) => {
                    crate::logging::warn(&format!(
                        "Failed to read .jcode/verify.toml: {}",
                        e
                    ));
                }
            }
        }
    }

    config
}

/// Project-level verify.toml file structure.
#[derive(Debug, Deserialize)]
struct ProjectVerifyFile {
    verify: VerifyConfig,
}

/// Run verification checks sequentially, returning pass/fail and report.
///
/// For each command:
/// - Spawn `sh -c <cmd>` in the given cwd
/// - Capture stdout + stderr
/// - Apply per-command timeout
/// First failure (non-zero exit, timeout, or spawn error) → passed:false,
/// report truncated to last 8192 chars. All pass → passed:true, lists commands.
pub async fn run_verification(
    cfg: &VerifyConfig,
    cwd: Option<&Path>,
) -> VerifyOutcome {
    if cfg.commands.is_empty() {
        return VerifyOutcome {
            passed: true,
            report: "No verification commands configured".to_string(),
        };
    }

    let timeout_duration = Duration::from_secs(cfg.timeout_secs);
    let mut all_output = String::new();

    for cmd in &cfg.commands {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                all_output.push_str(&format!("command `{}` failed (spawn error): {}\n", cmd, e));
                return VerifyOutcome {
                    passed: false,
                    report: format_report(&all_output),
                };
            }
        };

        let result = tokio::time::timeout(timeout_duration, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !output.status.success() {
                    let exit_code = output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string());
                    all_output.push_str(&format!(
                        "command `{}` failed (exit {}):\n{}{}\n",
                        cmd, exit_code, stdout, stderr
                    ));
                    return VerifyOutcome {
                        passed: false,
                        report: format_report(&all_output),
                    };
                }

                all_output.push_str(&format!("✓ command `{}`\n", cmd));
            }
            Ok(Err(e)) => {
                all_output.push_str(&format!("command `{}` failed (wait error): {}\n", cmd, e));
                return VerifyOutcome {
                    passed: false,
                    report: format_report(&all_output),
                };
            }
            Err(_) => {
                // kill_on_drop(true) reaps the child when the timed-out
                // wait_with_output future is dropped.
                all_output.push_str(&format!("command `{}` failed (timeout after {}s)\n", cmd, cfg.timeout_secs));
                return VerifyOutcome {
                    passed: false,
                    report: format_report(&all_output),
                };
            }
        }
    }

    VerifyOutcome {
        passed: true,
        report: all_output,
    }
}

/// Decide whether to run verification on this turn.
///
/// Returns true if:
/// - enabled && has_commands && made_edits && attempts < max_attempts
pub fn should_verify(
    enabled: bool,
    has_commands: bool,
    made_edits: bool,
    attempts: u32,
    max_attempts: u32,
) -> bool {
    enabled && has_commands && made_edits && attempts < max_attempts
}

/// Truncate output to last 8192 bytes, char-boundary-safe.
fn format_report(output: &str) -> String {
    const MAX_LEN: usize = 8192;
    if output.len() <= MAX_LEN {
        output.to_string()
    } else {
        let start = output.len().saturating_sub(MAX_LEN);
        // Find the first valid char boundary at or after start so the kept
        // tail is as close to MAX_LEN as possible.
        let mut found = output.len();
        for i in start..=output.len() {
            if output.is_char_boundary(i) {
                found = i;
                break;
            }
        }
        format!("...\n{}", &output[found..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_verify_truth_table() {
        // All conditions met
        assert!(should_verify(true, true, true, 0, 2));

        // Disabled
        assert!(!should_verify(false, true, true, 0, 2));

        // No commands
        assert!(!should_verify(true, false, true, 0, 2));

        // No edits
        assert!(!should_verify(true, true, false, 0, 2));

        // Attempts exhausted
        assert!(!should_verify(true, true, true, 2, 2));

        // At max_attempts boundary
        assert!(!should_verify(true, true, true, 1, 1));

        // Just before max
        assert!(should_verify(true, true, true, 0, 1));
        assert!(should_verify(true, true, true, 1, 2));
    }

    #[tokio::test]
    async fn test_run_verification_all_pass() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec!["true".to_string(), "echo 'hello'".to_string()],
            max_attempts: 2,
            timeout_secs: 5,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(outcome.passed);
        assert!(outcome.report.contains("✓ command `true`"));
        assert!(outcome.report.contains("✓ command `echo 'hello'`"));
    }

    #[tokio::test]
    async fn test_run_verification_first_fails() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec!["false".to_string()],
            max_attempts: 2,
            timeout_secs: 5,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(!outcome.passed);
        assert!(outcome.report.contains("failed"));
    }

    #[tokio::test]
    async fn test_run_verification_second_fails() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec!["true".to_string(), "false".to_string()],
            max_attempts: 2,
            timeout_secs: 5,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(!outcome.passed);
        assert!(outcome.report.contains("✓ command `true`"));
        assert!(outcome.report.contains("failed"));
    }

    #[tokio::test]
    async fn test_run_verification_captures_output() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec!["echo 'test output'".to_string(), "false".to_string()],
            max_attempts: 2,
            timeout_secs: 5,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(!outcome.passed);
        assert!(outcome.report.contains("test output"));
    }

    #[tokio::test]
    async fn test_run_verification_timeout() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec!["sleep 10".to_string()],
            max_attempts: 2,
            timeout_secs: 1,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(!outcome.passed);
        assert!(outcome.report.contains("timeout"));
    }

    #[tokio::test]
    async fn test_run_verification_empty_commands() {
        let cfg = VerifyConfig {
            enabled: true,
            commands: vec![],
            max_attempts: 2,
            timeout_secs: 5,
        };

        let outcome = run_verification(&cfg, None).await;
        assert!(outcome.passed);
        assert!(outcome.report.contains("No verification commands"));
    }

    #[test]
    fn test_format_report_no_truncation() {
        let short = "hello world";
        assert_eq!(format_report(short), "hello world");
    }

    #[test]
    fn test_format_report_truncation() {
        // Create a string longer than 8192 bytes
        let long = "x".repeat(10000);
        let report = format_report(&long);

        // Should be much shorter than original
        assert!(report.len() < long.len());

        // Should end with the latter part of the string
        assert!(report.contains("xxx")); // at least some x's remain
        assert!(report.starts_with("...")); // has truncation indicator
        // Tail-keeping: full 8192-byte tail retained after the marker
        assert!(report.ends_with(&"x".repeat(8192)));
    }

    #[test]
    fn test_format_report_truncation_multibyte() {
        // Create output with multibyte chars to ensure boundary safety
        let mut long = String::new();
        for _ in 0..5000 {
            long.push_str("café ");
        }

        // Should not panic even with multibyte chars near boundary
        let _report = format_report(&long);
    }

    #[test]
    fn test_resolve_verify_config_no_project_file() {
        // When no project file exists, should return global config
        let cfg = resolve_verify_config(Some(Path::new("/nonexistent")));
        // Should match global defaults (enabled=false by default)
        assert!(!cfg.enabled);
    }

    #[test]
    fn test_resolve_verify_config_with_project_file() {
        use std::fs;
        use std::io::Write;

        let temp_dir = std::env::temp_dir().join(format!(
            "jcode_verify_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join(".jcode")).unwrap();

        let verify_file = temp_dir.join(".jcode").join("verify.toml");
        let mut f = fs::File::create(&verify_file).unwrap();
        writeln!(
            f,
            r#"
[verify]
enabled = true
commands = ["cargo check"]
max_attempts = 3
timeout_secs = 600
"#
        )
        .unwrap();
        drop(f);

        let cfg = resolve_verify_config(Some(&temp_dir));
        assert!(cfg.enabled);
        assert_eq!(cfg.commands.len(), 1);
        assert_eq!(cfg.commands[0], "cargo check");
        assert_eq!(cfg.max_attempts, 3);
        assert_eq!(cfg.timeout_secs, 600);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_resolve_verify_config_bad_toml() {
        use std::fs;
        use std::io::Write;

        let temp_dir = std::env::temp_dir().join(format!(
            "jcode_verify_bad_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join(".jcode")).unwrap();

        let verify_file = temp_dir.join(".jcode").join("verify.toml");
        let mut f = fs::File::create(&verify_file).unwrap();
        writeln!(f, "this is not valid toml [[[").unwrap();
        drop(f);

        // Should log warning and return global config
        let cfg = resolve_verify_config(Some(&temp_dir));
        assert!(!cfg.enabled); // falls back to global default

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
