#!/usr/bin/env bash
# Usage: start.sh [vllm serve args] — (re)starts vLLM in WSL, writes a summary to ready.txt once healthy, then stays
# attached: the server lives only as long as this script's wsl.exe client, so run it in the background.
here="$(cd "$(dirname "$0")" && pwd)"
serve="$(wsl.exe -d Ubuntu -- wslpath -a "$(cygpath -m "$here")/serve.sh" | tr -d '\r\0')"
bash "$here/stop.sh"
MSYS_NO_PATHCONV=1 wsl.exe -d Ubuntu -- bash "$serve" "$@" &
started=$SECONDS
bash "$here/wait-ready.sh" 900
echo "ready=$? after $((SECONDS - started))s" >"$here/ready.txt"
wsl.exe -d Ubuntu -- bash -lc 'grep -E "GPU KV cache size|OutOfMemory|Error:|non-default args" "${VLLM_HOME:-$HOME/vllm-bench}/serve.log" | cut -c1-320 | tail -6' | tr -d '\0' >>"$here/ready.txt"
nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader >>"$here/ready.txt"
wait
