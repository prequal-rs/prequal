#!/usr/bin/env bash
# Usage: tools/fetch-mooncake-traces.sh [dir] — downloads Kimi's public Mooncake request traces (FAST'25 release,
# github.com/kvcache-ai/Mooncake, Apache-2.0; ~9 MB) for `llm-bench --virtual --trace` and the trace-* scenarios of
# tools/vsim-compete.sh. Default dir: $TRACES or target/mooncake-traces.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${1:-${TRACES:-$root/target/mooncake-traces}}"
base=https://raw.githubusercontent.com/kvcache-ai/Mooncake/main/FAST25-release/traces
mkdir -p "$dir"
for trace in conversation toolagent synthetic; do
  curl -fsSL "$base/${trace}_trace.jsonl" -o "$dir/$trace.jsonl"
done
wc -l "$dir"/*.jsonl
