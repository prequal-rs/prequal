// Usage: node tools/llm-summary.mjs <dir-or-csv...> — per scenario, each policy's TTFT p50/p90/p99, output tok/s,
// prefix hit rate and errors at each stage rate (mean over seeds), as markdown tables.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { basename, join } from "node:path";

const files = process.argv.slice(2).flatMap((p) =>
  statSync(p).isDirectory() ? readdirSync(p).filter((f) => f.endsWith(".csv")).map((f) => join(p, f)) : [p],
);

const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const fmt = (x, digits = 0) => (Number.isFinite(x) ? x.toFixed(digits) : "-");

for (const file of files) {
  const [header, ...lines] = readFileSync(file, "utf8").trim().split(/\r?\n/);
  const cols = header.split(",");
  const rows = lines.map((l) => Object.fromEntries(l.split(",").map((v, i) => [cols[i], v])));
  const key = (r) => (!parseFloat(r.stage_rate) ? `c${r.concurrency}` : r.stage_rate);
  const groups = new Map();
  for (const r of rows) {
    const k = `${key(r)}|${r.policy}`;
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(r);
  }
  const stages = [...new Set(rows.map(key))].sort((a, b) => parseFloat(a.replace("c", "")) - parseFloat(b.replace("c", "")));
  const policies = [...new Set(rows.map((r) => r.policy))];
  console.log(`\n### ${basename(file, ".csv")}\n`);
  console.log("| stage | policy | runs | ttft p50 | ttft p90 | ttft p99 | out tok/s | hit rate | errors |");
  console.log("|---|---|---|---|---|---|---|---|---|");
  for (const stage of stages) {
    for (const policy of policies) {
      const g = groups.get(`${stage}|${policy}`);
      if (!g) continue;
      const m = (c) => mean(g.map((r) => parseFloat(r[c])));
      const errors = g.reduce((a, r) => a + parseInt(r.errors, 10), 0);
      console.log(
        `| ${stage} | ${policy} | ${g.length} | ${fmt(m("ttft_p50_ms"))} | ${fmt(m("ttft_p90_ms"))} | ${fmt(m("ttft_p99_ms"))} | ${fmt(m("tokens_per_s"))} | ${fmt(m("prefix_hit_rate") * 100, 1)}% | ${errors} |`,
      );
    }
  }
}
