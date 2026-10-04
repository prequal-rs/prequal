//! One ext_proc stream's request handling: one gRPC stream per HTTP request. The request-headers response is
//! deferred until the body is complete (the protocol's streaming mode), then the pick is returned
//! and the body echoed back; on the response path the served endpoint's outcome is recorded.

use std::{sync::Arc, time::Instant};

use envoy_types::pb::envoy::service::ext_proc::v3::{
    ProcessingRequest, ProcessingResponse, processing_request::Request,
};
use prequal_llm::{DEFAULT_MAX_TOKENS, Prompt, Ticket, max_tokens};

use crate::{
    engine_priority::{EnginePriority, Learning},
    limits::Lease,
    metadata::{
        SERVED_MISSING, TEST_SELECTION_HEADER, TEST_SERVED_HEADER, header, header_any, served_endpoint, served_value,
        split_list, subset_hint,
    },
    objectives::OBJECTIVE_HEADERS,
    picker::{Picker, Reject},
    request_metrics::{DEFAULT_FAIRNESS_ID, FAIRNESS_HEADERS, Labels, REQUESTS, Running},
    responses::{self, LlmdError, Phase},
    telemetry::TELEMETRY,
};

/// Same cap as the reference EPP; larger bodies are rejected rather than buffered.
const MAX_BODY: usize = 10 << 20;

const BODY_TOO_LARGE: LlmdError =
    LlmdError { status: 413, code: "BadRequest", message: "request body too large", dropped_reason: None };
const BUFFERS_FULL: LlmdError = LlmdError {
    status: 503,
    code: "ServiceUnavailable",
    message: "endpoint picker request buffers full",
    dropped_reason: None,
};

/// One request's state. Its routing ticket lives as long as the ext_proc stream, which ends with the request.
#[derive(Default)]
pub struct Exchange {
    test_hooks: bool,
    started: Option<Instant>,
    subset: Option<Vec<String>>,
    priority: i32,
    fairness_id: Option<String>,
    body: Vec<u8>,
    buffered: Lease,
    ticket: Option<Ticket>,
    /// Set once routed (or rejected); taken when the response completes so it is recorded once.
    labels: Option<Labels>,
    running: Option<Running>,
    response_bytes: usize,
    engine_priority: Option<Arc<EnginePriority>>,
    learning: Option<Learning>,
}

type Out = Vec<ProcessingResponse>;

impl Exchange {
    pub fn new(test_hooks: bool, buffered: Lease) -> Self {
        Self { test_hooks, started: Some(Instant::now()), buffered, ..Self::default() }
    }

    pub fn with_engine_priority(self, engine_priority: Option<Arc<EnginePriority>>) -> Self {
        Self { engine_priority, ..self }
    }

    fn labels(&self, model: &str) -> Labels {
        REQUESTS.labels(model, self.fairness_id.as_deref().unwrap_or(DEFAULT_FAIRNESS_ID), self.priority)
    }

    /// An immediate error reply ending the stream, counted in `llm_d_epp_request_error_total`.
    fn reject(&mut self, error: &LlmdError, labels: Labels) -> (Out, bool) {
        REQUESTS.error(&labels, error.code);
        (vec![error.response()], true)
    }

    /// Picks and returns the deferred headers response, plus the echoed body if the request had one (a body response
    /// to a bodiless request is a protocol error to Envoy), or an immediate reply.
    async fn route(&mut self, picker: &Picker, with_body: bool) -> (Out, bool) {
        let model = Prompt::model(&self.body).map(String::from_utf8_lossy).unwrap_or_default();
        let labels = self.labels(&model);
        let prompt = Prompt::from_body(&self.body);
        let output = max_tokens(&self.body).unwrap_or(DEFAULT_MAX_TOKENS);
        let started = Instant::now();
        let picked = picker.pick(&prompt, output, self.subset.as_deref(), self.priority).await;
        if picked.as_ref().err() != Some(&Reject::Saturated) {
            TELEMETRY.record_pick(started.elapsed(), picked.is_ok());
        }
        let (ticket, ranked) = match picked {
            Ok(picked) => picked,
            Err(reject) => return self.reject(&reject.error(), labels),
        };
        self.ticket = Some(ticket);
        self.running = REQUESTS.routed(&labels, self.body.len());
        self.labels = Some(labels);
        let value = ranked.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
        let mut body = std::mem::take(&mut self.body);
        self.buffered.release();
        let stamped = self.engine_priority.as_ref().filter(|_| with_body);
        if let Some((stamped, learning)) = stamped.and_then(|p| p.stamp_body(self.priority, &prompt, &body)) {
            (body, self.learning) = (stamped, Some(learning));
        }
        let mut out = vec![responses::destination(&value, self.learning.as_ref().map(|_| body.len()))];
        if with_body {
            out.extend(responses::body_chunks(Phase::Request, &body));
        }
        (out, false)
    }

    /// Records the response's completion once, if the request was routed.
    fn complete(&mut self) {
        if let (Some(labels), Some(started)) = (self.labels.take(), self.started) {
            REQUESTS.completed(&labels, started.elapsed(), self.response_bytes);
        }
    }

    /// Handles one message; returns responses to send and whether the stream is finished.
    pub async fn handle(&mut self, picker: &Picker, message: ProcessingRequest) -> (Out, bool) {
        let metadata = message.metadata_context.as_ref();
        let counted = match &message.request {
            Some(Request::RequestHeaders(_)) => Some((0, 0)),
            Some(Request::RequestBody(b)) => Some((1, b.body.len())),
            Some(Request::RequestTrailers(_)) => Some((2, 0)),
            Some(Request::ResponseHeaders(_)) => Some((3, 0)),
            Some(Request::ResponseBody(b)) => Some((4, b.body.len())),
            Some(Request::ResponseTrailers(_)) => Some((5, 0)),
            None => None,
        };
        if let Some((kind, bytes)) = counted {
            TELEMETRY.record_message(kind, bytes);
        }
        match message.request {
            Some(Request::RequestHeaders(h)) => {
                let headers = h.headers.as_ref();
                let test_subset = match self.test_hooks {
                    true => header(headers, TEST_SELECTION_HEADER).map(|v| split_list(&v)),
                    false => None,
                };
                self.subset = test_subset.or_else(|| subset_hint(metadata));
                self.priority = picker.objectives().priority(header_any(headers, &OBJECTIVE_HEADERS).as_deref());
                self.fairness_id = header_any(headers, &FAIRNESS_HEADERS);
                if h.end_of_stream { self.route(picker, false).await } else { (Vec::new(), false) }
            }
            Some(Request::RequestBody(b)) => {
                if self.body.len() + b.body.len() > MAX_BODY {
                    return self.reject(&BODY_TOO_LARGE, self.labels(""));
                }
                if !self.buffered.grow(b.body.len()) {
                    return self.reject(&BUFFERS_FULL, self.labels(""));
                }
                self.body.extend_from_slice(&b.body);
                if b.end_of_stream { self.route(picker, true).await } else { (Vec::new(), false) }
            }
            Some(Request::ResponseHeaders(h)) => {
                let chosen = self.ticket.as_ref().map(Ticket::addr);
                let served = served_endpoint(metadata).or(chosen);
                let status = header(h.headers.as_ref(), ":status");
                let code: Option<u16> = status.as_deref().and_then(|s| s.parse().ok());
                if status.as_deref() != Some("200") {
                    self.learning = None;
                    if let Some(labels) = &self.labels {
                        REQUESTS.error(labels, "ModelServerError");
                    }
                }
                if served != chosen {
                    // The gateway fell back to another endpoint; the reservation was for the pick.
                    self.ticket = None;
                } else if code.is_some_and(|s| s >= 500)
                    && let Some(ticket) = self.ticket.take()
                {
                    ticket.failed();
                } else if let Some(ticket) = self.ticket.as_mut() {
                    ticket.response_started();
                }
                if h.end_of_stream {
                    self.complete();
                }
                if !self.test_hooks {
                    return (vec![responses::headers(Phase::Response, &[])], false);
                }
                // The gateway's report only, never our pick: the conformance check is that the gateway reports it.
                let reported = served_value(metadata).unwrap_or(SERVED_MISSING);
                (vec![responses::headers(Phase::Response, &[(TEST_SERVED_HEADER, reported)])], false)
            }
            Some(Request::ResponseBody(b)) => {
                if !b.body.is_empty()
                    && let Some(ticket) = self.ticket.as_mut()
                {
                    ticket.first_token();
                }
                self.response_bytes += b.body.len();
                if let Some(learning) = self.learning.as_mut() {
                    learning.feed(&b.body);
                }
                if b.end_of_stream {
                    self.ticket = None;
                    if let Some(learning) = self.learning.take() {
                        learning.finish();
                    }
                    self.complete();
                }
                (vec![responses::body_chunk(Phase::Response, b.body, b.end_of_stream)], false)
            }
            Some(Request::RequestTrailers(_)) => (vec![responses::trailers(Phase::Request)], false),
            Some(Request::ResponseTrailers(_)) => {
                self.complete();
                (vec![responses::trailers(Phase::Response)], false)
            }
            None => (Vec::new(), false),
        }
    }
}

#[cfg(test)]
#[path = "extproc_tests.rs"]
mod tests;
