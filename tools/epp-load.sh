#!/usr/bin/env bash
# prequal-epp CPU-per-request harness (crates/prequal-epp/examples/epp_load), run over ssh on a dedicated 12-thread
# Linux host (BENCH_HOST, required) in its own checkout (~/$BOX, default ~/work/prequal-rs-perf) so it never disturbs
# other trees there. Timed and profiled runs hold ~/bench.lock and stay off CPU 5/11. Default: a real Envoy with the llm-d
# chart's config in front of the EPP (ENVOY, extracted from envoyproxy/envoy:distroless-v1.33.2) and desynchronized
# token timing: EPP CPU/request and syscalls/request within ~10% of kind, reps within ~2% (Envoy's own CPU ~25%
# under kind). EPP on cores 0-2, Envoy (~2 cores) on both threads of cores 3-4, client + fake sims on 6-8.
# ENVOY= (empty) uses the direct ext_proc driver instead: its tokio-timer tokens fire in lockstep, so messages batch
# unrealistically and one binary swings 14-22 ms/req (A/B ratios 2.9x where kind showed 1.6x): use it for quick
# functional checks only. ROUTER=1 replaces Envoy+EPP with prequal-router alone (same traffic; its CPU is reported as
# cpu_us/req). BOX=<dir under ~> uses another checkout.
# Usage: tools/epp-load.sh sync | build | run [harness args] | keep <label> | ab <label> <reps> [harness args]
#        | perf <name> [harness args] | pull <name>
# Don't edit this file while a run is going: bash reads scripts lazily and the run breaks mid-way.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
host=${BENCH_HOST:?set BENCH_HOST to the benchmark host (ssh name)}
box=${BOX:-work/prequal-rs-perf}
envoy=${ENVOY-\$HOME/work/bin/envoy-1.33.2}
if [ -n "${ROUTER:-}" ]; then
  driver="taskset -c ${DRIVER_CPUS:-6,7,8} target/release/examples/epp_load --epp-cpus ${EPP_CPUS:-0-2} --router target/release/prequal-router --itl-us 1600 --seconds 30"
elif [ -n "$envoy" ]; then
  driver="taskset -c ${DRIVER_CPUS:-6,7,8} target/release/examples/epp_load --epp-cpus ${EPP_CPUS:-0-2} --envoy $envoy --envoy-cpus 3,4,9,10 --itl-us 1600 --seconds 30"
else
  driver="taskset -c ${DRIVER_CPUS:-3,4,9,10} target/release/examples/epp_load --epp-cpus ${EPP_CPUS:-0-2}"
fi
sub=${1:?sync|build|run|keep|ab|perf|pull}; shift

case $sub in
sync)
  tar -C "$root" --exclude=./target --exclude=./results -czf - . | ssh "$host" "mkdir -p ~/$box && tar -xzf - -C ~/$box"
  ;;
build)
  ssh "$host" ". ~/.cargo/env && cd ~/$box && cargo build -q --release -p prequal-epp -p prequal-router --bin prequal-epp --bin prequal-router --example epp_load"
  ;;
run)
  ssh "$host" "cd ~/$box && flock ~/bench.lock $driver ${*:+$(printf '%q ' "$@")}"
  ;;
keep)
  # Saves the current build as an A/B baseline.
  ssh "$host" "cp ~/$box/target/release/prequal-epp ~/$box/target/prequal-epp.${1:?label}"
  ;;
ab)
  # Alternates a kept baseline and the current build under one lock hold.
  label=${1:?label}; reps=${2:?reps}; shift 2
  ssh "$host" "cd ~/$box && flock ~/bench.lock bash -c 'for i in \$(seq $reps); do
      for epp in target/prequal-epp.$label target/release/prequal-epp; do
        printf \"%-28s \" \$epp; $driver --epp \$epp ${*:+$(printf '%q ' "$@")} 2>/dev/null | grep -o \" cpu_us/req=[0-9]*\\|envoy_cpu_us/req=[0-9]*\\|_ms_p99=[0-9.]*\\|driver_cores=[-0-9.]*\\|other_host_cores=[-0-9.]*\" | tr \"\\n\" \" \"; echo
      done; done'"
  ;;
perf)
  # Samples the whole harness as root (kernel symbols), then reports only the EPP/router process (pid on harness
  # stderr), or Envoy with PROC=envoy (symbols: ENVOY=~/work/envoydbg/envoy-debug, same build as the distroless one,
  # from envoyproxy/envoy:debug-v1.33.2). env: CG=dwarf|lbr call graphs (dwarf: full Rust stacks), FREQ sampling Hz.
  name=${1:?name}; shift
  ssh "$host" "set -e; P=~/work/prof/$name; rm -rf \$P; mkdir -p \$P; cd ~/$box
    flock ~/bench.lock sudo perf record -q -F ${FREQ:-499} --call-graph ${CG:-dwarf,32768} -o \$P/perf.data -- \
      sudo -u \$USER $driver ${*:+$(printf '%q ' "$@")} 2>\$P/stderr | tee \$P/result
    if [ '${PROC:-}' = envoy ]; then pid=\$(grep -o 'envoy Some([0-9]*' \$P/stderr | tr -dc 0-9)
    else pid=\$(grep -o 'pid [0-9]*' \$P/stderr | cut -d' ' -f2); fi
    rep() { sudo perf report --no-inline -i \$P/perf.data --pid \$pid --percentage relative --stdio -g none --percent-limit 0.5 \"\$@\" 2>/dev/null | grep -v -e '^#' -e '^\$' | ~/.cargo/bin/rustfilt | cut -c1-200 | head -n 70; }
    rep --no-children --sort symbol > \$P/self.txt
    rep --no-children --sort dso > \$P/by-dso.txt
    sudo perf script --no-inline -i \$P/perf.data --pid \$pid 2>/dev/null | ~/.cargo/bin/inferno-collapse-perf | ~/.cargo/bin/rustfilt > \$P/folded.txt
    ~/.cargo/bin/inferno-flamegraph < \$P/folded.txt > \$P/flame.svg"
  "$0" pull "$name"
  ;;
pull)
  name=${1:?name}; dest="$root/target/prof/$name"
  mkdir -p "$dest"
  ssh "$host" "tar -C ~/work/prof/$name --exclude=perf.data -czf - ." | tar -xzf - -C "$dest"
  echo "pulled to $dest"
  ;;
*) echo "unknown subcommand $sub" >&2; exit 2 ;;
esac
