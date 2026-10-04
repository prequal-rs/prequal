//! A vLLM-like engine over HTTP: `engine::EngineCore` stepped in real time. Serves `POST /v1/completions` (SSE when
//! `stream`) and `GET /metrics` with vLLM's metric names.

use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde::Deserialize;
use tokio::sync::{Notify, mpsc};

use crate::{
    cache::BLOCK_TOKENS,
    engine::{EngineCore, Job},
    preset::EngineSpec,
};

enum Event {
    Admitted { cached_tokens: u64 },
    Token,
}

type Tag = mpsc::UnboundedSender<Event>;

struct Engine {
    core: Mutex<EngineCore<Tag>>,
    wake: Notify,
}

impl Engine {
    fn core(&self) -> MutexGuard<'_, EngineCore<Tag>> {
        self.core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn submit(&self, job: Job<Tag>) {
        self.core().submit(job);
        self.wake.notify_one();
    }

    fn metrics(&self) -> String {
        let core = self.core();
        let usage = core.active_blocks() as f64 / core.num_blocks() as f64;
        let labels = "{engine=\"0\",model_name=\"sim\"}";
        format!(
            "# TYPE vllm:num_requests_running gauge\nvllm:num_requests_running{labels} {}\n\
             # TYPE vllm:num_requests_waiting gauge\nvllm:num_requests_waiting{labels} {}\n\
             # TYPE vllm:kv_cache_usage_perc gauge\nvllm:kv_cache_usage_perc{labels} {usage:.4}\n\
             # TYPE vllm:prefix_cache_queries counter\nvllm:prefix_cache_queries_total{labels} {}\n\
             # TYPE vllm:prefix_cache_hits counter\nvllm:prefix_cache_hits_total{labels} {}\n\
             # TYPE vllm:cache_config_info gauge\n\
             vllm:cache_config_info{{block_size=\"{BLOCK_TOKENS}\",num_gpu_blocks=\"{}\"}} 1\n",
            core.running(),
            core.waiting(),
            core.prefix_queries,
            core.prefix_hits,
            core.num_blocks(),
        )
    }
}

async fn schedule(engine: Arc<Engine>) {
    loop {
        let step = engine.core().start_step(|tag, cached| {
            let _ = tag.send(Event::Admitted { cached_tokens: cached });
        });
        let Some(step_us) = step else {
            engine.wake.notified().await;
            continue;
        };
        tokio::time::sleep(Duration::from_micros(step_us as u64)).await;
        engine.core().finish_step(
            |tag, _| {
                let _ = tag.send(Event::Token);
            },
            Tag::is_closed,
        );
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PromptField {
    Text(String),
    /// Token ids, as `llm-bench --token-vocab` sends real engines: four prompt bytes each.
    Ids(Vec<u32>),
}

impl Default for PromptField {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl PromptField {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.into_bytes(),
            Self::Ids(ids) => ids.into_iter().flat_map(u32::to_le_bytes).collect(),
        }
    }
}

#[derive(Deserialize)]
struct CompletionRequest {
    #[serde(default)]
    prompt: PromptField,
    max_tokens: Option<u64>,
    #[serde(default)]
    stream: bool,
    stream_options: Option<StreamOptions>,
    /// Simulation-only hint for prompt-less requests so the random workload needn't send kilobytes of text.
    prompt_tokens: Option<u64>,
    #[serde(default)]
    priority: u64,
}

#[derive(Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: bool,
}

fn usage(prompt: u64, completion: u64, cached: u64) -> serde_json::Value {
    serde_json::json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "prompt_tokens_details": {"cached_tokens": cached},
    })
}

/// Client-side view of a job: its cached-prefix size and tokens emitted so far.
struct Progress {
    events: mpsc::UnboundedReceiver<Event>,
    cached: u64,
    completion: u64,
}

impl Progress {
    async fn next_token(&mut self) -> bool {
        loop {
            match self.events.recv().await {
                Some(Event::Admitted { cached_tokens }) => self.cached = cached_tokens,
                Some(Event::Token) => {
                    self.completion += 1;
                    return true;
                }
                None => return false,
            }
        }
    }
}

async fn complete(State(engine): State<Arc<Engine>>, Json(req): Json<CompletionRequest>) -> Response {
    let text = req.prompt.into_bytes();
    let (hashes, tokens) = engine.core().tokenize(&text);
    let prompt = match text.is_empty() {
        true => req.prompt_tokens.unwrap_or(1).max(1),
        false => tokens,
    };
    let output = req.max_tokens.unwrap_or(16).max(1);
    let (events, rx) = mpsc::unbounded_channel();
    engine.submit(Job::new(hashes, prompt, output, req.priority, events));
    let mut progress = Progress { events: rx, cached: 0, completion: 0 };
    if req.stream {
        const CHUNK: &[u8] = b"data: {\"object\":\"text_completion\",\"choices\":[{\"index\":0,\"text\":\" x\"}]}\n\n";
        let include_usage = req.stream_options.is_some_and(|o| o.include_usage);
        let chunks = stream::unfold(Some(progress), move |state| async move {
            let mut p = state?;
            if p.next_token().await {
                return Some((Ok::<_, Infallible>(Bytes::from_static(CHUNK)), Some(p)));
            }
            let mut tail = String::new();
            if include_usage {
                let body = serde_json::json!({"object": "text_completion", "choices": [],
                    "usage": usage(prompt, p.completion, p.cached)});
                tail = format!("data: {body}\n\n");
            }
            tail.push_str("data: [DONE]\n\n");
            Some((Ok(Bytes::from(tail)), None))
        });
        return ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(chunks)).into_response();
    }
    while progress.next_token().await {}
    let body = serde_json::json!({
        "object": "text_completion",
        "choices": [{"index": 0, "text": " x".repeat(progress.completion as usize)}],
        "usage": usage(prompt, progress.completion, progress.cached),
    });
    Json(body).into_response()
}

pub async fn spawn_engines(specs: &[EngineSpec]) -> Vec<SocketAddr> {
    let mut addrs = Vec::with_capacity(specs.len());
    for &spec in specs {
        let engine = Arc::new(Engine { core: Mutex::new(EngineCore::new(spec)), wake: Notify::new() });
        tokio::spawn(schedule(Arc::clone(&engine)));
        let metrics = {
            let engine = Arc::clone(&engine);
            move || std::future::ready(engine.metrics())
        };
        let app =
            Router::new().route("/v1/completions", post(complete)).with_state(engine).route("/metrics", get(metrics));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind localhost");
        addrs.push(listener.local_addr().expect("bound address"));
        tokio::spawn(async move { axum::serve(listener, app).await.expect("engine server crashed") });
    }
    addrs
}
