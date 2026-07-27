//! Model filtering for the model picker.
//!
//! This module provides predicates that decide which models are suitable for
//! display in the model picker, hiding non-coding and deprecated models that
//! would otherwise clutter the UI.

/// Returns `true` if the model is a selectable coding model suitable for the
/// model picker. Returns `false` for video/image/audio/realtime models,
/// embeddings/moderation, ancient fine-tunes, deprecated codex models, and
/// deep-research/search models.
///
/// This is a pure string-based filter; it does not consult any catalog.
pub fn is_selectable_coding_model(model_id: &str) -> bool {
    let model_lower = model_id.to_ascii_lowercase();

    // Video/image/audio/realtime models
    if model_lower.contains("sora")
        || model_lower.contains("realtime")
        || model_lower.contains("audio")
        || model_lower.contains("livetranslate")
        || model_lower.contains("omni-flash-realtime")
        || model_lower.contains("-tts")
        || model_lower.contains("tts-")
        || model_lower.contains("whisper")
        || model_lower.contains("dall-e")
        || model_lower.contains("image")
    {
        return false;
    }

    // Embeddings/moderation/rerank
    if model_lower.contains("embedding")
        || model_lower.contains("moderation")
        || model_lower.contains("rerank")
    {
        return false;
    }

    // Ancient fine-tunes and legacy models
    if model_id.starts_with("ada:")
        || model_id.starts_with("ft:")
        || model_lower.starts_with("babbage")
        || model_lower.starts_with("davinci")
        || model_lower.starts_with("curie")
        || model_id == "gpt-3.5-turbo-instruct"
        || model_id == "computer-use-preview"
    {
        return false;
    }

    // Deprecated OpenAI codex models that 404 (keep gpt-5.3-codex - it works)
    if matches!(
        model_id,
        "gpt-5-codex"
            | "gpt-5.1-codex"
            | "gpt-5.1-codex-max"
            | "gpt-5.1-codex-mini"
            | "gpt-5.2-codex"
    ) {
        return false;
    }

    // Deep-research and search-api models
    if model_lower.contains("deep-research")
        || model_lower.contains("search-api")
        || model_lower.contains("search-preview")
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filters_video_image_audio_realtime() {
        // sora
        assert!(!is_selectable_coding_model("sora-1.0"));
        assert!(!is_selectable_coding_model("openai/sora-2.0-turbo"));

        // realtime
        assert!(!is_selectable_coding_model("gpt-4o-realtime"));
        assert!(!is_selectable_coding_model("openai/gpt-4o-realtime-preview"));

        // audio
        assert!(!is_selectable_coding_model("gpt-4o-audio-preview"));

        // livetranslate
        assert!(!is_selectable_coding_model("gpt-4o-livetranslate"));

        // omni-flash-realtime
        assert!(!is_selectable_coding_model("omni-flash-realtime"));

        // tts
        assert!(!is_selectable_coding_model("openai/tts-1"));
        assert!(!is_selectable_coding_model("gpt-4o-mini-tts"));

        // whisper
        assert!(!is_selectable_coding_model("whisper-1"));
        assert!(!is_selectable_coding_model("openai/whisper-large-v3"));

        // dall-e
        assert!(!is_selectable_coding_model("dall-e-3"));
        assert!(!is_selectable_coding_model("openai/dall-e-2"));

        // image
        assert!(!is_selectable_coding_model("gpt-image-1"));
        assert!(!is_selectable_coding_model("openai/gpt-image-2"));
    }

    #[test]
    fn test_filters_embeddings_moderation_rerank() {
        // embedding
        assert!(!is_selectable_coding_model("text-embedding-3-small"));
        assert!(!is_selectable_coding_model("text-embedding-ada-002"));
        assert!(!is_selectable_coding_model("openai/text-embedding-3-large"));

        // moderation
        assert!(!is_selectable_coding_model("text-moderation-latest"));
        assert!(!is_selectable_coding_model("omni-moderation-latest"));

        // rerank
        assert!(!is_selectable_coding_model("rerank-1"));
        assert!(!is_selectable_coding_model("cohere/rerank-v3.5"));
    }

    #[test]
    fn test_filters_ancient_fine_tunes_and_legacy() {
        // ada fine-tunes
        assert!(!is_selectable_coding_model("ada:ft-personal-2023"));
        assert!(!is_selectable_coding_model("ada:personal-org"));

        // ft: prefix (general fine-tune)
        assert!(!is_selectable_coding_model("ft:gpt-4o-mini-2024-07-18:personal:org"));

        // babbage
        assert!(!is_selectable_coding_model("babbage-002"));

        // davinci
        assert!(!is_selectable_coding_model("davinci-002"));

        // curie
        assert!(!is_selectable_coding_model("curie-001"));

        // gpt-3.5-turbo-instruct (exact match)
        assert!(!is_selectable_coding_model("gpt-3.5-turbo-instruct"));

        // computer-use-preview
        assert!(!is_selectable_coding_model("computer-use-preview"));
    }

    #[test]
    fn test_filters_deprecated_codex_models() {
        // Deprecated codex models that 404
        assert!(!is_selectable_coding_model("gpt-5-codex"));
        assert!(!is_selectable_coding_model("gpt-5.1-codex"));
        assert!(!is_selectable_coding_model("gpt-5.1-codex-max"));
        assert!(!is_selectable_coding_model("gpt-5.1-codex-mini"));
        assert!(!is_selectable_coding_model("gpt-5.2-codex"));

        // gpt-5.3-codex should PASS - it works
        assert!(is_selectable_coding_model("gpt-5.3-codex"));
    }

    #[test]
    fn test_filters_deep_research_and_search() {
        // deep-research
        assert!(!is_selectable_coding_model("openai/deep-research"));
        assert!(!is_selectable_coding_model("gpt-4.1-deep-research"));

        // search-api
        assert!(!is_selectable_coding_model("search-api-model"));

        // search-preview
        assert!(!is_selectable_coding_model("openai/search-preview"));
    }

    #[test]
    fn test_positive_coding_models_pass() {
        // All these should be selectable
        assert!(is_selectable_coding_model("gpt-5.5"));
        assert!(is_selectable_coding_model("claude-fable-5"));
        assert!(is_selectable_coding_model("glm-5.2"));
        assert!(is_selectable_coding_model("deepseek-v4-pro"));
        assert!(is_selectable_coding_model("kimi-k3"));
        assert!(is_selectable_coding_model("qwen3-coder-plus"));
        assert!(is_selectable_coding_model("MiniMax-M2.7"));
        assert!(is_selectable_coding_model("gpt-5.3-codex"));

        // Also test some common coding models
        assert!(is_selectable_coding_model("gpt-4o"));
        assert!(is_selectable_coding_model("gpt-4-turbo"));
        assert!(is_selectable_coding_model("claude-3-5-sonnet-20241022"));
        assert!(is_selectable_coding_model("claude-sonnet-4-20250514"));
        assert!(is_selectable_coding_model("deepseek-r1"));
        assert!(is_selectable_coding_model("openrouter/deepseek/deepseek-chat-v3-0324"));
        assert!(is_selectable_coding_model("anthropic/claude-3.5-sonnet"));
    }
}