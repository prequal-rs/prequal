//! Envoy's side of one ext_proc stream per request, as the llm-d standalone chart configures it (FULL_DUPLEX_STREAMED
//! request and response bodies): request headers, the body in read-sized chunks, then — once routed — response
//! headers and one SSE body chunk per streamed token.

use std::time::{Duration, Instant};

use envoy_types::pb::envoy::{
    config::core::v3::{HeaderMap, HeaderValue},
    service::ext_proc::v3::{
        HttpBody, HttpHeaders, ProcessingRequest, ProcessingResponse, body_mutation::Mutation,
        external_processor_client::ExternalProcessorClient, processing_request::Request, processing_response::Response,
    },
};
use rand::{RngExt, SeedableRng, rngs::SmallRng};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

/// inference-perf `shared_prefix`: `groups` system prompts, each followed by one of `questions` per group.
pub struct Workload {
    systems: Vec<String>,
    questions: Vec<Vec<String>>,
    pub output_tokens: usize,
}

const WORDS: [&str; 16] = [
    "the", "model", "cache", "prefix", "token", "route", "queue", "server", "latency", "batch", "decode", "prompt",
    "system", "answer", "shared", "request",
];

fn text(rng: &mut SmallRng, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 16);
    while s.len() < bytes {
        s.push_str(WORDS[rng.random_range(0..WORDS.len())]);
        s.push(' ');
    }
    s
}

impl Workload {
    pub fn new(groups: usize, questions: usize, system_tokens: usize, question_tokens: usize, output: usize) -> Self {
        let mut rng = SmallRng::seed_from_u64(7);
        let systems = (0..groups).map(|_| text(&mut rng, system_tokens * 4)).collect();
        let questions =
            (0..groups).map(|_| (0..questions).map(|_| text(&mut rng, question_tokens * 4)).collect()).collect();
        Self { systems, questions, output_tokens: output }
    }

    pub fn body(&self, rng: &mut SmallRng) -> Vec<u8> {
        let g = rng.random_range(0..self.systems.len());
        let q = &self.questions[g][rng.random_range(0..self.questions[g].len())];
        format!(
            r#"{{"model":"Qwen/Qwen3-8B","prompt":"{}{q}","max_tokens":{},"stream":true,"ignore_eos":true}}"#,
            self.systems[g], self.output_tokens
        )
        .into_bytes()
    }
}

pub struct Outcome {
    pub routed: bool,
    pub pick: Duration,
    pub sent: usize,
    pub received: usize,
    /// Per streamed token: microseconds from handing the chunk to the gRPC stream until its echo came back.
    pub echo_us: Vec<u32>,
}

/// Each SSE chunk's id carries its send time (ns since `t0`) in hex, so the echo's latency can be read back.
fn echo_latency(response: &ProcessingResponse, t0: Instant) -> Option<u32> {
    let Some(Response::ResponseBody(b)) = &response.response else { return None };
    let Some(Mutation::StreamedResponse(s)) = b.response.as_ref()?.body_mutation.as_ref()?.mutation.as_ref() else {
        return None;
    };
    let at = s.body.windows(5).position(|w| w == b"cmpl-")? + 5;
    let sent = u64::from_str_radix(std::str::from_utf8(s.body.get(at..at + 16)?).ok()?, 16).ok()?;
    Some(((t0.elapsed().as_nanos() as u64).saturating_sub(sent) / 1000) as u32)
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let headers = pairs.iter().map(|(k, v)| HeaderValue {
        key: (*k).to_owned(),
        raw_value: v.as_bytes().to_vec(),
        ..Default::default()
    });
    HeaderMap { headers: headers.collect() }
}

fn msg(request: Request) -> ProcessingRequest {
    ProcessingRequest { request: Some(request), ..Default::default() }
}

fn body(bytes: &[u8], end_of_stream: bool) -> HttpBody {
    HttpBody { body: bytes.to_vec(), end_of_stream, ..Default::default() }
}

#[derive(PartialEq)]
enum Ended {
    Request,
    Response,
}

/// Which body, if any, this echo finishes.
fn ends_body(response: &ProcessingResponse) -> Option<Ended> {
    let (b, ended) = match &response.response {
        Some(Response::RequestBody(b)) => (b, Ended::Request),
        Some(Response::ResponseBody(b)) => (b, Ended::Response),
        _ => return None,
    };
    let mutation = b.response.as_ref().and_then(|c| c.body_mutation.as_ref()).and_then(|m| m.mutation.as_ref());
    matches!(mutation, Some(Mutation::StreamedResponse(s)) if s.end_of_stream).then_some(ended)
}

/// Runs one request's ext_proc stream to completion.
pub async fn run(
    channel: Channel,
    request_body: Vec<u8>,
    chunk: usize,
    output_tokens: usize,
    itl: Duration,
) -> Outcome {
    let (tx, rx) = mpsc::channel(64);
    let (routed_tx, routed_rx) = oneshot::channel::<()>();
    let mut outcome = Outcome { routed: false, pick: Duration::ZERO, sent: 0, received: 0, echo_us: Vec::new() };
    let t0 = Instant::now();
    let writer = tokio::spawn(async move {
        let mut sent = 0;
        let request_headers = headers(&[
            (":method", "POST"),
            (":path", "/v1/completions"),
            ("content-type", "application/json"),
            ("content-length", &request_body.len().to_string()),
        ]);
        let h = HttpHeaders { headers: Some(request_headers), end_of_stream: false, ..Default::default() };
        let mut out = vec![msg(Request::RequestHeaders(h))];
        let chunks: Vec<&[u8]> = request_body.chunks(chunk).collect();
        let last = chunks.len() - 1;
        out.extend(chunks.iter().enumerate().map(|(i, c)| msg(Request::RequestBody(body(c, i == last)))));
        for m in out {
            sent += 1;
            if tx.send(m).await.is_err() {
                return sent;
            }
        }
        if routed_rx.await.is_err() {
            return sent;
        }
        let response_headers = headers(&[(":status", "200"), ("content-type", "text/event-stream")]);
        let h = HttpHeaders { headers: Some(response_headers), end_of_stream: false, ..Default::default() };
        let _ = tx.send(msg(Request::ResponseHeaders(h))).await;
        sent += 1;
        let mut ticker = tokio::time::interval(itl);
        for i in 0..output_tokens {
            ticker.tick().await;
            let sse = format!(
                "data: {{\"id\":\"cmpl-{:016x}\",\"created\":1727700000,\"model\":\"Qwen/Qwen3-8B\",\"choices\":[{{\"index\":0,\"finish_reason\":null,\"text\":\" tok{i}\"}}],\"object\":\"text_completion\"}}\n\n",
                t0.elapsed().as_nanos()
            );
            let _ = tx.send(msg(Request::ResponseBody(body(sse.as_bytes(), false)))).await;
            sent += 1;
        }
        let _ = tx.send(msg(Request::ResponseBody(body(b"data: [DONE]\n\n", true)))).await;
        sent + 1
    });

    let started = Instant::now();
    let mut client = ExternalProcessorClient::new(channel);
    let Ok(response) = client.process(ReceiverStream::new(rx)).await else {
        writer.abort();
        return outcome;
    };
    let mut inbound = response.into_inner();
    let mut routed_tx = Some(routed_tx);
    while let Ok(Some(response)) = inbound.message().await {
        outcome.received += 1;
        outcome.echo_us.extend(echo_latency(&response, t0));
        match &response.response {
            Some(Response::RequestHeaders(_)) => {
                outcome.routed = true;
                outcome.pick = started.elapsed();
            }
            Some(Response::ImmediateResponse(_)) => break,
            _ => {}
        }
        match ends_body(&response) {
            Some(Ended::Request) => {
                if let Some(tx) = routed_tx.take() {
                    let _ = tx.send(());
                }
            }
            Some(Ended::Response) => break,
            None => {}
        }
    }
    drop(routed_tx);
    outcome.sent = writer.await.unwrap_or(0);
    outcome
}
