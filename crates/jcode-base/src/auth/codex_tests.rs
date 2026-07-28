use super::*;
use std::ffi::OsString;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, previous }
    }

    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, previous }
    }

    /// Clear an ambient credential for the duration of a test.
    ///
    /// `JCODE_HOME` sandboxes files, but not the environment. A developer with
    /// a real `OPENAI_API_KEY` exported would otherwise have their live key
    /// picked up by the API-key fallback in `load_credentials`, so tests
    /// asserting on OAuth tokens failed locally while passing in CI.
    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        crate::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            crate::env::set_var(self.key, previous);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

#[test]
fn auth_file_with_oauth_tokens() {
    let json = r#"{
        "tokens": {
            "access_token": "at_openai_123",
            "refresh_token": "rt_openai_456",
            "id_token": "header.payload.signature",
            "account_id": "acct_789",
            "expires_at": 9999999999999
        }
    }"#;
    let file: LegacyAuthFile = serde_json::from_str(json).unwrap();
    let tokens = file.tokens.unwrap();
    assert_eq!(tokens.access_token, "at_openai_123");
    assert_eq!(tokens.refresh_token, "rt_openai_456");
    assert_eq!(
        tokens.id_token,
        Some("header.payload.signature".to_string())
    );
    assert_eq!(tokens.account_id, Some("acct_789".to_string()));
    assert_eq!(tokens.expires_at, Some(9999999999999));
}

#[test]
fn auth_file_with_api_key_only() {
    let json = r#"{
        "OPENAI_API_KEY": "sk-test-key-123"
    }"#;
    let file: LegacyAuthFile = serde_json::from_str(json).unwrap();
    assert!(file.tokens.is_none());
    assert_eq!(file.api_key, Some("sk-test-key-123".to_string()));
}

#[test]
fn auth_file_minimal_tokens() {
    let json = r#"{
        "tokens": {
            "access_token": "at",
            "refresh_token": "rt"
        }
    }"#;
    let file: LegacyAuthFile = serde_json::from_str(json).unwrap();
    let tokens = file.tokens.unwrap();
    assert_eq!(tokens.access_token, "at");
    assert!(tokens.id_token.is_none());
    assert!(tokens.account_id.is_none());
    assert!(tokens.expires_at.is_none());
}

#[test]
fn decode_jwt_payload_valid() {
    let payload = serde_json::json!({
        "sub": "user123",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_abc"
        }
    });
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let token = format!("header.{}.signature", payload_b64);

    let decoded = decode_jwt_payload(&token).unwrap();
    assert_eq!(decoded["sub"], "user123");
}

fn jwt_with_exp(exp_secs: i64) -> String {
    let payload = serde_json::json!({ "exp": exp_secs });
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    format!("header.{}.signature", payload_b64)
}

#[test]
fn expires_at_from_access_token_reads_exp_claim_as_millis() {
    // The Codex `auth.json` stores no `expires_at`; we recover it from the
    // access-token JWT `exp` (seconds) and report it in millis.
    let token = jwt_with_exp(1_780_000_000);
    assert_eq!(
        expires_at_from_access_token(&token),
        Some(1_780_000_000_000)
    );
}

#[test]
fn expires_at_from_access_token_handles_missing_exp_and_garbage() {
    let no_exp = {
        let payload = serde_json::json!({ "sub": "u" });
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        format!("header.{}.signature", payload_b64)
    };
    assert_eq!(expires_at_from_access_token(&no_exp), None);
    assert_eq!(expires_at_from_access_token("not-a-jwt"), None);
    assert_eq!(expires_at_from_access_token(""), None);
}

#[test]
fn legacy_tokens_without_expiry_recover_expires_at_from_jwt() {
    // Reproduces the onboarding bug: the imported Codex token carries no
    // explicit `expires_at`, so we must derive it from the JWT so jcode can
    // refresh proactively instead of sending a dead token and forcing /login.
    let tokens = LegacyTokens {
        access_token: jwt_with_exp(1_780_000_000),
        refresh_token: "rt_example".to_string(),
        id_token: None,
        account_id: Some("acct".to_string()),
        expires_at: None,
    };
    let creds = credentials_from_legacy_tokens(&tokens);
    assert_eq!(creds.expires_at, Some(1_780_000_000_000));
}

#[test]
fn explicit_expires_at_takes_precedence_over_jwt() {
    let tokens = LegacyTokens {
        access_token: jwt_with_exp(1_780_000_000),
        refresh_token: "rt_example".to_string(),
        id_token: None,
        account_id: None,
        expires_at: Some(9_999_999_999_000),
    };
    let creds = credentials_from_legacy_tokens(&tokens);
    assert_eq!(creds.expires_at, Some(9_999_999_999_000));
}

#[test]
fn extract_account_id_from_jwt() {
    let payload = serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_test_123"
        }
    });
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let token = format!("header.{}.signature", payload_b64);

    assert_eq!(
        extract_account_id(&token),
        Some("acct_test_123".to_string())
    );
}

#[test]
fn extract_email_from_jwt() {
    let payload = serde_json::json!({
        "email": "user@example.com"
    });
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let token = format!("header.{}.signature", payload_b64);

    assert_eq!(extract_email(&token), Some("user@example.com".to_string()));
}

#[test]
fn load_credentials_falls_back_to_env_api_key() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("OPENAI_API_KEY", "sk-env-test");
    set_active_account_override(None);

    let creds = load_credentials().unwrap();
    assert_eq!(creds.access_token, "sk-env-test");
    assert!(creds.refresh_token.is_empty());
    assert!(creds.id_token.is_none());
    assert!(creds.expires_at.is_none());
}

#[test]
fn multi_account_active_switch_works() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    // A real exported key would otherwise satisfy load_credentials first.
    let _no_api_key = EnvVarGuard::unset("OPENAI_API_KEY");
    set_active_account_override(None);

    upsert_account(OpenAiAccount {
        label: "personal".to_string(),
        access_token: "at_personal".to_string(),
        refresh_token: "rt_personal".to_string(),
        id_token: None,
        account_id: Some("acct_personal".to_string()),
        expires_at: Some(10),
        email: Some("personal@example.com".to_string()),
    })
    .unwrap();
    upsert_account(OpenAiAccount {
        label: "work".to_string(),
        access_token: "at_work".to_string(),
        refresh_token: "rt_work".to_string(),
        id_token: None,
        account_id: Some("acct_work".to_string()),
        expires_at: Some(20),
        email: Some("work@example.com".to_string()),
    })
    .unwrap();

    assert_eq!(active_account_label().as_deref(), Some("openai-1"));
    set_active_account("openai-2").unwrap();
    assert_eq!(active_account_label().as_deref(), Some("openai-2"));

    let creds = load_credentials().unwrap();
    assert_eq!(creds.access_token, "at_work");
    assert_eq!(creds.account_id.as_deref(), Some("acct_work"));
}

#[test]
fn load_auth_file_migrates_legacy_codex_tokens() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    set_active_account_override(None);

    let legacy_path = temp
        .path()
        .join("external")
        .join(".codex")
        .join("auth.json");
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy_path,
        r#"{
            "tokens": {
                "access_token": "at_legacy",
                "refresh_token": "rt_legacy",
                "account_id": "acct_legacy",
                "expires_at": 1234
            }
        }"#,
    )
    .unwrap();

    let auth = load_auth_file().unwrap();
    assert!(auth.openai_accounts.is_empty());
    assert!(auth.active_openai_account.is_none());
    assert!(
        legacy_path.exists(),
        "expected legacy Codex auth file to remain untouched"
    );
}

#[test]
fn load_credentials_ignores_legacy_oauth_without_consent() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    // This asserts load_credentials finds NOTHING, so a real exported key
    // would mask the very failure the test is checking for.
    let _no_api_key = EnvVarGuard::unset("OPENAI_API_KEY");
    set_active_account_override(None);

    let legacy_path = temp
        .path()
        .join("external")
        .join(".codex")
        .join("auth.json");
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy_path,
        r#"{
            "tokens": {
                "access_token": "at_legacy",
                "refresh_token": "rt_legacy",
                "account_id": "acct_legacy",
                "expires_at": 1234
            },
            "OPENAI_API_KEY": "sk-legacy"
        }"#,
    )
    .unwrap();

    let err = load_credentials().unwrap_err();
    assert!(
        err.to_string()
            .contains("No OpenAI tokens or API key found"),
        "unexpected error: {err:#}"
    );

    let legacy: LegacyAuthFile =
        serde_json::from_str(&std::fs::read_to_string(&legacy_path).unwrap()).unwrap();
    assert!(legacy.tokens.is_some());
    assert_eq!(legacy.api_key.as_deref(), Some("sk-legacy"));
}

#[test]
fn load_credentials_reads_legacy_oauth_when_allowed() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let _allow = EnvVarGuard::set(ALLOW_LEGACY_AUTH_ENV, "1");
    set_active_account_override(None);

    let legacy_path = temp
        .path()
        .join("external")
        .join(".codex")
        .join("auth.json");
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy_path,
        r#"{
            "tokens": {
                "access_token": "at_legacy",
                "refresh_token": "rt_legacy",
                "account_id": "acct_legacy",
                "expires_at": 9999999999999
            }
        }"#,
    )
    .unwrap();

    let creds = load_credentials().unwrap();
    assert_eq!(creds.access_token, "at_legacy");
    assert_eq!(creds.refresh_token, "rt_legacy");
    assert!(
        legacy_path.exists(),
        "legacy auth file should remain in place"
    );
}

#[cfg(unix)]
#[test]
fn load_credentials_reads_legacy_oauth_without_changing_external_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let _allow = EnvVarGuard::set(ALLOW_LEGACY_AUTH_ENV, "1");
    set_active_account_override(None);

    let legacy_path = temp
        .path()
        .join("external")
        .join(".codex")
        .join("auth.json");
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy_path,
        r#"{
            "tokens": {
                "access_token": "at_legacy",
                "refresh_token": "rt_legacy",
                "account_id": "acct_legacy",
                "expires_at": 4102444800000
            }
        }"#,
    )
    .unwrap();
    std::fs::set_permissions(
        legacy_path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(&legacy_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let creds = load_credentials().expect("load legacy oauth");
    assert_eq!(creds.access_token, "at_legacy");

    let dir_mode = std::fs::metadata(legacy_path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let file_mode = std::fs::metadata(&legacy_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o755);
    assert_eq!(file_mode, 0o644);
}

#[test]
fn load_auth_file_renames_existing_labels_to_numbered_scheme() {
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    set_active_account_override(None);

    let auth_path = temp.path().join("openai-auth.json");
    std::fs::write(
        &auth_path,
        r#"{
            "openai_accounts": [
                {
                    "label": "personal",
                    "access_token": "at_personal",
                    "refresh_token": "rt_personal"
                },
                {
                    "label": "work",
                    "access_token": "at_work",
                    "refresh_token": "rt_work"
                }
            ],
            "active_openai_account": "work"
        }"#,
    )
    .unwrap();

    let auth = load_auth_file().unwrap();
    assert_eq!(
        auth.openai_accounts
            .iter()
            .map(|account| account.label.as_str())
            .collect::<Vec<_>>(),
        vec!["openai-1", "openai-2"]
    );
    assert_eq!(auth.active_openai_account.as_deref(), Some("openai-2"));
}

#[test]
fn api_key_credentials_work_with_oauth_accounts_present() {
    // load_api_key_credentials() must succeed even when OAuth accounts
    // exist in openai-auth.json — the ApiKey credential mode bypasses
    // OAuth entirely. This is the resolution path used when
    // CredentialMode::ApiKey is pinned.
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("OPENAI_API_KEY", "sk-from-env");
    set_active_account_override(None);

    // Write an OAuth account to openai-auth.json (still valid, not expired).
    // This is the exact scenario that triggers the bug: a valid OAuth token
    // sits in openai-auth.json and load_credentials() picks it even though
    // the user just saved an API key.
    upsert_account(OpenAiAccount {
        label: "openai-1".to_string(),
        access_token: "at_oauth_valid".to_string(),
        refresh_token: "rt_oauth_valid".to_string(),
        id_token: None,
        account_id: Some("acct_oauth".to_string()),
        expires_at: Some(9999999999999), // far future, not expired
        email: Some("oauth@example.com".to_string()),
    })
    .unwrap();

    // Auto mode prefers the non-expired OAuth (this is the bug:
    // load_credentials() picks OAuth first even though the user
    // explicitly saved an API key).
    let auto_creds = load_credentials().unwrap();
    assert_eq!(auto_creds.access_token, "at_oauth_valid");

    // But ApiKey mode bypasses OAuth entirely and picks the env key.
    let api_key_creds = load_api_key_credentials().unwrap();
    assert_eq!(api_key_creds.access_token, "sk-from-env");
    assert!(api_key_creds.refresh_token.is_empty());
}

#[test]
fn oauth_credentials_still_work_when_oauth_accounts_exist() {
    // load_oauth_credentials() must still resolve correctly when
    // OAuth accounts are present — the OAuth credential mode must
    // not be broken by the API-key-pinning fix.
    let _lock = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set_path("JCODE_HOME", temp.path());
    set_active_account_override(None);

    upsert_account(OpenAiAccount {
        label: "openai-1".to_string(),
        access_token: "at_oauth_valid".to_string(),
        refresh_token: "rt_oauth_valid".to_string(),
        id_token: None,
        account_id: Some("acct_oauth".to_string()),
        expires_at: Some(9999999999999), // far future
        email: Some("oauth@example.com".to_string()),
    })
    .unwrap();

    let creds = load_oauth_credentials().unwrap();
    assert_eq!(creds.access_token, "at_oauth_valid");
    assert_eq!(creds.refresh_token, "rt_oauth_valid");
}
