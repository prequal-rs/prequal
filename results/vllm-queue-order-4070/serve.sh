#!/usr/bin/env bash
# Runs inside WSL: (re)starts vLLM on :8000, replacing any running server. Extra args are passed to `vllm serve`.
# Usage (from Windows): wsl.exe -d Ubuntu -- bash "$(wslpath of this file)" [args]
# VLLM_HOME (default ~/vllm-bench) holds the venv with vLLM installed, and receives serve.log.
set -uo pipefail
home="${VLLM_HOME:-$HOME/vllm-bench}"
pkill -f 'vllm serve' && while pgrep -f 'vllm serve' >/dev/null; do sleep 0.5; done
while nvidia-smi --query-compute-apps=pid --format=csv,noheader | grep -q .; do sleep 0.5; done
# WSL's apt nvcc (12.4) can't JIT-build FlashInfer's sampler; use vLLM's PyTorch sampler instead.
export VLLM_USE_FLASHINFER_SAMPLER=0
exec "$home/.venv/bin/vllm" serve "${MODEL:-Qwen/Qwen2.5-1.5B-Instruct}" --port 8000 --max-model-len 32000 "$@" \
  >"$home/serve.log" 2>&1
