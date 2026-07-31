//! Live probe: can the memory sidecar actually reach a working judge?
//!
//! Ignored by default because it spends real quota and needs credentials. Run
//! it deliberately when the memory judge looks unhealthy:
//!
//! ```text
//! cargo test -p jcode-base --test sidecar_live_fallback -- --ignored --nocapture
//! ```
//!
//! This is the check that would have caught the fallback sending
//! `gpt-5.6-luna` to Anthropic: unit tests asserted the routing decision, but
//! only a real call proves the chosen backend accepts the model it is handed.

#[tokio::test]
#[ignore = "spends real provider quota; run deliberately"]
async fn the_memory_sidecar_can_reach_a_working_judge() {
    let sidecar = jcode_base::sidecar::Sidecar::new();
    println!(
        "sidecar backend={} model={}",
        sidecar.backend_name(),
        sidecar.model_name()
    );

    let result = sidecar
        .complete(
            "You are a terse assistant. Reply with exactly one word.",
            "Reply with the single word: OK",
        )
        .await;

    match result {
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
