use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Upper bound on the number of distinct session/provider/model/format baselines we retain
/// in memory. Without a cap this map grows without bound for the lifetime of a long-running
/// server process as new sessions are created. When at capacity, the least-recently-updated
/// entry is evicted to make room for a new key.
const MAX_BASELINE_ENTRIES: usize = 256;

/// Monotonic tick used as a lightweight LRU clock. Plain `u64` counter incremented on every
/// insert; avoids pulling in a dedicated LRU crate or relying on wall-clock time (which would
/// be awkward to control in tests).
static BASELINE_TICK: AtomicU64 = AtomicU64::new(0);

fn next_tick() -> u64 {
    BASELINE_TICK.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone)]
struct ProviderInputSnapshot {
    request_hash: u64,
    item_hashes: Vec<u64>,
    item_hashes_hash: u64,
    system_hash: Option<u64>,
    tools_hash: Option<u64>,
    captured_at: Instant,
    last_touch: u64,
}

static PROVIDER_INPUT_BASELINES: LazyLock<Mutex<HashMap<String, ProviderInputSnapshot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Insert `snapshot` under `key`, evicting the least-recently-updated entry first if the map
/// is already at `cap` and `key` is not already present. Returns the previous value for `key`,
/// mirroring `HashMap::insert`.
fn insert_bounded(
    baselines: &mut HashMap<String, ProviderInputSnapshot>,
    key: String,
    snapshot: ProviderInputSnapshot,
    cap: usize,
) -> Option<ProviderInputSnapshot> {
    if !baselines.contains_key(&key) && baselines.len() >= cap {
        if let Some(evict_key) = baselines
            .iter()
            .min_by_key(|(_, value)| value.last_touch)
            .map(|(key, _)| key.clone())
        {
            baselines.remove(&evict_key);
        }
    }
    baselines.insert(key, snapshot)
}

pub fn stable_hash_str(value: &str) -> u64 {
    let digest = Sha256::digest(value.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

pub fn stable_hash_json<T: Serialize + ?Sized>(value: &T) -> u64 {
    let encoded = serde_json::to_string(value).unwrap_or_default();
    stable_hash_str(&encoded)
}

fn stable_json_len<T: Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|encoded| encoded.len())
        .unwrap_or_default()
}

fn item_hashes(items: &[Value]) -> Vec<u64> {
    items.iter().map(stable_hash_json).collect()
}

fn prefix_matches(current: &[u64], previous: &[u64]) -> bool {
    if previous.len() > current.len() {
        return false;
    }
    current[..previous.len()] == *previous
}

fn common_prefix_len(current: &[u64], previous: &[u64]) -> usize {
    current
        .iter()
        .zip(previous.iter())
        .take_while(|(current, previous)| current == previous)
        .count()
}

/// Log a privacy-preserving fingerprint of the provider-specific prompt payload.
///
/// `payload` should be the prompt/cache-relevant request shape after provider-specific
/// normalization, not the high-level Jcode message list. Do not include volatile transport
/// IDs unless they are intentionally part of the cache key. `items` should be the ordered
/// provider-visible message/content array so prefix drift can be diagnosed by index.
#[allow(clippy::too_many_arguments)]
pub fn log_provider_canonical_input(
    provider: &str,
    model: &str,
    format: &str,
    payload: &Value,
    items: &[Value],
    system: Option<&Value>,
    tools: Option<&Value>,
    tool_count: Option<usize>,
    extra_fields: &[(&str, String)],
) {
    let request_hash = stable_hash_json(payload);
    let request_json_chars = stable_json_len(payload);
    let item_hashes = item_hashes(items);
    let item_hashes_hash = stable_hash_json(&item_hashes);
    let input_hash = stable_hash_json(items);
    let system_hash = system.map(stable_hash_json);
    let system_json_chars = system.map(stable_json_len);
    let tools_hash = tools.map(stable_hash_json);
    let tools_json_chars = tools.map(stable_json_len);
    let first_item_hash = item_hashes.first().copied();
    let last_item_hash = item_hashes.last().copied();

    let log_context = jcode_logging::current_context_snapshot();
    let session_key = log_context.session.as_deref().unwrap_or("no-session");
    let key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        session_key, provider, model, format
    );
    let snapshot = ProviderInputSnapshot {
        request_hash,
        item_hashes: item_hashes.clone(),
        item_hashes_hash,
        system_hash,
        tools_hash,
        captured_at: Instant::now(),
        last_touch: next_tick(),
    };

    let previous = PROVIDER_INPUT_BASELINES
        .lock()
        .map(|mut baselines| insert_bounded(&mut baselines, key, snapshot, MAX_BASELINE_ENTRIES))
        .ok()
        .flatten();

    let previous_age_secs = previous
        .as_ref()
        .map(|previous| previous.captured_at.elapsed().as_secs());
    let request_changed = previous
        .as_ref()
        .map(|previous| previous.request_hash != request_hash);
    let item_hashes_changed = previous
        .as_ref()
        .map(|previous| previous.item_hashes_hash != item_hashes_hash);
    let prefix_matches = previous
        .as_ref()
        .map(|previous| prefix_matches(&item_hashes, &previous.item_hashes));
    let common_prefix_items = previous
        .as_ref()
        .map(|previous| common_prefix_len(&item_hashes, &previous.item_hashes));
    let first_changed_item_index = common_prefix_items
        .zip(previous.as_ref().map(|previous| previous.item_hashes.len()))
        .and_then(|(common, previous_len)| (common < previous_len).then_some(common));
    let previous_item_count = previous.as_ref().map(|previous| previous.item_hashes.len());
    let system_changed = previous
        .as_ref()
        .map(|previous| previous.system_hash != system_hash);
    let tools_changed = previous
        .as_ref()
        .map(|previous| previous.tools_hash != tools_hash);

    let mut extras = String::new();
    for (key, value) in extra_fields {
        if !key.is_empty() && !value.is_empty() {
            extras.push(' ');
            extras.push_str(key);
            extras.push('=');
            extras.push_str(value);
        }
    }

    jcode_logging::info(&format!(
        "PROVIDER_CANONICAL_INPUT: provider={} model={} format={} request_hash={} request_json_chars={} \
         input_hash={} item_count={} previous_item_count={:?} item_hashes_hash={} first_item_hash={:?} last_item_hash={:?} \
         previous_age_secs={:?} prefix_matches={:?} common_prefix_items={:?} first_changed_item_index={:?} \
         request_changed={:?} item_hashes_changed={:?} system_hash={:?} system_json_chars={:?} system_changed={:?} \
         tools_hash={:?} tools_json_chars={:?} tool_count={:?} tools_changed={:?}{}",
        provider,
        model,
        format,
        request_hash,
        request_json_chars,
        input_hash,
        items.len(),
        previous_item_count,
        item_hashes_hash,
        first_item_hash,
        last_item_hash,
        previous_age_secs,
        prefix_matches,
        common_prefix_items,
        first_changed_item_index,
        request_changed,
        item_hashes_changed,
        system_hash,
        system_json_chars,
        system_changed,
        tools_hash,
        tools_json_chars,
        tool_count,
        tools_changed,
        extras,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefix_matching_allows_append_only_growth() {
        assert!(prefix_matches(&[1, 2, 3], &[1, 2]));
    }

    #[test]
    fn prefix_matching_detects_changed_prefix() {
        assert!(!prefix_matches(&[1, 9, 3], &[1, 2]));
        assert_eq!(common_prefix_len(&[1, 9, 3], &[1, 2]), 1);
    }

    #[test]
    fn json_hashes_are_content_sensitive() {
        assert_ne!(
            stable_hash_json(&json!({"a": 1})),
            stable_hash_json(&json!({"a": 2}))
        );
    }

    fn make_snapshot(tick: u64) -> ProviderInputSnapshot {
        ProviderInputSnapshot {
            request_hash: tick,
            item_hashes: Vec::new(),
            item_hashes_hash: 0,
            system_hash: None,
            tools_hash: None,
            captured_at: Instant::now(),
            last_touch: tick,
        }
    }

    #[test]
    fn bounded_insert_caps_size_and_keeps_newest_keys() {
        let cap = 16;
        let total = cap + 10;
        let mut baselines: HashMap<String, ProviderInputSnapshot> = HashMap::new();

        for tick in 0..total {
            let key = format!("session-{tick}\u{1f}provider\u{1f}model\u{1f}format");
            insert_bounded(&mut baselines, key, make_snapshot(tick as u64), cap);
            assert!(baselines.len() <= cap);
        }

        assert!(baselines.len() <= cap);

        // The most recently inserted keys (the last `cap` of them) must have survived
        // eviction, since eviction always removes the least-recently-updated entry.
        for tick in (total - cap)..total {
            let key = format!("session-{tick}\u{1f}provider\u{1f}model\u{1f}format");
            assert!(
                baselines.contains_key(&key),
                "expected newest key {key} to survive eviction"
            );
        }

        // The oldest keys should have been evicted.
        for tick in 0..(total - cap) {
            let key = format!("session-{tick}\u{1f}provider\u{1f}model\u{1f}format");
            assert!(
                !baselines.contains_key(&key),
                "expected oldest key {key} to be evicted"
            );
        }
    }
}
