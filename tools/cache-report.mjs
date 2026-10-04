#!/usr/bin/env node
// Per-stage report for tools/kind-cache.sh runs: one JSON line per run and stage (or --md: per-arm tables averaged
// over rounds). Usage: node tools/cache-report.mjs <reports dir> [label regex] [--md] [--slo-p90 0.5]
// TTFT percentiles across the hot/warm/cold generators are mixture quantiles: each generator's CDF is interpolated
// linearly between its reported percentiles and weighted by its successes (inference-perf reports no per-request data).
// Default SLO: TTFT p90 <= 0.5 s and <= 1% failures (--slo-p90, --max-fail).
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const flag = (name, dflt) => {
  const i = args.indexOf(name);
  return i < 0 ? dflt : Number(args.splice(i, 2)[1]);
};
const md = args.includes("--md") && args.splice(args.indexOf("--md"), 1);
const sloP90 = flag("--slo-p90", 0.5);
const maxFail = flag("--max-fail", 0.01);
const [dir, filter = ""] = args;
const tiers = { hot: "-hot", warm: "", cold: "-cold" };
const knots = [["min", 0], ["p0.1", 0.001], ["p1", 0.01], ["p5", 0.05], ["p10", 0.1], ["p25", 0.25], ["median", 0.5],
  ["p75", 0.75], ["p90", 0.9], ["p95", 0.95], ["p99", 0.99], ["p99.9", 0.999], ["max", 1]];

const read = (f) => (fs.existsSync(f) ? fs.readFileSync(f, "utf8") : "");
const lines = (f) => read(f).split("\n").filter(Boolean);

function jobDirs(label) {
  const out = {};
  for (const [tier, suffix] of Object.entries(tiers)) {
    const re = new RegExp(`^${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}${suffix}-\\d{8}-\\d{6}$`);
    const d = fs.readdirSync(dir).filter((e) => re.test(e)).sort().pop();
    if (d) out[tier] = path.join(dir, d);
  }
  return out;
}

function cdf(ks, x) {
  if (x <= ks[0][0]) return 0;
  for (let i = 1; i < ks.length; i++) {
    const [v1, p1] = ks[i], [v0, p0] = ks[i - 1];
    if (x <= v1) return v1 > v0 ? p0 + ((p1 - p0) * (x - v0)) / (v1 - v0) : p1;
  }
  return 1;
}

function mixtureQuantile(parts, q) {
  const total = parts.reduce((s, p) => s + p.n, 0);
  if (!total) return null;
  let lo = Math.min(...parts.map((p) => p.ks[0][0])), hi = Math.max(...parts.map((p) => p.ks.at(-1)[0]));
  for (let i = 0; i < 60; i++) {
    const mid = (lo + hi) / 2;
    const f = parts.reduce((s, p) => s + p.n * cdf(p.ks, mid), 0) / total;
    if (f < q) lo = mid; else hi = mid;
  }
  return hi;
}

// `<ts> ... Stage N - run started|completed|failed` -> {N: {start, end}} in epoch seconds.
function stageWindows(file) {
  const w = {};
  for (const l of lines(file)) {
    const m = l.match(/^(\S+) .*Stage (\d+) - run (started|completed|failed)/);
    if (!m) continue;
    const t = Date.parse(m[1]) / 1000;
    w[m[2]] ??= {};
    w[m[2]][m[3] === "started" ? "start" : "end"] = t;
  }
  return w;
}

// Fleet prefix-hit rate between the 5 s counter samples bracketing a window (as kind-llmd.sh's stage_hits).
function windowHits(samples, { start, end }) {
  if (!samples.length || start === undefined || end === undefined) return null;
  let a = samples[0], b = samples.at(-1);
  for (const s of samples) if (s[0] <= start) a = s;
  for (const s of [...samples].reverse()) if (s[0] >= end) b = s;
  const dq = b[2] - a[2];
  return dq > 0 ? (b[1] - a[1]) / dq : null;
}

// Mean cores used by the kind node and by everything else on the host over a window (`kind-cache.sh host-cpu` log).
function hostCores(samples, { start, end }) {
  const inside = samples.filter((s) => s[0] >= start && s[0] <= end);
  if (inside.length < 2) return {};
  const [a, b] = [inside[0], inside.at(-1)], dt = b[0] - a[0];
  const kind = (b[2] - a[2]) / dt;
  return { kind_cores: Math.round(kind * 10) / 10, foreign_cores: Math.round(((b[1] - a[1]) / dt - kind) * 10) / 10 };
}

// Peak and mean of per-sample totals (millicores, MiB) per container.
function usage(file) {
  const samples = [];
  let cur = null;
  for (const l of lines(file)) {
    const f = l.trim().split(/\s+/);
    if (f[0] === "load") { cur = { load: Number(f[1]) }; samples.push(cur); continue; }
    if (!cur || f.length < 4) continue;
    const c = f[1];
    cur[`${c}_mcpu`] = (cur[`${c}_mcpu`] ?? 0) + parseInt(f[2], 10);
    cur[`${c}_mib`] = (cur[`${c}_mib`] ?? 0) + parseInt(f[3], 10);
  }
  const out = {};
  for (const key of new Set(samples.flatMap(Object.keys))) {
    const v = samples.map((s) => s[key]).filter((x) => x !== undefined);
    out[`peak_${key}`] = Math.max(...v);
    if (key !== "load") out[`mean_${key}`] = Math.round(v.reduce((s, x) => s + x, 0) / v.length);
  }
  return out;
}

function simTotals(file) {
  let h = 0, q = 0, n = 0;
  const epp = {};
  for (const l of lines(file)) {
    const f = l.split(/\s+/), v = Number(f.at(-1));
    if (/prefix_cache_hits(_total)?[{ ]/.test(l)) h += v;
    else if (/prefix_cache_queries(_total)?[{ ]/.test(l)) q += v;
    else if (/request_success_total[{ ]/.test(l)) n += v;
    else if (f[0].startsWith("epp-")) {
      const name = f[1].replace(/\{.*$/, "");
      epp[name] = (epp[name] ?? 0) + v;
    }
  }
  return { hits: h, queries: q, requests: n, epp };
}

function report(label) {
  const jobs = jobDirs(label);
  const samples = lines(path.join(dir, `${label}-hits.log`)).map((l) => l.split(" ").map(Number));
  const windows = Object.fromEntries(Object.entries(tiers).map(([t, s]) =>
    [t, stageWindows(path.join(dir, `${label}${s}-stages.log`))]));
  const sims = simTotals(path.join(dir, `${label}-sims.txt`));
  const simTokPerReq = sims.requests ? sims.queries / sims.requests : null;
  const stages = [];
  for (let n = 0; ; n++) {
    const tierStats = {};
    for (const [tier, d] of Object.entries(jobs)) {
      const f = path.join(d, `stage_${n}_lifecycle_metrics.json`);
      if (fs.existsSync(f)) tierStats[tier] = JSON.parse(read(f));
    }
    if (!Object.keys(tierStats).length) break;
    const parts = Object.values(tierStats).filter((j) => j.successes?.count).map((j) => ({
      n: j.successes.count, ks: knots.map(([k, p]) => [j.successes.latency.time_to_first_token[k], p]) }));
    const sum = (fn) => Object.values(tierStats).reduce((s, j) => s + (fn(j) ?? 0), 0);
    const ok = sum((j) => j.successes?.count), fail = sum((j) => j.failures?.count);
    const promptLen = sum((j) => (j.successes?.prompt_len?.mean ?? 0) * (j.successes?.count ?? 0)) / (ok || 1);
    const starts = Object.values(windows).map((w) => w[n]?.start).filter((x) => x !== undefined);
    const hit = windowHits(samples, windows.warm[n] ?? {});
    const r3 = (x) => (x === null ? null : Math.round(x * 1000) / 1000);
    stages.push({
      run: label, stage: n, rate: r3(sum((j) => j.load_summary?.requested_rate)), ok, fail,
      ttft_p50: r3(mixtureQuantile(parts, 0.5)), ttft_p90: r3(mixtureQuantile(parts, 0.9)),
      ttft_p99: r3(mixtureQuantile(parts, 0.99)),
      ...Object.fromEntries(Object.entries(tierStats).map(([t, j]) =>
        [`${t}_ttft_p90`, r3(j.successes?.latency?.time_to_first_token?.p90 ?? null)])),
      prefix_hit: r3(hit), recomputed_prompt_tok: hit === null ? null : Math.round((1 - hit) * promptLen),
      recomputed_sim_tok: hit === null || !simTokPerReq ? null : Math.round((1 - hit) * simTokPerReq),
      stage_skew_s: starts.length ? Math.round(Math.max(...starts) - Math.min(...starts)) : null,
      ...hostCores(hostCpu, windows.warm[n] ?? {}),
    });
  }
  // Max QPS at SLO: highest sweep rate (stage 0 is warm-up) with every sweep stage at or below it meeting the SLO.
  const sweep = stages.slice(1).sort((a, b) => a.rate - b.rate);
  let capacity = 0;
  for (const s of sweep) {
    if (s.ttft_p90 !== null && s.ttft_p90 <= sloP90 && s.fail <= maxFail * (s.ok + s.fail)) capacity = s.rate;
    else break;
  }
  const summary = {
    run: label, capacity_qps: capacity, slo: `ttft_p90<=${sloP90}s,fail<=${maxFail * 100}%`,
    prefix_hit_rate: sims.queries ? Math.round((sims.hits / sims.queries) * 1000) / 1000 : null,
    sim_tok_per_req: simTokPerReq && Math.round(simTokPerReq), ...usage(path.join(dir, `${label}-top.log`)),
    epp: sims.epp,
  };
  return { stages, summary };
}

const hostCpu = lines(path.join(dir, "host-cpu.log")).map((l) => l.split(" ").map(Number));
const labels = fs.readdirSync(dir).filter((f) => f.endsWith("-hits.log")).map((f) => f.slice(0, -9))
  .filter((l) => new RegExp(filter).test(l)).sort();
const runs = labels.map(report);
if (!md) {
  for (const r of runs) for (const line of [...r.stages, r.summary]) console.log(JSON.stringify(line));
} else {
  const arms = {};
  for (const r of runs) (arms[r.summary.run.replace(/-r\d+$/, "")] ??= []).push(r);
  const mean = (xs) => { const v = xs.filter((x) => x !== null && x !== undefined); return v.length ? v.reduce((s, x) => s + x, 0) / v.length : null; };
  const f = (x, d = 3) => (x === null ? "-" : x.toFixed(d));
  for (const [arm, rs] of Object.entries(arms)) {
    console.log(`\n### ${arm} (${rs.length} rounds; capacity at SLO: ${rs.map((r) => r.summary.capacity_qps).join(", ")} QPS)\n`);
    console.log("| stage | QPS | ok | fail | hit | recomputed tok/req | TTFT p50 | TTFT p90 (range) | TTFT p99 | hot/warm/cold p90 | foreign cores (max) |");
    console.log("|---|---|---|---|---|---|---|---|---|---|---|");
    for (let n = 0; n < rs[0].stages.length; n++) {
      const st = rs.map((r) => r.stages[n]).filter(Boolean), g = (k) => mean(st.map((s) => s[k]));
      const p90 = st.map((s) => s.ttft_p90).filter((x) => x !== null);
      console.log(`| ${n}${n ? "" : " (warm-up)"} | ${g("rate")} | ${Math.round(g("ok"))} | ${Math.round(g("fail"))} | ${f(g("prefix_hit"))} | ` +
        `${f(g("recomputed_prompt_tok"), 0)} | ${f(g("ttft_p50"))} | ${f(g("ttft_p90"))} (${f(Math.min(...p90))}-${f(Math.max(...p90))}) | ` +
        `${f(g("ttft_p99"))} | ${["hot", "warm", "cold"].map((t) => f(g(`${t}_ttft_p90`))).join(" / ")} | ` +
        `${f(Math.max(...st.map((s) => s.foreign_cores ?? -Infinity)), 1).replace("-Infinity", "-")} |`);
    }
    const u = (k) => f(mean(rs.map((r) => r.summary[k])), 0);
    console.log(`\nRun hit rate ${f(mean(rs.map((r) => r.summary.prefix_hit_rate)))}; picker CPU mean/peak ${u("mean_epp_mcpu")}/${u("peak_epp_mcpu")} m, ` +
      `mem peak ${u("peak_epp_mib")} MiB; Envoy CPU mean/peak ${u("mean_envoy-proxy_mcpu")}/${u("peak_envoy-proxy_mcpu")} m, mem peak ${u("peak_envoy-proxy_mib")} MiB; ` +
      `peak load1 ${f(mean(rs.map((r) => r.summary.peak_load)), 1)}`);
  }
}
