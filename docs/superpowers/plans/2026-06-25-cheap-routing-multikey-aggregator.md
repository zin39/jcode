# Cheap-Routing Multi-Key Model Aggregator — Implementation Plan (Plan 6)

> **For agentic workers:** Use TDD where a pure surface exists; compile-verify the wiring. Steps use checkbox (`- [ ]`).

**Goal:** Make cheap_route see models from EVERY configured `[providers.X]` block (deepseek, dashscope, modelscope, nvidia, openrouter, …), not just the active OpenRouter slot. Today `provider.model_routes()` only surfaces the active provider + 36 built-in profiles + ONE named-config slot, so the user's other configured keys' models are invisible to cheap-routing.

**Architecture:** Add a route aggregator used by `ProviderCheapBackend::routes()`: it returns `dedupe_model_routes(provider.model_routes() ++ configured_named_provider_routes())`. `configured_named_provider_routes()` iterates `config().providers` (the named `[providers.X]` blocks), and for each, emits a `ModelRoute` per model — model ids come from the union of the block's static `models[]` list AND its discovered disk cache (`load_disk_cache_entry_for_namespace(name)`), with pricing from `metered_pricing_for_source_with_tier("openai-compatible:{name}", id, None)` and `available` set by whether the block's key is present. The route-building is split into a PURE function (`build_named_provider_routes`) that is unit-tested; the config/cache/pricing lookups are thin wiring.

**Tech Stack:** Rust, `jcode_provider_core::{ModelRoute, RouteCheapnessEstimate, selection::dedupe_model_routes}`, `jcode-config-types::NamedProviderConfig`, `jcode-provider-openrouter::load_disk_cache_entry_for_namespace`, `jcode-base` pricing.

**Run cargo with:** `. "$HOME/.cargo/env" && cargo test -p jcode-app-core ...`

**Verified anchors:**
- `NamedProviderConfig` — `crates/jcode-config-types/src/lib.rs:375` (fields: `base_url`, `api_key_env: Option<String>`, `api_key: Option<String>`, `env_file: Option<String>`, `default_model: Option<String>`, `model_catalog: bool`, `models: Vec<NamedProviderModelConfig>`). `NamedProviderModelConfig.id: String` (`:358`).
- Config accessor: `crate::config::config()` (jcode-base `config.rs:245`). Find the field holding the named providers map (likely `config().providers: HashMap<String, NamedProviderConfig>` or under a sub-struct — CONFIRM by reading the `Config` struct in `crates/jcode-base/src/config.rs` / `jcode-config-types`).
- Discovered-models cache: `jcode_provider_openrouter::load_disk_cache_entry_for_namespace(namespace: &str) -> Option<DiskCache>` (`crates/jcode-provider-openrouter/src/lib.rs:411`). `DiskCache.models` is a `Vec` of entries each with `.id: String` (CONFIRM the exact `DiskCache`/model-entry struct field names by reading near `lib.rs:411` + the `DiskCache` definition).
- Pricing: `crate::provider::pricing::metered_pricing_for_source_with_tier(source_key: &str, model: &str, tier: Option<&str>) -> Option<RouteCheapnessEstimate>` (`crates/jcode-base/src/provider/pricing.rs:131`). Source key form for a named provider: `"openai-compatible:{name}"`.
- Key-present check: `jcode_provider_env::load_api_key_from_env_or_config(api_key_env: Option<&str>, env_file: Option<&str>) -> Option<String>` (CONFIRM exact name/signature in `crates/jcode-provider-env`). Used by `provider_catalog.rs:849`.
- Dedup: `jcode_provider_core::selection::dedupe_model_routes(Vec<ModelRoute>) -> Vec<ModelRoute>`.
- `ModelRoute` fields: `{ model, provider, api_method, available, detail, cheapness: Option<RouteCheapnessEstimate> }`.
- Reference pattern to mirror: `append_openai_compatible_profile_routes` (`crates/jcode-base/src/provider/catalog_routes.rs:395`).

---

### Task 1: Pure `build_named_provider_routes` + tests

**File:** `crates/jcode-app-core/src/agent/cheap_route.rs`

- [ ] **Step 1: Write failing tests** (in the `#[cfg(test)] mod tests` block):

```rust
    #[test]
    fn build_named_provider_routes_unions_static_and_cached_models_with_availability() {
        // name="modelscope", static model deepseek-v4-flash, cached model qwen-x.
        let routes = build_named_provider_routes(
            "modelscope",
            "https://api-inference.modelscope.cn/v1",
            &["deepseek-v4-flash".to_string()],   // static (config) ids
            &["qwen-x".to_string(), "deepseek-v4-flash".to_string()], // discovered (cache) ids
            true,                                  // key present -> available
            |_source, _model| None,                // pricing lookup stub
        );

        let models: std::collections::BTreeSet<&str> =
            routes.iter().map(|r| r.route_model()).collect();
        // union, deduped
        assert!(models.contains("deepseek-v4-flash"));
        assert!(models.contains("qwen-x"));
        assert_eq!(routes.len(), 2);
        // all carry the named-provider api_method + availability + base url detail
        assert!(routes.iter().all(|r| r.api_method == "openai-compatible:modelscope"));
        assert!(routes.iter().all(|r| r.available));
        assert!(routes.iter().all(|r| r.detail.contains("modelscope")));
    }

    #[test]
    fn build_named_provider_routes_marks_unavailable_when_no_key() {
        let routes = build_named_provider_routes(
            "deepseek",
            "https://api.deepseek.com/v1",
            &["deepseek-chat".to_string()],
            &[],
            false, // no key
            |_s, _m| None,
        );
        assert_eq!(routes.len(), 1);
        assert!(!routes[0].available);
    }
```

(`route_model()` is a tiny test helper: add `fn route_model(&self) -> &str { &self.model }` via a local trait, OR just use `r.route.model.as_str()` if you keep `CheapRouteCandidate` — here routes are bare `ModelRoute`, so use `r.model.as_str()` directly and drop the helper.)

- [ ] **Step 2: Run → fail** (`cargo test -p jcode-app-core build_named_provider_routes`).

- [ ] **Step 3: Implement the pure builder:**

```rust
use jcode_provider_core::{ModelRoute, RouteCheapnessEstimate};

/// Build cheap-routing candidate routes for one configured named provider.
/// `static_ids` come from the config block's `models[]`; `cached_ids` from the
/// provider's discovered disk catalog. The union (deduped) becomes routes, each
/// priced via `price` and marked available per `key_present`.
fn build_named_provider_routes(
    name: &str,
    base_url: &str,
    static_ids: &[String],
    cached_ids: &[String],
    key_present: bool,
    price: impl Fn(&str, &str) -> Option<RouteCheapnessEstimate>,
) -> Vec<ModelRoute> {
    let api_method = format!("openai-compatible:{name}");
    let mut seen = std::collections::HashSet::new();
    let mut routes = Vec::new();
    for id in static_ids.iter().chain(cached_ids.iter()) {
        if !seen.insert(id.clone()) {
            continue;
        }
        let cheapness = price(&api_method, id);
        routes.push(ModelRoute {
            model: id.clone(),
            provider: name.to_string(),
            api_method: api_method.clone(),
            available: key_present,
            detail: base_url.to_string(),
            cheapness,
        });
    }
    routes
}
```

- [ ] **Step 4: Run → pass.**

- [ ] **Step 5: Commit** (`feat(cheap_route): pure builder for named-provider routes`).

---

### Task 2: Wire the aggregator into `ProviderCheapBackend::routes()`

**File:** `crates/jcode-app-core/src/agent/cheap_route.rs`

- [ ] **Step 1: Implement `configured_named_provider_routes()`** (non-pure wiring — reads config + caches + pricing). Read `crates/jcode-base/src/config.rs` to confirm the field exposing the named providers map, the `DiskCache` model-entry field name, and the key-present helper name; adjust paths to compile:

```rust
fn configured_named_provider_routes() -> Vec<ModelRoute> {
    let cfg = crate::config::config();
    let mut routes = Vec::new();
    for (name, provider_cfg) in cfg.providers.iter() {
        let static_ids: Vec<String> =
            provider_cfg.models.iter().map(|m| m.id.clone()).collect();
        let cached_ids: Vec<String> =
            jcode_provider_openrouter::load_disk_cache_entry_for_namespace(name)
                .map(|cache| cache.models.iter().map(|m| m.id.clone()).collect())
                .unwrap_or_default();
        if static_ids.is_empty() && cached_ids.is_empty() {
            continue;
        }
        let key_present = jcode_provider_env::load_api_key_from_env_or_config(
            provider_cfg.api_key_env.as_deref(),
            provider_cfg.env_file.as_deref(),
        )
        .is_some()
            || provider_cfg.api_key.is_some();
        routes.extend(build_named_provider_routes(
            name,
            &provider_cfg.base_url,
            &static_ids,
            &cached_ids,
            key_present,
            |source, model| {
                crate::provider::pricing::metered_pricing_for_source_with_tier(source, model, None)
            },
        ));
    }
    routes
}
```

- [ ] **Step 2: Use it in `ProviderCheapBackend::routes()`**:

```rust
    fn routes(&self) -> Vec<ModelRoute> {
        let mut routes = self.provider.model_routes();
        routes.extend(configured_named_provider_routes());
        jcode_provider_core::selection::dedupe_model_routes(routes)
    }
```

- [ ] **Step 3: Build + full test** (`cargo build -p jcode --bin jcode` then `cargo test -p jcode-app-core cheap_route`). Only the known-flaky `tool::bash::*`/`server::*` tests may fail; no new failures.

- [ ] **Step 4: Commit** (`feat(cheap_route): aggregate all configured named-provider models into routes`).

---

## Self-Review

**1. Spec coverage:** "cheap_route sees models from every configured key" → Task 2 aggregates all `config().providers` blocks' models (static + discovered) into the candidate routes, with pricing, deduped against the existing route set. The parent's "best for task" suggestion already exists (`build_recommend_prompt`) and now operates over the fuller menu.

**2. Placeholder scan:** Pure builder + tests are concrete. The wiring names three jcode-base/env/openrouter symbols that MUST be confirmed against source (config providers field, `DiskCache` model field, `load_api_key_from_env_or_config`); the plan says to read + adjust. These are the only unknowns and the compiler will pin them.

**3. Type consistency:** `build_named_provider_routes` returns `Vec<ModelRoute>`; `configured_named_provider_routes` returns the same; `routes()` extends + dedupes via `dedupe_model_routes`. Pricing closure `Fn(&str,&str)->Option<RouteCheapnessEstimate>` matches `metered_pricing_for_source_with_tier(.., None)`.

**4. Ambiguity check:** Union/dedup of static+cached ids pinned by test; availability-by-key pinned by test. Discovery of FRESH `/v1/models` for inactive providers (live fetch) is explicitly OUT OF SCOPE for v1 — v1 surfaces statically-configured + already-cached models; a future v2 can trigger per-key catalog refresh.
