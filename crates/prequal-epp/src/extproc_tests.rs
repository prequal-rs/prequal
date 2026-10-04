use std::{collections::HashMap, net::SocketAddr, time::Duration};

use envoy_types::pb::{
    envoy::{
        config::core::v3::{HeaderMap, HeaderValue, Metadata},
        service::ext_proc::v3::{
            HttpBody, HttpHeaders, ImmediateResponse, body_mutation::Mutation,
            external_processor_client::ExternalProcessorClient, external_processor_server::ExternalProcessorServer,
            processing_response::Response,
        },
    },
    google::protobuf::{Struct, Value, value::Kind},
};
use prequal_llm::{Scheduler, policy};

use super::*;
use crate::{
    limits::Quota,
    metadata::{DESTINATION, LB_NAMESPACE, SERVED},
    responses::DROPPED_REASON,
    service::Epp,
};

fn picker(endpoints: &[&str]) -> Picker {
    let picker = Picker::new(Scheduler::new(policy::by_name("prequal").unwrap()), 1, Duration::from_millis(50));
    picker.sync(&endpoints.iter().map(|e| (e.parse().unwrap(), (*e).to_owned())).collect());
    picker
}

fn headers(pairs: &[(&str, &str)], end_of_stream: bool) -> Request {
    let headers = pairs.iter().map(|(k, v)| HeaderValue { key: (*k).into(), value: (*v).into(), ..Default::default() });
    Request::RequestHeaders(HttpHeaders {
        headers: Some(HeaderMap { headers: headers.collect() }),
        end_of_stream,
        ..Default::default()
    })
}

fn body(bytes: &[u8], end_of_stream: bool) -> ProcessingRequest {
    msg(Request::RequestBody(HttpBody { body: bytes.to_vec(), end_of_stream, ..Default::default() }), None)
}

fn msg(request: Request, metadata: Option<Metadata>) -> ProcessingRequest {
    ProcessingRequest { request: Some(request), metadata_context: metadata, ..Default::default() }
}

fn lb_metadata(namespace: &str, key: &str, value: &str) -> Metadata {
    let inner = Struct { fields: HashMap::from([(key.into(), Value { kind: Some(Kind::StringValue(value.into())) })]) };
    Metadata { filter_metadata: HashMap::from([(namespace.into(), inner)]), ..Default::default() }
}

/// The destination header value and the `envoy.lb` metadata value of a routing response.
fn destination_of(response: &ProcessingResponse) -> (String, String) {
    let Some(Response::RequestHeaders(h)) = &response.response else { panic!("not a headers response") };
    let set = &h.response.as_ref().unwrap().header_mutation.as_ref().unwrap().set_headers[0];
    let header = set.header.as_ref().unwrap();
    assert_eq!(header.key, DESTINATION);
    let Some(Kind::StructValue(lb)) = &response.dynamic_metadata.as_ref().unwrap().fields[LB_NAMESPACE].kind else {
        panic!("no envoy.lb metadata")
    };
    let Some(Kind::StringValue(meta)) = &lb.fields[DESTINATION].kind else { panic!("no destination metadata") };
    (String::from_utf8(header.raw_value.clone()).unwrap(), meta.clone())
}

fn immediate(out: &[ProcessingResponse]) -> &ImmediateResponse {
    match out {
        [ProcessingResponse { response: Some(Response::ImmediateResponse(i)), .. }] => i,
        other => panic!("expected one immediate response, got {other:?}"),
    }
}

#[tokio::test]
async fn defers_headers_until_body_end_then_routes_and_echoes() {
    let picker = picker(&["10.0.0.1:8000", "10.0.0.2:8000"]);
    let mut exchange = Exchange::default();
    assert_eq!(exchange.handle(&picker, msg(headers(&[], false), None)).await.0.len(), 0);
    let payload = vec![b'x'; 70_000];
    assert!(exchange.handle(&picker, body(&payload[..30_000], false)).await.0.is_empty());
    let (out, done) = exchange.handle(&picker, body(&payload[30_000..], true)).await;
    assert!(!done);
    assert_eq!(out.len(), 3, "headers + two body chunks (62,000 + 8,000 bytes)");
    let (header, meta) = destination_of(&out[0]);
    assert_eq!(header, meta, "header and metadata must match");
    assert_eq!(header.split(',').count(), 2, "pick plus one fallback");
    let echoed: usize = out[1..]
        .iter()
        .map(|r| match &r.response {
            Some(Response::RequestBody(b)) => {
                match &b.response.as_ref().unwrap().body_mutation.as_ref().unwrap().mutation {
                    Some(Mutation::StreamedResponse(s)) => s.body.len(),
                    _ => 0,
                }
            }
            _ => 0,
        })
        .sum();
    assert_eq!(echoed, payload.len());
}

#[tokio::test]
async fn a_bodiless_request_gets_only_the_headers_response() {
    // Envoy treats a body response to a request without a body as spurious (fail-closed: a 500).
    let picker = picker(&["10.0.0.1:8000"]);
    let (out, done) = Exchange::default().handle(&picker, msg(headers(&[], true), None)).await;
    assert!(!done);
    assert_eq!(out.len(), 1);
    assert_eq!(destination_of(&out[0]).0, "10.0.0.1:8000");
}

#[tokio::test]
async fn honors_subset_hint_and_test_header() {
    let picker = picker(&["10.0.0.1:8000", "10.0.0.2:8000", "10.0.0.3:8000"]);
    let subset = lb_metadata("envoy.lb.subset_hint", "x-gateway-destination-endpoint-subset", "10.0.0.3:8000");
    let (out, _) = Exchange::default().handle(&picker, msg(headers(&[], true), Some(subset.clone()))).await;
    assert_eq!(destination_of(&out[0]).0, "10.0.0.3:8000");

    // The test header overrides the gateway's hint with hooks on (as in lwepp) and is ignored with them off.
    let selection = || msg(headers(&[(TEST_SELECTION_HEADER, "10.0.0.2")], true), Some(subset.clone()));
    let (out, _) = Exchange::new(true, Lease::default()).handle(&picker, selection()).await;
    assert_eq!(destination_of(&out[0]).0, "10.0.0.2:8000");
    let (out, _) = Exchange::default().handle(&picker, selection()).await;
    assert_eq!(destination_of(&out[0]).0, "10.0.0.3:8000");

    let unknown = lb_metadata("envoy.lb.subset_hint", "x-gateway-destination-endpoint-subset", "10.9.9.9:1");
    let (out, done) = Exchange::default().handle(&picker, msg(headers(&[], true), Some(unknown))).await;
    assert!(done);
    let reply = immediate(&out);
    assert_eq!(reply.status.unwrap().code, 503);
    assert_eq!(
        reply.body,
        b"inference error: ServiceUnavailable - failed to find endpoint candidates for serving the request"
    );
    let set = &reply.headers.as_ref().unwrap().set_headers[0].header.as_ref().unwrap();
    assert_eq!((set.key.as_str(), set.raw_value.as_slice()), (DROPPED_REASON, &b"rejected-no-endpoints"[..]));
}

#[tokio::test]
async fn sheds_sheddable_objectives_with_429_when_saturated() {
    let picker = picker(&["10.0.0.1:8000"]);
    picker.objectives().replace(HashMap::from([("batch".to_owned(), -1)]));
    let request = |objective| msg(headers(&[(OBJECTIVE_HEADERS[1], objective)], true), None);
    // Never scraped, so saturated: the sheddable objective is shed, the unknown one (priority 0) routed.
    let (out, done) = Exchange::default().handle(&picker, request("batch")).await;
    assert!(done);
    let reply = immediate(&out);
    assert_eq!(reply.status.unwrap().code, 429);
    assert_eq!(reply.body, b"inference error: ResourceExhausted - system saturated, sheddable request dropped");
    assert!(reply.headers.is_none(), "llm-d's legacy admission sets no dropped-reason header");
    let (out, _) = Exchange::default().handle(&picker, request("interactive")).await;
    assert_eq!(destination_of(&out[0]).0, "10.0.0.1:8000");
}

#[tokio::test]
async fn caps_each_body_and_all_buffered_bytes() {
    let picker = picker(&["10.0.0.1:8000"]);
    let mut exchange = Exchange::default();
    assert!(exchange.handle(&picker, body(&vec![b'x'; MAX_BODY], false)).await.0.is_empty());
    let (out, done) = exchange.handle(&picker, body(b"x", true)).await;
    assert!(done);
    assert_eq!(immediate(&out).status.unwrap().code, 413);

    let budget = Quota::new(100);
    let mut first = Exchange::new(false, budget.lease());
    assert!(first.handle(&picker, body(&[b'x'; 80], false)).await.0.is_empty());
    let (out, done) = Exchange::new(false, budget.lease()).handle(&picker, body(&[b'x'; 30], true)).await;
    assert!(done);
    assert_eq!(immediate(&out).status.unwrap().code, 503, "budget shared across streams");
    let (out, _) = first.handle(&picker, body(&[b'x'; 10], true)).await;
    assert_eq!(out.len(), 2, "routed and echoed");
    assert_eq!(budget.used(), 0, "released once routed");
}

/// The `x-conformance-test-served-endpoint` value set on the response, if any.
async fn served_header(test_hooks: bool, metadata: Option<Metadata>) -> Option<Vec<u8>> {
    let picker = picker(&["10.0.0.1:8000"]);
    let mut exchange = Exchange::new(test_hooks, Lease::default());
    exchange.handle(&picker, msg(headers(&[], true), None)).await;
    let response_headers = match headers(&[(":status", "200")], false) {
        Request::RequestHeaders(h) => Request::ResponseHeaders(h),
        _ => unreachable!(),
    };
    let (out, _) = exchange.handle(&picker, msg(response_headers, metadata)).await;
    let Some(Response::ResponseHeaders(h)) = &out[0].response else { panic!("not a response-headers reply") };
    let set = h.response.as_ref()?.header_mutation.as_ref()?.set_headers.first()?;
    assert_eq!(set.header.as_ref()?.key, TEST_SERVED_HEADER);
    Some(set.header.as_ref()?.raw_value.clone())
}

#[tokio::test]
async fn reports_only_the_gateway_served_endpoint_with_test_hooks() {
    let served = || Some(lb_metadata(LB_NAMESPACE, SERVED, "10.0.0.1:8000"));
    assert_eq!(served_header(true, served()).await.unwrap(), b"10.0.0.1:8000");
    assert_eq!(served_header(true, None).await.unwrap(), SERVED_MISSING.as_bytes(), "never falls back to our pick");
    assert_eq!(served_header(false, served()).await, None);
}

async fn serve(epp: Epp) -> ExternalProcessorClient<tonic::transport::Channel> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(ExternalProcessorServer::new(epp))
            .serve_with_incoming(incoming),
    );
    ExternalProcessorClient::connect(format!("http://{addr}")).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_round_trip() {
    let mut client = serve(Epp::new(Arc::new(picker(&["10.0.0.7:8000"])), false)).await;
    let requests = tokio_stream::iter([msg(headers(&[], true), None)]);
    let mut responses = client.process(requests).await.unwrap().into_inner();
    let first = responses.message().await.unwrap().unwrap();
    assert_eq!(destination_of(&first).0, "10.0.0.7:8000");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_streams_over_the_cap_until_one_closes() {
    let streams = Quota::new(1);
    let epp = Epp::new(Arc::new(picker(&["10.0.0.7:8000"])), false).with_limits(Arc::clone(&streams), Quota::new(0));
    let mut client = serve(epp).await;
    let (open_tx, open_rx) = tokio::sync::mpsc::unbounded_channel();
    open_tx.send(msg(headers(&[], true), None)).unwrap();
    let mut open = client.process(tokio_stream::wrappers::UnboundedReceiverStream::new(open_rx)).await.unwrap();
    open.get_mut().message().await.unwrap().unwrap();
    assert_eq!(streams.used(), 1);
    let refused = client.process(tokio_stream::iter([msg(headers(&[], true), None)])).await.unwrap_err();
    assert_eq!(refused.code(), tonic::Code::ResourceExhausted);
    drop((open, open_tx));
    tokio::time::timeout(Duration::from_secs(5), async {
        while streams.used() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the closed stream's permit returns");
    assert!(client.process(tokio_stream::iter([msg(headers(&[], true), None)])).await.is_ok());
}

#[tokio::test]
async fn stamps_engine_priority_and_learns_from_the_response() {
    let picker = picker(&["10.0.0.1:8000"]);
    let engine_priority = Some(Arc::new(EnginePriority::new(Duration::from_secs(30))));
    let mut exchange = Exchange { engine_priority, ..Exchange::default() };
    exchange.handle(&picker, msg(headers(&[], false), None)).await;
    let request = br#"{"model":"m","prompt":"hi","stream":true}"#;
    let (out, _) = exchange.handle(&picker, body(request, true)).await;
    let Some(Response::RequestHeaders(h)) = &out[0].response else { panic!("not a headers response") };
    let set = &h.response.as_ref().unwrap().header_mutation.as_ref().unwrap().set_headers;
    let length = set.iter().find_map(|s| s.header.as_ref().filter(|h| h.key == "content-length")).unwrap();
    let Some(Response::RequestBody(b)) = &out[1].response else { panic!("not a body response") };
    let Some(Mutation::StreamedResponse(echoed)) =
        &b.response.as_ref().unwrap().body_mutation.as_ref().unwrap().mutation
    else {
        panic!("not a streamed body")
    };
    assert!(echoed.body.starts_with(&request[..request.len() - 1]));
    assert!(echoed.body[request.len() - 1..].starts_with(b",\"priority\":") && echoed.body.ends_with(b"}"));
    assert_eq!(length.raw_value, echoed.body.len().to_string().into_bytes());

    let status = HeaderValue { key: ":status".into(), value: "200".into(), ..Default::default() };
    let response_headers = HttpHeaders { headers: Some(HeaderMap { headers: vec![status] }), ..Default::default() };
    exchange.handle(&picker, msg(Request::ResponseHeaders(response_headers), None)).await;
    let chunk = |bytes: &[u8], end_of_stream| {
        msg(Request::ResponseBody(HttpBody { body: bytes.to_vec(), end_of_stream, ..Default::default() }), None)
    };
    exchange.handle(&picker, chunk(b"data: {\"choices\":[]}\n\n", false)).await;
    assert!(exchange.learning.is_some());
    exchange.handle(&picker, chunk(b"data: [DONE]\n\n", true)).await;
    assert!(exchange.learning.is_none(), "learned at end of stream");
}

#[tokio::test]
async fn without_engine_priority_the_body_is_echoed_unchanged() {
    let picker = picker(&["10.0.0.1:8000"]);
    let mut exchange = Exchange::default();
    exchange.handle(&picker, msg(headers(&[], false), None)).await;
    let (out, _) = exchange.handle(&picker, body(br#"{"prompt":"hi"}"#, true)).await;
    let Some(Response::RequestHeaders(h)) = &out[0].response else { panic!("not a headers response") };
    assert_eq!(h.response.as_ref().unwrap().header_mutation.as_ref().unwrap().set_headers.len(), 1);
}
