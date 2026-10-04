# Sharing placements between routers

When several routers share a fleet, each one's prefix index holds only its own placements, so it sends prompts another
router already warmed somewhere to a cold replica. This page measures how much that costs, how much perfect knowledge
of the engines' caches would recover (the gate for [roadmap](roadmap.md) project 4, KV events), and whether routers
telling each other where they sent each prompt recovers the same without any engine feature.

All numbers are from the virtual-time simulator (`llm-bench --virtual`), policy `prequal`, mean of five seeds. Nothing
here has run on real engines, and the gossip mechanism exists only in the simulator.

## Arms

- **Today**: each router knows only its own placements.
- **Oracle** (`--oracle-index events`): every routing decision sees what each engine's cache holds at that instant,
  which is the most an engine KV-event feed could deliver. `perfect` (also counting prompts still queued in an engine)
  and `hybrid` (the larger of the oracle and the router's own index) land within a point of it.
- **Gossip** (`--gossip-ms N`): `N` ms after routing a prompt, a router tells the others which replica it went to. They
  add it to their own index and count it toward sizing their cache model. `--gossip-loss` drops a share of messages.

## Results

Shared-prefix workload, 8 modelled H100 engines (`shared`): hit rate at 10 and 20 req/s, TTFT p90 at 20 req/s.

| Routers | Today | Gossip, 100 ms | Oracle |
|---|---|---|---|
| 1 | 99.8% / 99.8% / 50 ms | same as today | 99.6% / 99.1% / 52 ms |
| 2 | 91.4% / 85.2% / 3,006 ms | 98.7% / 96.5% / 111 ms | 98.7% / 96.8% / 96 ms |
| 3 | 84.2% / 76.9% / 9,225 ms | 98.8% / 96.5% / 123 ms | 98.9% / 97.2% / 107 ms |
| 4 | 79.3% / 72.2% / 11,896 ms | 98.5% / 96.5% / 123 ms | 98.9% / 96.3% / 111 ms |

Cache-pressure workload (`cache`, the simulator's model of §2 of [benchmarks](benchmarks.md)): hit rate at 12 req/s.
TTFT does not move here, because this engine model's prefill is nearly free.

| Routers | Today | Gossip, 100 ms | Oracle |
|---|---|---|---|
| 1 | 66.6% | 66.6% | 66.8% |
| 2 | 61.0% | 66.2% | 66.0% |
| 3 | 59.5% | 66.1% | 66.2% |
| 4 | 59.5% | 65.7% | 66.3% |

Delay and loss, `shared` at 20 req/s: hit rate / TTFT p90.

| Gossip | 2 routers | 4 routers |
|---|---|---|
| 100 ms | 96.5% / 111 ms | 96.5% / 123 ms |
| 1 s | 96.4% / 115 ms | 95.6% / 130 ms |
| 5 s | 94.6% / 179 ms | 92.2% / 1,014 ms |
| 100 ms, 20% lost | 95.3% / 128 ms | 95.3% / 145 ms |
| 100 ms, 50% lost | 93.3% / 231 ms | 96.0% / 119 ms |

Router 0 restarting mid-run (`cache`, `--router-restart-s 480`), hit rate in the two-minute stage that follows: with
one router, today 62.9% and oracle 67.7% (gossip has no peer to learn from); with two, today 61.8%, oracle and gossip
both 66.4%.

## Conclusions

- With one router the oracle gains nothing: the router's own placements already say where a prefix is.
- Every added router costs today's prequal hit rate and, near saturation, tips the fleet into overload. With the
  oracle or gossip, two to four routers stay within about three points of the single-router hit rate.
- Gossip matches the oracle on hit rate and on median and p90 TTFT up to a second of delay, and stays far ahead of
  today at five seconds or with half its messages lost.
- The oracle keeps a better tail at the saturation point: at 20 req/s with two routers, TTFT p99 is 187 ms with the
  oracle, 1,940 ms with gossip and 5,808 ms today.
- Project 4's gate (3 hit-rate points or 10% at TTFT p90) is met only with several routers, and gossip meets it too,
  with no engine feature, tokenizer or ZMQ dependency. KV events would still cover what gossip cannot: traffic that
  bypasses the routers and engines whose eviction isn't LRU. Neither is measured.

## Not measured

Real engines, reordered messages, routers joining or leaving, more than four routers, a non-LRU engine, and the cost of
the messages themselves (one 8-byte hash per 256 prompt bytes: about 1.4 KB for a 45 KB prompt, per peer).

## Reproducing

```sh
ARMS="none;--oracle-index events;--gossip-ms 100;--gossip-ms 1000;--gossip-ms 5000;--gossip-ms 100 --gossip-loss 0.2;--gossip-ms 100 --gossip-loss 0.5" \
  tools/vsim-peer-index.sh
ARMS="none;--oracle-index events;--gossip-ms 100" WORKLOADS=cache ROUTERS="1 2" \
  LLM_BENCH_ARGS="--router-restart-s 480" tools/vsim-peer-index.sh results/vsim-peer-index-restart
node tools/llm-summary.mjs results/vsim-peer-index
```
