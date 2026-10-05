#!/usr/bin/env bash
# Blocks until vLLM on :8000 answers /health; fails if the server process exits first or ${1:-900}s pass.
deadline=$((SECONDS + ${1:-900}))
seen=0
until curl -sf -o /dev/null http://127.0.0.1:8000/health; do
  if wsl.exe -d Ubuntu -- pgrep -f 'vllm serve' >/dev/null; then seen=1; elif [ "$seen" = 1 ]; then
    echo "vllm exited before becoming healthy" >&2
    exit 1
  fi
  [ "$SECONDS" -lt "$deadline" ] || { echo "timed out waiting for vllm" >&2; exit 2; }
  sleep 2
done
