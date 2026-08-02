//! Catalog fetching for provider profiles that exist only in `config.toml`.
//!
//! A `[providers.<name>]` block is read at runtime, so it has no compile-time
//! `OpenAiCompatibleProfile` catalog entry and cannot use the refresh hook
//! keyed by one. Without a way to fetch, a profile declaring no static `models`
//! could never list anything: the picker needs the disk cache, the cache was
//! only written after a request, and the request could only come from picking a
//! model out of the list.

/// Ask the runtime to fetch `GET <base_url>/models` for a config-defined
/// profile, so its models can appear in the picker without the user having
/// first sent a request through it.
///
/// Returns whether a refresh was actually started. It is not started when the
/// profile lacks a usable base URL, when no credential can be found (an
/// unauthenticated fetch would just 401), when one is already in flight, or
/// when the runtime hook is unregistered, which is the case in minimal test
/// binaries.
pub(super) fn schedule_refresh(
    profile_name: &str,
    profile_config: &crate::config::NamedProviderConfig,
) -> bool {
    let api_base = profile_config.base_url.trim();
    if api_base.is_empty() {
        return false;
    }

    // Mirror how the runtime locates this profile's key, so a profile that can
    // authenticate a chat request can also authenticate its catalog fetch. The
    // env var defaults to the conventional <PROFILE>_API_KEY, matching the
    // `api_key_env` a user would otherwise spell out.
    let api_key_env = profile_config
        .api_key_env
        .clone()
        .unwrap_or_else(|| format!("{}_API_KEY", profile_name.to_ascii_uppercase()));
    let env_file = profile_config
        .env_file
        .clone()
        .unwrap_or_else(|| format!("{profile_name}.env"));

    let resolved = crate::provider_catalog::ResolvedOpenAiCompatibleProfile {
        // The id doubles as the disk-cache namespace, so it must be the config
        // key: that is where `named_provider_profile_routes` reads it back from.
        id: profile_name.to_string(),
        display_name: profile_name.to_string(),
        api_base: api_base.to_string(),
        api_key_env,
        env_file,
        setup_url: String::new(),
        default_model: profile_config.default_model.clone(),
        requires_api_key: profile_config.requires_api_key.unwrap_or(true),
    };

    super::external::maybe_schedule_profile_catalog_refresh(
        resolved,
        "named provider profile route build",
    )
}
