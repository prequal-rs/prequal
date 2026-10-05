#!/usr/bin/env bash
# Stops vLLM in WSL and returns once :8000 no longer answers, so a following wait-ready.sh can't see the old server.
wsl.exe -d Ubuntu -- pkill -f 'vllm serve'
while curl -sf -o /dev/null --max-time 2 http://127.0.0.1:8000/health; do sleep 0.5; done
# The VM's page cache counts against Windows' commit limit, which vLLM's GPU allocations also need (see results/README.md).
wsl.exe -d Ubuntu -u root -- sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
