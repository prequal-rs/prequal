#!/usr/bin/env bash
# Inclusive share of samples whose stack contains each regex (first match wins per stack, in argument order), plus
# the top self-time leaves. Usage: tools/folded-share.sh <folded.txt> [label=regex ...]
set -euo pipefail
folded=${1:?folded.txt}; shift
awk -v specs="$(printf '%s\n' "$@")" '
  BEGIN { n = split(specs, lines, "\n"); for (i = 1; i <= n; i++) { split(lines[i], kv, "="); label[i] = kv[1]; re[i] = substr(lines[i], length(kv[1]) + 2) } }
  { c = $NF; total += c; stack = $0; sub(/ [0-9]+$/, "", stack)
    hit = "other"; for (i = 1; i <= n; i++) if (re[i] != "" && stack ~ re[i]) { hit = label[i]; break }
    by[hit] += c
    k = split(stack, frames, ";"); leaf[frames[k]] += c
    l = frames[k]; side = l ~ /::|^</ ? "user" : l ~ /libc|unknown|anon/ ? "libc/unknown" : "kernel"
    if (side == "kernel") { sys = "kernel/other"; for (i = 1; i <= k; i++) if (frames[i] ~ /^__x64_sys_/) { sys = frames[i]; break } side = sys }
    split_[side] += c }
  END { for (s in split_) printf "%5.1f%%  %s\n", 100 * split_[s] / total, s | "sort -rn"; close("sort -rn")
        print "-- groups"; for (h in by) printf "%5.1f%%  %s\n", 100 * by[h] / total, h | "sort -rn"; close("sort -rn")
        print "-- self"; for (l in leaf) printf "%5.1f%%  %s\n", 100 * leaf[l] / total, substr(l, 1, 150) | "sort -rn | head -25" }
' "$folded"
