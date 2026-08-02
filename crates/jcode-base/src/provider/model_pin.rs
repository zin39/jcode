//! Parsing for `<prefix>:<model>` model pins.
//!
//! A pin's prefix selects where the request is billed, so these parsers are the
//! difference between hitting a provider directly and paying a reseller for the
//! same model id. They are pure functions over the pin string plus the provider
//! catalog and config, deliberately kept apart from runtime binding.

use super::explicit_model_provider_prefix;

/// Parse an `<profile>:<model>` pin against the OpenAI-compatible catalog.
///
/// Also accepts the fully qualified `openai-compatible:<profile>:<model>` form,
/// which is the shape the route catalog emits in `api_method`.
pub(super) fn openai_compatible_model_prefix(
    model: &str,
) -> Option<(crate::provider_catalog::OpenAiCompatibleProfile, &str)> {
    // A fully qualified route spec must not have its transport segment parsed
    // as the profile id: that leaves `<profile>:<model>` as the model name,
    // falls through to the generic catch-all profile, and silently bills the
    // wrong endpoint. Strip it only when a profile segment actually follows.
    let model = model
        .trim()
        .strip_prefix("openai-compatible:")
        .filter(|rest| {
            rest.split_once(':').is_some_and(|(profile, model)| {
                !profile.trim().is_empty() && !model.trim().is_empty()
            })
        })
        .unwrap_or(model);

    let (prefix, rest) = model.split_once(':')?;
    if explicit_model_provider_prefix(model).is_some() {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    let profile = crate::provider_catalog::openai_compatible_profile_by_id(prefix)?;
    Some((profile, rest))
}

/// The model id inside a transport-only `openai-compatible:<model>` pin.
///
/// The route catalog can emit `api_method = "openai-compatible"` with no
/// profile segment, and [`crate::provider::RouteSelection::routed_model_spec`]
/// then produces `openai-compatible:<model>`. That is a transport declaration,
/// not a profile pin: there is no profile id to look up.
///
/// [`openai_compatible_model_prefix`] returns `None` for it (correctly, since
/// `openai-compatible` names no profile), so the spec used to reach
/// `MultiProvider::set_model`'s heuristics still carrying its prefix. Nothing
/// matched, and the "unknown model, try the current provider" fallthrough sent
/// it to whatever was active. Measured: selecting `qwen3.7-max` while Claude was
/// active failed with "Model qwen3.7-max not supported by Anthropic provider"
/// even though a DashScope route served it.
///
/// Returning the bare id lets the caller resolve the owning profile from the
/// live route catalog, which is the same recovery bare hand-typed ids already
/// get.
pub(super) fn openai_compatible_transport_only_model(model: &str) -> Option<&str> {
    let rest = model.trim().strip_prefix("openai-compatible:")?.trim();
    if rest.is_empty() {
        return None;
    }
    // A profile segment means this is a real pin for
    // `openai_compatible_model_prefix` to parse, not a transport-only spec.
    if rest
        .split_once(':')
        .is_some_and(|(profile, model)| !profile.trim().is_empty() && !model.trim().is_empty())
    {
        return None;
    }
    Some(rest)
}

/// Find the configured OpenAI-compatible profile that serves a bare model id,
/// using the live route catalog as the source of truth.
///
/// Route specs from the picker carry a `<profile>:<model>` prefix, but
/// hand-typed `/model <id>` and saved sessions can carry the bare id. The
/// active profile wins when several profiles serve the same id, so a re-select
/// of the current model never silently hops endpoints.
pub(super) fn openai_compatible_profile_owning_model(
    model: &str,
    active_profile_id: Option<&str>,
    routes: impl FnOnce() -> Vec<crate::provider::ModelRoute>,
) -> Option<crate::provider_catalog::OpenAiCompatibleProfile> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }

    let mut fallback: Option<String> = None;
    for route in routes() {
        if !route.available || route.model != model {
            continue;
        }
        let Some(profile_id) = route
            .api_method
            .strip_prefix("openai-compatible:")
            .map(str::trim)
            .filter(|profile_id| !profile_id.is_empty())
        else {
            continue;
        };
        if active_profile_id == Some(profile_id) {
            fallback = Some(profile_id.to_string());
            break;
        }
        if fallback.is_none() {
            fallback = Some(profile_id.to_string());
        }
    }

    crate::provider_catalog::openai_compatible_profile_by_id(&fallback?)
}

/// Resolve an OpenAI-compatible route spec to the profile that should serve it.
///
/// `Ok(None)` means the spec names no OpenAI-compatible profile and the caller
/// should keep parsing. `Err` is a resolution failure that must NOT fall
/// through to the active provider.
///
/// The transport-only `openai-compatible:<model>` form is decided first,
/// before the ordinary `<profile>:<model>` pin. The
/// catalog has a generic catch-all profile whose id is literally
/// "openai-compatible" (api_base https://api.openai.com/v1), so the pin parser
/// matches this spec and reads the transport segment as that profile.
/// Unconfigured, the switch errors and the caller falls onward to the active
/// provider: that is how selecting `qwen3.7-max` under Claude reported "Model
/// qwen3.7-max not supported by Anthropic provider". Configured, it is worse,
/// because the request would be billed to api.openai.com for a model another
/// provider serves.
pub(super) fn resolve_openai_compatible_target(
    requested_model: &str,
    owning_profile: impl FnOnce(&str) -> Option<crate::provider_catalog::OpenAiCompatibleProfile>,
) -> anyhow::Result<Option<(crate::provider_catalog::OpenAiCompatibleProfile, &str)>> {
    let Some(target_model) = openai_compatible_transport_only_model(requested_model) else {
        // Not transport-only: fall back to the ordinary `<profile>:<model>` pin.
        return Ok(openai_compatible_model_prefix(requested_model));
    };
    match owning_profile(target_model) {
        Some(profile) => Ok(Some((profile, target_model))),
        // Naming the route that could not be resolved beats blaming a provider
        // the user never asked for.
        None => Err(anyhow::anyhow!(
            "No configured OpenAI-compatible provider serves model '{}'. \
             Pin the profile explicitly (`<profile>:{}`) or run `jcode login` \
             for the provider that hosts it.",
            target_model,
            target_model
        )),
    }
}

/// Parse a `<name>:<model>` spec whose prefix is a user-defined named provider
/// profile from config (`[providers.<name>]`). Built-in provider prefixes and
/// catalog profile ids take precedence and never reach here.
pub(super) fn named_provider_profile_model_prefix(model: &str) -> Option<(String, String)> {
    let (prefix, rest) = model.split_once(':')?;
    if explicit_model_provider_prefix(model).is_some()
        || openai_compatible_model_prefix(model).is_some()
    {
        return None;
    }
    let prefix = prefix.trim();
    let rest = rest.trim();
    if prefix.is_empty() || rest.is_empty() {
        return None;
    }
    crate::config::config()
        .providers
        .contains_key(prefix)
        .then(|| (prefix.to_string(), rest.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully qualified route spec (`openai-compatible:<profile>:<model>`) must
    /// resolve to the named profile, not the generic catch-all.
    ///
    /// Regression: `agents.swarm_model = "openai-compatible:deepseek:deepseek-v4-pro"`
    /// billed every swarm worker to the generic profile (dashscope) instead of
    /// DeepSeek direct, because `split_once(':')` took `openai-compatible` as the
    /// profile id. Asserts the resolved endpoint, since that is what costs money.
    #[test]
    fn fully_qualified_compatible_pin_resolves_to_its_profile_not_the_generic_endpoint() {
        let (profile, model) =
            openai_compatible_model_prefix("openai-compatible:deepseek:deepseek-v4-pro")
                .expect("fully qualified route spec should resolve to a profile");
        assert_eq!(profile.id, "deepseek");
        assert_eq!(model, "deepseek-v4-pro");
        assert!(
            profile.api_base.contains("api.deepseek.com"),
            "pin must bill DeepSeek direct, got endpoint {}",
            profile.api_base
        );

        // The two-segment form is canonical and must keep working, and a generic
        // two-segment pin must not be mistaken for a profile pin.
        let (short, short_model) = openai_compatible_model_prefix("deepseek:deepseek-v4-pro")
            .expect("two-segment pin should resolve");
        assert_eq!((short.id, short_model), (profile.id, model));
        assert!(
            openai_compatible_model_prefix("openai-compatible:glm-5.2")
                .is_none_or(|(p, _)| p.id != "deepseek"),
            "a two-segment generic pin must not resolve to the DeepSeek profile"
        );
    }

    /// Regression for the live failure: the picker emitted
    /// `api_method=openai-compatible` (no profile), which
    /// `RouteSelection::routed_model_spec` turned into
    /// `openai-compatible:qwen3.7-max`. That is a transport declaration, so it
    /// must be recognized as one and yield the bare model id for catalog
    /// resolution instead of being mistaken for a profile pin.
    #[test]
    fn transport_only_spec_yields_the_bare_model_id() {
        assert_eq!(
            openai_compatible_transport_only_model("openai-compatible:qwen3.7-max"),
            Some("qwen3.7-max")
        );
        // The prefix alone is not a model.
        assert_eq!(
            openai_compatible_transport_only_model("openai-compatible:"),
            None
        );
        assert_eq!(
            openai_compatible_transport_only_model("openai-compatible:   "),
            None
        );
    }

    /// A real `<profile>:<model>` pin must NOT be treated as transport-only, or
    /// the explicit profile would be discarded and re-resolved from the catalog,
    /// which is exactly how a pin silently bills the wrong endpoint.
    #[test]
    fn profile_qualified_spec_is_not_transport_only() {
        assert_eq!(
            openai_compatible_transport_only_model("openai-compatible:dashscope:qwen3.7-max"),
            None
        );
        // Bare ids and other prefixes are untouched.
        assert_eq!(openai_compatible_transport_only_model("qwen3.7-max"), None);
        assert_eq!(
            openai_compatible_transport_only_model("dashscope:qwen3.7-max"),
            None
        );
    }

    /// Both parsers can match `openai-compatible:<model>`, because the catalog
    /// has a catch-all profile whose id is literally "openai-compatible". That
    /// overlap is exactly the bug, so pin the precedence the caller relies on:
    /// the transport-only reading must win, or the spec resolves to a generic
    /// profile pointed at api.openai.com for a model another provider serves.
    #[test]
    fn transport_only_reading_must_take_precedence_on_the_overlap() {
        let spec = "openai-compatible:qwen3.7-max";
        assert_eq!(
            openai_compatible_transport_only_model(spec),
            Some("qwen3.7-max"),
            "transport-only parser must claim the profile-less spec"
        );
        if let Some((profile, _)) = openai_compatible_model_prefix(spec) {
            assert_eq!(
                profile.id, "openai-compatible",
                "the pin parser only matches here via the catch-all profile, \
                 which is why set_model must check transport-only first"
            );
        }
    }

    /// A profile-qualified pin must be claimed only by the pin parser, so an
    /// explicit profile is never re-resolved from the catalog.
    ///
    /// Uses a catalog profile id, since that is what the pin parser resolves
    /// against; config-defined profiles take the named-profile path instead.
    #[test]
    fn profile_qualified_spec_belongs_only_to_the_pin_parser() {
        let spec = "openai-compatible:zai:glm-4.7";
        assert_eq!(
            openai_compatible_transport_only_model(spec),
            None,
            "a spec carrying an explicit profile is not transport-only"
        );
        let (profile, model) =
            openai_compatible_model_prefix(spec).expect("profile pin should parse");
        assert_eq!(profile.id, "zai");
        assert_eq!(model, "glm-4.7");
    }
}
