//! Live probes for the memory sidecar's backend fallback.
//!
//! Ignored by default because they spend real provider quota and need
//! credentials. Run them deliberately when the memory judge looks unhealthy:
//!
//! ```text
//! cargo test -p jcode-base --test sidecar_live_fallback -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is required: these probes share the process-wide HTTP
//! client, and running them on separate `#[tokio::test]` runtimes concurrently
//! tears that client down mid-flight ("dispatch task is gone"). That is a
//! harness artifact, not a product failure.
//!
//! These are the checks unit tests cannot make. Unit tests assert the *routing
//! decision*; only a real call proves the chosen backend accepts the model it
//! is handed. That gap is exactly how two defects shipped on 2026-07-31: the
//! fallback kept the primary's model (Claude rejected `gpt-5.6-luna`), and the
//! Claude id itself was a nonexistent dated snapshot.

/// The configured sidecar must be able to reach a working judge right now.
///
/// A failure here means memory recall is degraded for real, not in theory.
#[tokio::test]
#[ignore = "spends real provider quota; run deliberately"]
async fn the_memory_sidecar_can_reach_a_working_judge() {
    let sidecar = jcode_base::sidecar::Sidecar::new();
    println!(
        "sidecar backend={} model={}",
        sidecar.backend_name(),
        sidecar.model_name()
    );

    match sidecar
        .complete(
            "You are a terse assistant. Reply with exactly one word.",
            "Reply with the single word: OK",
        )
        .await
    {
        Ok(text) => {
            println!("judge reachable, replied: {:?}", text.trim());
            assert!(
                !text.trim().is_empty(),
                "a reachable judge must return text, got an empty reply"
            );
        }
        Err(err) => {
            panic!("no sidecar backend is reachable, so memory recall is degraded: {err:#}")
        }
    }
}

/// A DEAD primary must fall through to a working backend.
///
/// This is the actual regression: when the primary account was exhausted, every
/// judge vote died with it and memory silently dropped to the no-LLM path.
/// Pinning the primary to an unservable model reproduces that failure without
/// waiting for a real outage, so the fallback is exercised rather than assumed.
///
/// Note the previous test can pass on the primary alone once quota resets, so
/// it does NOT prove the fallback works. This one does.
#[tokio::test]
#[ignore = "spends real provider quota; run deliberately"]
async fn a_dead_primary_falls_through_to_a_working_backend() {
    let dead_primary =
        jcode_base::sidecar::Sidecar::with_openai_model("gpt-does-not-exist-9", None);

    let result = dead_primary
        .complete(
            "You are a terse assistant. Reply with exactly one word.",
            "Reply with the single word: OK",
        )
        .await;

    match &result {
        Ok(text) => println!("fallback served the request: {:?}", text.trim()),
        Err(err) => println!(
            "every backend failed: {}",
            err.to_string().chars().take(160).collect::<String>()
        ),
    }

    let text = result.expect("a dead primary must fall through to a working backend");
    assert!(
        !text.trim().is_empty(),
        "the fallback must return real text, not an empty reply"
    );
}
