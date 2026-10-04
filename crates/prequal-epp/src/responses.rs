//! Builders for the ext_proc `ProcessingResponse` shapes the endpoint-picker protocol uses.

use envoy_types::pb::envoy::{
    config::core::v3::{HeaderValue, HeaderValueOption},
    service::ext_proc::v3::{
        BodyMutation, BodyResponse, CommonResponse, HeaderMutation, HeadersResponse, ImmediateResponse,
        ProcessingResponse, StreamedBodyResponse, TrailersResponse, body_mutation::Mutation,
        processing_response::Response,
    },
    r#type::v3::HttpStatus,
};

use crate::metadata::destination_metadata;

/// Some gateways cap each streamed chunk at 64 KiB; the reference EPP uses 62,000 bytes.
pub const MAX_CHUNK: usize = 62_000;

#[derive(Clone, Copy)]
pub enum Phase {
    Request,
    Response,
}

fn set_headers(headers: &[(&str, &str)]) -> Option<HeaderMutation> {
    (!headers.is_empty()).then(|| HeaderMutation {
        set_headers: headers
            .iter()
            .map(|(key, value)| HeaderValueOption {
                header: Some(HeaderValue {
                    key: (*key).to_owned(),
                    raw_value: value.as_bytes().to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

pub fn headers(phase: Phase, set: &[(&str, &str)]) -> ProcessingResponse {
    let common = CommonResponse { header_mutation: set_headers(set), ..Default::default() };
    let headers = HeadersResponse { response: Some(common) };
    let response = match phase {
        Phase::Request => Response::RequestHeaders(headers),
        Phase::Response => Response::ResponseHeaders(headers),
    };
    ProcessingResponse { response: Some(response), ..Default::default() }
}

/// The deferred request-headers response carrying the pick: header and `envoy.lb` metadata hold
/// the same `ip:port[,fallback...]` value, as the protocol requires. `content_length` is set when the echoed body
/// was rewritten, as the reference EPP does.
pub fn destination(value: &str, content_length: Option<usize>) -> ProcessingResponse {
    let length = content_length.map(|n| n.to_string());
    let mut headers = vec![(crate::metadata::DESTINATION, value)];
    headers.extend(length.as_deref().map(|n| ("content-length", n)));
    let common =
        CommonResponse { header_mutation: set_headers(&headers), clear_route_cache: true, ..Default::default() };
    ProcessingResponse {
        response: Some(Response::RequestHeaders(HeadersResponse { response: Some(common) })),
        dynamic_metadata: Some(destination_metadata(value)),
        ..Default::default()
    }
}

pub fn body_chunk(phase: Phase, body: Vec<u8>, end_of_stream: bool) -> ProcessingResponse {
    let streamed = StreamedBodyResponse { body, end_of_stream, ..Default::default() };
    let mutation = BodyMutation { mutation: Some(Mutation::StreamedResponse(streamed)) };
    let body = BodyResponse { response: Some(CommonResponse { body_mutation: Some(mutation), ..Default::default() }) };
    let response = match phase {
        Phase::Request => Response::RequestBody(body),
        Phase::Response => Response::ResponseBody(body),
    };
    ProcessingResponse { response: Some(response), ..Default::default() }
}

/// Splits a buffered body into protocol-sized streamed chunks; an empty body still yields one
/// chunk so the gateway sees end of stream.
pub fn body_chunks(phase: Phase, body: &[u8]) -> Vec<ProcessingResponse> {
    if body.is_empty() {
        return vec![body_chunk(phase, Vec::new(), true)];
    }
    let chunks: Vec<&[u8]> = body.chunks(MAX_CHUNK).collect();
    let last = chunks.len() - 1;
    chunks.into_iter().enumerate().map(|(i, c)| body_chunk(phase, c.to_vec(), i == last)).collect()
}

pub fn trailers(phase: Phase) -> ProcessingResponse {
    let trailers = TrailersResponse::default();
    let response = match phase {
        Phase::Request => Response::RequestTrailers(trailers),
        Phase::Response => Response::ResponseTrailers(trailers),
    };
    ProcessingResponse { response: Some(response), ..Default::default() }
}

/// An error as llm-d's EPP reports it: an immediate response with body `inference error: <code> - <message>` and,
/// for some, an `x-llm-d-request-dropped-reason` header; `code` is also the `error_code` metric label.
#[derive(Debug, PartialEq, Eq)]
pub struct LlmdError {
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
    pub dropped_reason: Option<&'static str>,
}

pub const DROPPED_REASON: &str = "x-llm-d-request-dropped-reason";

impl LlmdError {
    pub fn response(&self) -> ProcessingResponse {
        let body = format!("inference error: {} - {}", self.code, self.message);
        let headers: &[(&str, &str)] = match self.dropped_reason {
            Some(reason) => &[(DROPPED_REASON, reason)],
            None => &[],
        };
        let immediate = ImmediateResponse {
            status: Some(HttpStatus { code: i32::from(self.status) }),
            headers: set_headers(headers),
            details: body.clone(),
            body: body.into_bytes(),
            ..Default::default()
        };
        ProcessingResponse { response: Some(Response::ImmediateResponse(immediate)), ..Default::default() }
    }
}
