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
}
