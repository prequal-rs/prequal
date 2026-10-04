//! The ext_proc gRPC service: admits each stream against the shared limits and drives its [`Exchange`] as the
//! response stream is polled.

use std::{collections::VecDeque, pin::Pin, sync::Arc};

use envoy_types::pb::envoy::service::ext_proc::v3::{
    ProcessingRequest, ProcessingResponse, external_processor_server::ExternalProcessor,
};
use futures_util::stream::{self, Stream};
use tonic::{Code, Status, Streaming};

use crate::{
    engine_priority::EnginePriority,
    extproc::Exchange,
    limits::{Lease, Quota},
    picker::Picker,
    telemetry::TELEMETRY,
};

pub struct Epp {
    picker: Arc<Picker>,
    test_hooks: bool,
    streams: Arc<Quota>,
    bodies: Arc<Quota>,
    engine_priority: Option<Arc<EnginePriority>>,
}

impl Epp {
    /// `test_hooks` enables the GIE conformance hooks: the `test-epp-endpoint-selection` request header restricts
    /// candidates, and responses carry `x-conformance-test-served-endpoint`. Never enable it on production traffic.
    pub fn new(picker: Arc<Picker>, test_hooks: bool) -> Self {
        Self { picker, test_hooks, streams: Quota::new(0), bodies: Quota::new(0), engine_priority: None }
    }

    /// Caps open streams at `streams` units and buffered request bytes at `bodies`, both shared with other `Epp`s.
    pub fn with_limits(self, streams: Arc<Quota>, bodies: Arc<Quota>) -> Self {
        Self { streams, bodies, ..self }
    }

    /// Stamps request bodies with an engine priority (see `engine_priority`), shared with other `Epp`s.
    pub fn with_engine_priority(self, engine_priority: Option<Arc<EnginePriority>>) -> Self {
        Self { engine_priority, ..self }
    }
}

#[tonic::async_trait]
impl ExternalProcessor for Epp {
    type ProcessStream = Pin<Box<dyn Stream<Item = Result<ProcessingResponse, Status>> + Send>>;

    async fn process(
        &self,
        request: tonic::Request<Streaming<ProcessingRequest>>,
    ) -> Result<tonic::Response<Self::ProcessStream>, Status> {
        let mut permit = self.streams.lease();
        if !permit.grow(1) {
            TELEMETRY.record_stream(Code::ResourceExhausted);
            return Err(Status::resource_exhausted("prequal-epp: too many concurrent ext_proc streams"));
        }
        // Handled inline as the response stream is polled: a spawned task feeding a channel adds wakeups per
        // message, which is most of the cost at one message per token (see `shards`).
        let session = Session {
            inbound: request.into_inner(),
            picker: Arc::clone(&self.picker),
            exchange: Exchange::new(self.test_hooks, self.bodies.lease())
                .with_engine_priority(self.engine_priority.clone()),
            pending: VecDeque::new(),
            done: false,
            outcome: Code::Cancelled,
            _permit: permit,
        };
        Ok(tonic::Response::new(Box::pin(stream::unfold(session, Session::next))))
    }
}

struct Session {
    inbound: Streaming<ProcessingRequest>,
    picker: Arc<Picker>,
    exchange: Exchange,
    pending: VecDeque<ProcessingResponse>,
    done: bool,
    /// How the stream ended, for `llm_d_epp_extproc_streams_total`: dropped before finishing means Envoy cancelled.
    outcome: Code,
    _permit: Lease,
}

impl Session {
    async fn next(mut self) -> Option<(Result<ProcessingResponse, Status>, Self)> {
        loop {
            if let Some(response) = self.pending.pop_front() {
                return Some((Ok(response), self));
            }
            if self.done {
                self.outcome = Code::Ok;
                return None;
            }
            let message = match self.inbound.message().await {
                Ok(Some(message)) => message,
                // EOF and cancellation end the stream normally.
                Ok(None) => {
                    self.outcome = Code::Ok;
                    return None;
                }
                Err(status) => {
                    self.outcome = status.code();
                    return None;
                }
            };
            let (out, done) = self.exchange.handle(&self.picker, message).await;
            self.pending.extend(out);
            self.done = done;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        TELEMETRY.record_stream(self.outcome);
    }
}
