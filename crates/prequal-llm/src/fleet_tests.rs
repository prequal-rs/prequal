use super::*;

fn scrape(queries: f64, hits: f64) -> EngineStats {
    EngineStats { prefix_queries: Some(queries), prefix_hits: Some(hits), ..EngineStats::default() }
}

#[test]
fn confidence_tracks_observed_hits_and_resets_on_restart() {
    let mut replica = Replica::new("127.0.0.1:1".parse().unwrap());
    let now = Instant::now();
    replica.observe(scrape(0.0, 0.0), now);
    let prompt = Prompt { blocks: (0..100).collect(), tokens: 6_400 };
    replica.record_route(&prompt, 0, 10);
    replica.count_prefilled(6_400, 3_200);
    replica.observe(scrape(3_200.0, 300.0), now);
    replica.count_prefilled(6_400, 3_200);
    replica.observe(scrape(6_400.0, 640.0), now);
    assert!(replica.confidence < 0.9, "only 10% of predicted hits happened, measured over the whole window");
    assert!(replica.cache.len() == 100, "no forgetting, only discounting");
    replica.observe(scrape(10.0, 0.0), now);
    assert_eq!(replica.confidence, 1.0);
    assert!(replica.cache.is_empty(), "restart forgets the cache model");
}

#[test]
fn sizes_the_cache_model_in_measured_bytes_per_engine_token() {
    let mut replica = Replica::new("127.0.0.1:1".parse().unwrap());
    let now = Instant::now();
    let scrape = |queries| EngineStats { cache_tokens: Some(1_024.0), ..scrape(queries, 0.0) };
    let capacity = |bytes_per_token: usize| 1_024 * bytes_per_token / BLOCK_BYTES;
    replica.observe(scrape(0.0), now);
    assert_eq!(replica.cache.capacity(), capacity(BYTES_PER_TOKEN), "assumed until measured");
    // A window of prompts (4 bytes per router token) the engine counted as 16 bytes per token, in two scrapes.
    let window_tokens = MIN_BYTES_FOR_CALIBRATION / BYTES_PER_TOKEN as f64;
    let engine_tokens = MIN_BYTES_FOR_CALIBRATION / 16.0;
    replica.count_prefilled(window_tokens as u64 / 2, 0);
    replica.observe(scrape(engine_tokens / 2.0), now);
    assert_eq!(replica.cache.capacity(), capacity(BYTES_PER_TOKEN), "half a window measures nothing");
    replica.count_prefilled(window_tokens as u64 / 2, 0);
    replica.observe(scrape(engine_tokens), now);
    assert_eq!(replica.cache.capacity(), capacity(16));
    // Another router's equal traffic doubles the engine's count: the model shrinks toward this router's share.
    replica.count_prefilled(window_tokens as u64, 0);
    replica.observe(scrape(3.0 * engine_tokens), now);
    assert_eq!(replica.cache.capacity(), capacity(12), "averaged with the first window");
}
