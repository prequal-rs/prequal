// Usage: node tools/llm-compare.mjs <baselines-dir> <candidate-dir> [candidate-policy=prequal] [rival=llmd-default]
// Per scenario and stage: the candidate's TTFT p90/p99 and hit rate against a named rival and the best other policy
// (lowest p99), each the mean over seeds, with a win/loss count. Stages where every policy failed are skipped.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const [baseDir, candDir, cand = "prequal", rival = "llmd-default"] = process.argv.slice(2);

const load = (file) => {
  const [header, ...lines] = readFileSync(file, "utf8").trim().split(/\r?\n/);
  const cols = header.split(",");
  return lines.map((l) => Object.fromEntries(l.split(",").map((v, i) => [cols[i], v])));
};
const stageKey = (r) => (parseFloat(r.stage_rate) ? r.stage_rate : `c${r.concurrency}`);
const METRICS = ["ttft_p90_ms", "ttft_p99_ms", "prefix_hit_rate"];

/** Mean of each metric over seeds, per policy and stage; a seed where the stage failed makes the mean fail. */
function aggregate(rows) {
  const groups = new Map();
  for (const r of rows) {
    const key = `${r.policy}|${stageKey(r)}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(r);
  }
  return new Map(
    [...groups].map(([key, g]) => [
      key,
      { runs: g.length, ...Object.fromEntries(METRICS.map((m) => [m, g.reduce((a, r) => a + parseFloat(r[m]), 0) / g.length])) },
    ]),
  );
}

const fmt = (ms) => (ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms)}ms`);
const alive = (s) => s && Number.isFinite(s.ttft_p99_ms);
const cell = (s) => (alive(s) ? `${fmt(s.ttft_p90_ms)} / ${fmt(s.ttft_p99_ms)} (${(s.prefix_hit_rate * 100).toFixed(0)}%)` : "failed");

let wins = 0;
let losses = 0;
for (const file of readdirSync(candDir).filter((f) => f.endsWith(".csv"))) {
  const ours = aggregate(load(join(candDir, file)).filter((r) => r.policy === cand));
  const others = aggregate(load(join(baseDir, file)).filter((r) => r.policy !== cand));
  const policies = [...new Set([...others.keys()].map((k) => k.split("|")[0]))];
  console.log(`\n### ${file.replace(".csv", "")}\n`);
  console.log(`| stage | ${cand} p90 / p99 (hits) | ${rival} | best other | vs ${rival} p99 |`);
  console.log("|---|---|---|---|---|");
  for (const [key, s] of ours) {
    const stage = key.split("|")[1];
    const rv = others.get(`${rival}|${stage}`);
    const rivals = policies.map((p) => [p, others.get(`${p}|${stage}`)]).filter(([, o]) => alive(o));
    if (!alive(s) && rivals.length === 0) continue;
    const best = rivals.reduce((a, b) => (b[1].ttft_p99_ms < a[1].ttft_p99_ms ? b : a), rivals[0]);
    let verdict;
    if (alive(s) && alive(rv)) {
      const ratio = s.ttft_p99_ms / rv.ttft_p99_ms;
      verdict = ratio <= 0.9 ? `win ${ratio.toFixed(2)}x` : ratio >= 1.1 ? `LOSS ${ratio.toFixed(2)}x` : "tie";
      wins += ratio <= 0.9;
      losses += ratio >= 1.1;
    } else if (alive(s)) {
      verdict = "win (rival failed)";
      wins++;
    } else if (alive(rv)) {
      verdict = "LOSS (we failed)";
      losses++;
    } else {
      verdict = "-";
    }
    const bestCell = best ? `${best[0]} ${cell(best[1])}` : "-";
    console.log(`| ${stage} (${s.runs}) | ${cell(s)} | ${cell(rv)} | ${bestCell} | ${verdict} |`);
  }
}
console.log(`\n**vs ${rival}: ${wins} wins, ${losses} losses (mean p99 TTFT over seeds, ±10% = tie)**`);
