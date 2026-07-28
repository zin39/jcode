//! jcode's canonical location for credentials.
//!
//! Credentials used to be split across two directories along the wrong axis:
//! `*.env` API keys lived in [`app_config_dir`] while OAuth token JSON lived in
//! [`jcode_dir`]. "Where are my credentials?" therefore had two answers
//! depending on how you logged in.
//!
//! The invariant is now one axis:
//! - **secrets** (API keys, OAuth tokens) live in [`app_config_dir`]
//! - **everything else** (`config.toml`, logs, builds) stays in [`jcode_dir`]

use crate::{app_config_dir, harden_secret_file_permissions, jcode_dir};
use anyhow::Result;
use std::path::PathBuf;

/// Resolve a secret-bearing file to jcode's canonical secrets directory.
///
/// `app_config_dir()` is canonical for secrets because the env loaders already
/// read *only* from there; moving env keys back to `jcode_dir()` would revert a
/// deliberate fix, whereas moving OAuth files forward completes it.
///
/// Use [`resolve_secret_path`] rather than this when reading, so an existing
/// credential in the legacy location is still found.
pub fn secret_path(file_name: &str) -> Result<PathBuf> {
    Ok(app_config_dir()?.join(file_name))
}

/// The legacy (pre-consolidation) location for a secret file.
///
/// Retained so existing installs keep working with no user action, and so a
/// downgrade to an older binary still finds its credentials.
pub fn legacy_secret_path(file_name: &str) -> Result<PathBuf> {
    Ok(jcode_dir()?.join(file_name))
}

/// Resolve where to *read* a secret file from, migrating it forward on the way.
///
/// Resolution order:
/// 1. canonical path, if it exists
/// 2. legacy path, copied forward to the canonical path and then read there
/// 3. canonical path, for a caller that is about to create the file
///
/// The legacy file is deliberately **copied, not moved**. Deleting it would
/// mean a rollback to an older binary (which reads only `jcode_dir()`) reports
/// the user's credentials as missing. Losing access to a live credential is a
/// far worse failure than one stale duplicate, so the legacy copy is left in
/// place for a later release to remove.
///
/// Migration is best-effort: if the copy fails, the legacy path is returned so
/// the credential remains usable.
pub fn resolve_secret_path(file_name: &str) -> Result<PathBuf> {
    let canonical = secret_path(file_name)?;
    if canonical.exists() {
        return Ok(canonical);
    }

    let Ok(legacy) = legacy_secret_path(file_name) else {
        return Ok(canonical);
    };
    if !legacy.is_file() {
        return Ok(canonical);
    }

    if let Some(parent) = canonical.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return Ok(legacy);
    }

    match std::fs::copy(&legacy, &canonical) {
        Ok(_) => {
            harden_secret_file_permissions(&canonical);
            Ok(canonical)
        }
        // Keep the credential reachable rather than failing the read.
        Err(_) => Ok(legacy),
    }
}

#[cfg(test)]
mod secret_path_tests {
    use super::{legacy_secret_path, resolve_secret_path, secret_path};
    use crate::scoped_test_home;

    /// R2: an existing install keeps working with no user action. A credential
    /// that only exists in the legacy location must still be reachable.
    #[test]
    fn reads_credential_from_legacy_path() {
        let sandbox = tempfile::tempdir().expect("tempdir");
        let _home = scoped_test_home(sandbox.path());

        let legacy = legacy_secret_path("auth.json").unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{\"token\":\"legacy\"}").unwrap();

        let resolved = resolve_secret_path("auth.json").unwrap();
        assert_eq!(
            std::fs::read_to_string(&resolved).unwrap(),
            "{\"token\":\"legacy\"}",
            "a credential only present in the legacy location must still be readable"
        );
    }

    /// R3: downgrade safety. Migration copies forward but must never delete the
    /// legacy file, or rolling back to an older binary (which reads only
    /// `jcode_dir()`) would report the user's credentials as missing.
    #[test]
    fn migration_keeps_legacy_copy() {
        let sandbox = tempfile::tempdir().expect("tempdir");
        let _home = scoped_test_home(sandbox.path());

        let legacy = legacy_secret_path("openai-auth.json").unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{\"token\":\"t\"}").unwrap();

        let resolved = resolve_secret_path("openai-auth.json").unwrap();

        assert_eq!(
            resolved,
            secret_path("openai-auth.json").unwrap(),
            "resolution should migrate the credential forward to the canonical dir"
        );
        assert!(
            legacy.exists(),
            "legacy credential must be KEPT, not moved: deleting it breaks downgrades"
        );
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "{\"token\":\"t\"}",
            "legacy copy must be left byte-identical"
        );
    }

    /// R4: when both locations hold a credential, the canonical one wins and
    /// neither file is destroyed.
    #[test]
    fn both_present_prefers_canonical_and_deletes_nothing() {
        let sandbox = tempfile::tempdir().expect("tempdir");
        let _home = scoped_test_home(sandbox.path());

        let legacy = legacy_secret_path("auth.json").unwrap();
        let canonical = secret_path("auth.json").unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "stale").unwrap();
        std::fs::write(&canonical, "fresh").unwrap();

        let resolved = resolve_secret_path("auth.json").unwrap();

        assert_eq!(resolved, canonical);
        assert_eq!(
            std::fs::read_to_string(&resolved).unwrap(),
            "fresh",
            "canonical credential must not be clobbered by a stale legacy file"
        );
        assert!(legacy.exists(), "legacy file must not be deleted");
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "stale");
    }

    /// A caller creating a brand new credential gets the canonical path, so
    /// writes never land back in the legacy location.
    #[test]
    fn new_credentials_resolve_to_canonical_dir() {
        let sandbox = tempfile::tempdir().expect("tempdir");
        let _home = scoped_test_home(sandbox.path());

        assert_eq!(
            resolve_secret_path("gemini_oauth.json").unwrap(),
            secret_path("gemini_oauth.json").unwrap(),
            "with no existing file, callers must be pointed at the canonical dir"
        );
    }

    /// R5: consolidating a credential must not widen access to it.
    #[cfg(unix)]
    #[test]
    fn migrated_credential_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::tempdir().expect("tempdir");
        let _home = scoped_test_home(sandbox.path());

        let legacy = legacy_secret_path("antigravity_oauth.json").unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "{}").unwrap();
        // Deliberately world-readable, as a legacy file may well be.
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644)).unwrap();

        let resolved = resolve_secret_path("antigravity_oauth.json").unwrap();
        let mode = std::fs::metadata(&resolved).unwrap().permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o600,
            "a migrated credential must be owner-only, not inherit permissive legacy modes"
        );
    }
}
