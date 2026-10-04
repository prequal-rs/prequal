# Gateway API Inference Extension conformance

`prequal-epp` passes all 14 tests of the [Gateway API Inference Extension][gie] v1.6.2 conformance suite when it
replaces the suite's reference endpoint picker behind **agentgateway v1.5.0**. The reference picker passes the same
14 on the same gateway. Behind **Istio 1.31.1** it does not work at all, because of an Istio bug described
[below](#istio).

## What the suite tests, and what this run shows

The suite (`conformance/` in the GIE repository) has a single profile, **Gateway**: it certifies gateway
implementations. The EPP and model-server profiles that the project's docs mention do not exist yet, so no endpoint
picker can be conformance-certified, and none has a report under `conformance/reports/`.

The suite exercises the picker anyway. Its base manifests deploy five copies of a reference picker (`lwepp`), one per
`InferencePool`: primary, fail-open secondary, `http` and `h2c` app-protocol pools, and a data-parallel pool with
three target ports per pod. Its tests steer requests with two test hooks that a picker has to implement:

- the `test-epp-endpoint-selection: ip[,ip:port...]` request header restricts the candidate endpoints;
- the picker copies the gateway's `envoy.lb` / `x-gateway-destination-endpoint-served` response metadata into the
  `x-conformance-test-served-endpoint` response header.

In `prequal-epp` both hooks are behind `--conformance-test-hooks`, which is off by default because any client could
use the header to steer its own requests. The served-endpoint header only ever reports the gateway's metadata,
never `prequal-epp`'s own pick, so a gateway that doesn't report the served endpoint still fails.

This run swaps `prequal-epp` into all five picker Deployments. The only changes are the image and
`--conformance-test-hooks`; the suite's arguments, ports, probes and RBAC are unchanged. Every test then runs
through `prequal-epp`. A pass shows that it interoperates with that gateway on everything the suite exercises:

- InferencePool and Pod discovery, including several target ports per pod (data parallelism);
- endpoint subsets in `ip` and `ip:port` form;
- ext_proc over TLS, with `FULL_DUPLEX_STREAMED` request bodies and the destination header plus `envoy.lb` metadata;
- served-endpoint metadata, the gRPC health service, fail-open when the picker is down, and weighted splits across
  two pools.

**What can be claimed:** "passes the GIE v1.6.2 conformance suite (Gateway profile, 14/14) as the endpoint picker
of every InferencePool, behind agentgateway v1.5.0".

**What can't:** "GIE-conformant" or "certified". There is no EPP profile, and the report's `implementation` block
describes a gateway, so the report can't be submitted upstream. The suite also doesn't test routing quality, and it
doesn't test `prequal-epp`'s default configuration: it needs the test hooks, and the backends are echo servers
without engine metrics.

## Results

GIE v1.6.2 conformance suite, Gateway API v1.6.1 (standard channel), kind (Kubernetes v1.37.0) on one Linux host,
2026-10-01. Reports and per-test logs: [`results/conformance/`](../results/conformance).

| Test | agentgateway v1.5.0 + lwepp | agentgateway v1.5.0 + prequal-epp | Istio 1.31.1 + lwepp | Istio 1.31.1 + prequal-epp |
|---|---|---|---|---|
| EppUnAvailableFailOpen | pass | pass | pass | fail |
| GatewayDestinationEndpointServed | pass | pass | pass | fail |
| GatewayFollowingEPPRouting | pass | pass | pass | fail |
| GatewayFollowingEPPRoutingWithDataParallelism | pass | pass | pass | fail |
| GatewayWeightedAcrossTwoInferencePools | pass | pass | **fail** (Istio) | fail |
| HTTPRouteInvalidInferencePoolRef | pass | pass | pass | pass |
| HTTPRouteMultipleGatewaysDifferentPools | pass | pass | pass | pass |
| HTTPRouteMultipleRulesDifferentPools | pass | pass | pass | not run |
| InferencePoolAccepted | pass | pass | pass | pass |
| InferencePoolAppProtocol | pass | pass | pass | fail |
| InferencePoolHTTPRoutePortValidation | pass | pass | pass | fail |
| InferencePoolInvalidEPPService | pass | pass | pass | not run |
| InferencePoolMissingEPPRef | pass | pass | pass | not run |
| InferencePoolResolvedRefsCondition | pass | pass | pass | not run |
| **Profile result** | **success, 14/14** | **success, 14/14** | failure, 13/14 | (run stopped) |

Every Istio + prequal-epp failure has the same cause, and the run was stopped once that was established, so it has no
report. Its per-test failed-request counts are in `results/conformance/istio/prequal-summary.txt`.

### Istio

Istio 1.31.1 fails with both pickers, for different reasons.

- **Weighted two-pool routes (an Istio bug that affects every picker).** When a route splits traffic across two
  InferencePools, Istio sends every request to the second pool's picker. That picker cannot place requests meant
  for the first pool (`no endpoints available`), so the test fails with lwepp too.
- **prequal-epp never receives a request.** Istio builds each pool's ext_proc gRPC service with only a cluster name
  (`pilot/pkg/networking/core/route/route.go`, `buildExtProcPerRoute`, also on Istio master as of 2026-10-01). Envoy
  then uses that cluster name, `outbound|9002||<epp-service-fqdn>`, as the stream's `:authority` and, when a
  DestinationRule enables TLS, as the TLS SNI (Istio turns on `auto_sni`). `|` is not legal in either.
  - Go's gRPC and TLS stacks accept both values, so lwepp and llm-d's picker work.
  - rustls rejects the SNI (`illegal_parameter`). The Rust `h2` server under tonic resets the stream as a malformed
    authority (`protocol_error`), in plaintext too.
  - Neither library can be configured to accept these values.

  On a single request through the same route, lwepp returns 200 with the correct served endpoint, while
  `prequal-epp`'s counters show zero ext_proc messages and zero picks. An explicit `sni:` in the DestinationRule
  fixes the TLS half. The authority half needs Istio to set `envoy_grpc.authority`, for example to the picker
  Service's FQDN; that is a one-line change in Istio.

agentgateway dials the picker with its Service hostname and sets up TLS to it by itself. llm-d's standalone chart,
which is Envoy with a static config, sends a valid authority too: it is what the [benchmarks](benchmarks.md) ran on.

## Reproduce

[`tools/gie-conformance.sh`](../tools/gie-conformance.sh) runs on a Linux host with docker, kind, kubectl, helm, go
and git. It creates one kind cluster and refuses to start if another kind cluster exists. `run` holds
`~/bench.lock`.

```sh
tools/gie-conformance.sh setup          # kind, Gateway API + GIE CRDs, MetalLB, agentgateway, prequal-epp image
tools/gie-conformance.sh run lwepp      # control: the suite as published
tools/gie-conformance.sh run prequal    # prequal-epp as all five pickers
tools/gie-conformance.sh report         # per-test results and the report's profile summary
tools/gie-conformance.sh teardown
```

- **Gateway.** `GATEWAY=istio` installs Istio instead, with TLS DestinationRules for the pickers. Both gateways can
  live in the same cluster, but each run uses one.
- **Versions.** `GIE_VERSION`, `ISTIO_VERSION` and `AGENTGATEWAY_VERSION` override the pinned versions.
- **Extra arguments.** Anything after the arm is passed to the suite, for example `-run-test GatewayFollowingEPPRouting`
  or `-cleanup-base-resources=false` to keep the namespaces for inspection.
- **Output.** Reports and logs go to `~/gie-conformance/reports/<gateway>/`.

A run takes about 5 minutes; the weighted two-pool test accounts for most of it.

[gie]: https://github.com/kubernetes-sigs/gateway-api-inference-extension
