// Catalog-fetch scheduling for `[providers.<name>]` profiles defined only in
// config.toml.
//
// Split out of catalog_routes.rs, which is already over the oversized-file
// budget.

/// Serializes the tests below and restores a benign hook when each finishes.
///
/// The catalog-refresh hook is a process-global slot with no deregistration,
/// so a test that installs one leaks it into every later test in the process.
/// That is not theoretical: a hook installed here that panicked on an
/// unexpected call took down `test_on_auth_changed_hot_initializes_openrouter`,
/// which builds routes for its own reasons and legitimately triggers a fetch.
///
/// Holding the lock for the whole test keeps concurrent registrations from
/// clobbering each other, and the drop replaces whatever the test installed
/// with an inert no-op.
struct RefreshHookGuard(std::sync::MutexGuard<'static, ()>);

impl Drop for RefreshHookGuard {
    fn drop(&mut self) {
        crate::provider::external::register_profile_catalog_refresh(|_, _| false);
    }
}

fn lock_refresh_hook() -> RefreshHookGuard {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    RefreshHookGuard(
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// The identifying fields a scheduled catalog refresh must carry.
#[derive(Debug, PartialEq, Eq)]
struct Scheduled {
    id: String,
    api_base: String,
    api_key_env: String,
    env_file: String,
}

/// A `[providers.<name>]` profile with no static `models` must ask the
/// runtime to fetch its catalog while building routes.
///
/// Regression: this branch was empty, with a comment saying the cache
/// "will be populated when the provider is actually used". For a profile
/// declaring no models that is circular, since the only way to use it is to
/// pick a model that the picker cannot list. Measured on a VM whose
/// `[providers.dashscope]` had no `models`: no `dashscope_models.json` was
/// ever written and `/model` never offered a qwen model, while a sibling
/// `[providers.deepseek]` with two static models listed fine.
#[test]
fn config_only_profile_without_models_schedules_a_catalog_fetch() {
    let _hook_lock = lock_refresh_hook();
    use std::sync::{Arc, Mutex};

    let seen: Arc<Mutex<Vec<Scheduled>>> = Arc::new(Mutex::new(vec![]));
    let sink = Arc::clone(&seen);
    crate::provider::external::register_profile_catalog_refresh(
        move |resolved, _context| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Scheduled {
                    id: resolved.id.clone(),
                    api_base: resolved.api_base.clone(),
                    api_key_env: resolved.api_key_env.clone(),
                    env_file: resolved.env_file.clone(),
                });
            true
        },
    );

    let profile = crate::config::NamedProviderConfig {
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
        env_file: Some("dashscope.env".to_string()),
        api_key_env: Some("DASHSCOPE_API_KEY".to_string()),
        ..Default::default()
    };
    assert!(
        profile.models.is_empty(),
        "the regression only applies to a profile with no static models"
    );

    let scheduled = crate::provider::named_profile_catalog::schedule_refresh("dashscope", &profile);
    assert!(
        scheduled,
        "a refresh must be scheduled, not silently skipped"
    );

    let calls = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // The id doubles as the disk-cache namespace that
    // `named_provider_profile_routes` reads back from, so a mismatch here
    // would write a cache nothing ever loads.
    assert_eq!(
        calls.as_slice(),
        [Scheduled {
            id: "dashscope".to_string(),
            api_base: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            api_key_env: "DASHSCOPE_API_KEY".to_string(),
            env_file: "dashscope.env".to_string(),
        }],
        "exactly one refresh, carrying the profile's own cache namespace and credentials"
    );
}

/// The credential names default to the same conventions the runtime uses,
/// so a profile that can authenticate a chat request can also authenticate
/// its own catalog fetch without repeating them in config.
#[test]
fn config_only_profile_defaults_its_credential_names_from_the_profile_id() {
    let _hook_lock = lock_refresh_hook();
    use std::sync::{Arc, Mutex};

    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![]));
    let sink = Arc::clone(&seen);
    crate::provider::external::register_profile_catalog_refresh(
        move |resolved, _context| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((resolved.api_key_env.clone(), resolved.env_file.clone()));
            true
        },
    );

    let profile = crate::config::NamedProviderConfig {
        base_url: "https://example.test/v1".to_string(),
        ..Default::default()
    };
    assert!(crate::provider::named_profile_catalog::schedule_refresh(
        "myprofile",
        &profile
    ));

    let calls = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        calls[0],
        ("MYPROFILE_API_KEY".to_string(), "myprofile.env".to_string())
    );
}

/// Building routes for a config-only profile must itself trigger the
/// fetch. This is the assertion that actually pins the bug: the previous
/// code had the helper's job written as an empty `if` block, so testing
/// the helper alone still passed while the picker stayed empty forever.
#[test]
fn building_routes_for_a_config_only_profile_triggers_the_fetch() {
    use std::sync::{Arc, Mutex};
    let _hook_lock = lock_refresh_hook();

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));

    with_clean_provider_test_env(|| {
        // Register inside the scope: `with_clean_provider_test_env` installs
        // the shared runtime stubs, which would otherwise overwrite this
        // recorder and make the assertion below vacuous.
        let sink = Arc::clone(&seen);
        crate::provider::external::register_profile_catalog_refresh(move |resolved, _context| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(resolved.id.clone());
            true
        });

        let profile = crate::config::NamedProviderConfig {
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            env_file: Some("dashscope.env".to_string()),
            api_key_env: Some("DASHSCOPE_API_KEY".to_string()),
            ..Default::default()
        };

        // The scoped temp home has no cache file for this profile, so the route
        // build takes the cache-miss path rather than reading whatever catalog
        // this machine happens to have on disk.
        let routes =
            crate::provider::catalog_routes::named_provider_profile_routes("dashscope", &profile);
        assert!(
            routes.is_empty(),
            "a profile with no models, no default and no cache lists nothing yet"
        );

        let calls = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            calls.as_slice(),
            ["dashscope"],
            "building routes on a cache miss must schedule the catalog fetch, \
             otherwise the profile can never populate its cache and the picker \
             stays empty forever"
        );
    });
}

/// A profile with no base URL has nothing to fetch from, so it must not
/// occupy the refresh tracker's in-flight slot or its retry backoff.
#[test]
fn profile_without_a_base_url_schedules_nothing() {
    let _hook_lock = lock_refresh_hook();
    // Record rather than panic: this hook is a process-global slot, so a panic
    // here would fire inside whatever unrelated test runs next.
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&called);
    crate::provider::external::register_profile_catalog_refresh(move |_, _| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    });

    let profile = crate::config::NamedProviderConfig {
        base_url: "   ".to_string(),
        ..Default::default()
    };
        assert!(
        !crate::provider::named_profile_catalog::schedule_refresh(
            "nobase", &profile
        )
    );
    assert!(
        !called.load(std::sync::atomic::Ordering::SeqCst),
        "a profile with no base URL must not reach the fetch scheduler at all"
    );
}
