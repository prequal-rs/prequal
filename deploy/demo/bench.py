"""Runs inference-perf's shared-prefix workload through each router in turn and prints a comparison.

Each argument is a policy name `p`: its router is `router-p` (:8000, admin :8081) and its fleet `sim-p` (:8000).
"""
import glob
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

RATE = float(os.environ.get("RATE", "10"))
DURATION = int(os.environ.get("DURATION", "60"))
REPLICAS = int(os.environ.get("REPLICAS", "4"))

CONFIG = """
load: {{type: constant, stages: [{{rate: {rate}, duration: {duration}}}], num_workers: 4}}
api: {{type: completion, streaming: true}}
server: {{type: vllm, model_name: Qwen/Qwen3-8B, base_url: "http://{router}:8000", ignore_eos: true}}
data:
  type: shared_prefix
  shared_prefix: {{num_groups: 16, num_prompts_per_group: 5, system_prompt_len: 2000, question_len: 100,
                  output_len: 100, enable_multi_turn_chat: false}}
report: {{request_lifecycle: {{summary: true, per_stage: true, per_request: false}}}}
storage: {{local_storage: {{path: "{out}"}}}}
"""

COUNTER = re.compile(r"^vllm:prefix_cache_(hits|queries)(?:_total)?(?:\{[^}]*\})? ([0-9.e+]+)$", re.M)


def get(url):
    with urllib.request.urlopen(url, timeout=5) as r:
        return r.read().decode()


def wait_for(url):
    for _ in range(120):
        try:
            return get(url)
        except OSError:
            time.sleep(1)
    sys.exit(f"{url} never answered")


def fleet(name):
    """Every replica of the `sim-<name>` service, once each answers and the router has had time to resolve them."""
    for _ in range(120):
        sims = sorted({a[4][0] for a in socket.getaddrinfo(f"sim-{name}", 8000, proto=socket.IPPROTO_TCP)})
        if len(sims) >= REPLICAS:
            break
        time.sleep(1)
    else:
        sys.exit(f"sim-{name} resolved to {len(sims)} of {REPLICAS} replicas")
    for sim in sims:
        wait_for(f"http://{sim}:8000/metrics")
    wait_for(f"http://router-{name}:8081/readyz")
    time.sleep(3)  # the router re-resolves names every 2 s
    return sims


def cache_counters(sims):
    totals = {"hits": 0.0, "queries": 0.0}
    for sim in sims:
        for kind, value in COUNTER.findall(get(f"http://{sim}:8000/metrics")):
            totals[kind] += float(value)
    return totals


def run_arm(name):
    sims = fleet(name)
    out = tempfile.mkdtemp()
    cfg = os.path.join(out, "config.yml")
    with open(cfg, "w") as f:
        f.write(CONFIG.format(rate=RATE, duration=DURATION, router=f"router-{name}", out=out))
    print(f"running {name}: {len(sims)} replicas, {RATE:g} QPS for {DURATION} s ...", flush=True)
    before = cache_counters(sims)
    proc = subprocess.run(["inference-perf", "--config_file", cfg], capture_output=True, text=True)
    if proc.returncode:
        sys.exit(f"inference-perf failed for {name}:\n{proc.stdout[-3000:]}\n{proc.stderr[-3000:]}")
    after = cache_counters(sims)
    stats = json.load(open(glob.glob(f"{out}/**/stage_0_lifecycle_metrics.json", recursive=True)[0]))
    ttft = stats["successes"]["latency"]["time_to_first_token"]
    queries = after["queries"] - before["queries"]
    return {
        "name": name,
        "ok": stats["successes"]["count"],
        "failed": stats.get("failures", {}).get("count", 0),
        "hit": (after["hits"] - before["hits"]) / queries if queries else float("nan"),
        **{p: ttft[key] * 1000 for p, key in (("p50", "median"), ("p90", "p90"), ("p99", "p99"))},
    }


def main():
    rows = [run_arm(name) for name in sys.argv[1:]]
    print(f"\n{'policy':<12} {'requests':>8} {'failed':>6} {'prefix hit':>10} {'TTFT p50':>9} {'p90':>7} {'p99':>7}")
    for r in rows:
        print(f"{r['name']:<12} {r['ok']:>8} {r['failed']:>6} {r['hit']:>10.1%} "
              f"{r['p50']:>7.0f}ms {r['p90']:>5.0f}ms {r['p99']:>5.0f}ms")


if __name__ == "__main__":
    main()
