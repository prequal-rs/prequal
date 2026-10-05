// Usage: node per-seed.mjs [dir] — one row per run CSV (toolagent.<speed>.<arm>.<seed>.csv): each metric's mean
// over the measured stages, as tools/queue-order-summary.mjs averages them, plus engine.tsv's per-run counters.
import { readdirSync, readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const dir = process.argv[2] ?? dirname(fileURLToPath(import.meta.url));
const METRICS = ["ttft_p50_ms", "ttft_p90_ms", "ttft_p99_ms", "e2e_p50_ms", "e2e_p99_ms", "tokens_per_s"];
const table = (file) => {
  const [header, ...lines] = readFileSync(join(dir, file), "utf8").trim().split(/\r?\n/);
  const sep = header.includes("\t") ? "\t" : ",";
  const cols = header.split(sep);
  return lines.map((line) => Object.fromEntries(line.split(sep).map((v, i) => [cols[i], v])));
};
const engine = new Map(table("engine.tsv").map((r) => [`${r.speed}|${r.arm.replace(/[:@]/g, "_")}|${r.seed}`, r]));

console.log(`| speed | seed | arm | ${METRICS.join(" | ")} | completed | errors | prefix_hit | kv_peak | preempt |`);
console.log(`|${"---|".repeat(METRICS.length + 8)}`);
const runs = readdirSync(dir).filter((f) => /^toolagent\..*\.csv$/.test(f)).map((file) => {
  const [, speed, arm, seed] = file.match(/^toolagent\.(.+)\.([^.]+)\.(\d+)\.csv$/);
  return { file, speed, arm, seed };
});
runs.sort((a, b) => a.speed - b.speed || a.seed - b.seed || a.arm.localeCompare(b.arm));
for (const { file, speed, arm, seed } of runs) {
  const rows = table(file);
  const mean = (m) => (rows.reduce((s, r) => s + Number(r[m]), 0) / rows.length).toFixed(0);
  const sum = (m) => rows.reduce((s, r) => s + Number(r[m]), 0);
  const e = engine.get(`${speed}|${arm}|${seed}`) ?? {};
  const hit = e.prefix_queries > 0 ? ((100 * e.prefix_hits) / e.prefix_queries).toFixed(1) + "%" : "";
  const peak = e.kv_usage_max ? (100 * e.kv_usage_max).toFixed(1) + "%" : "";
  console.log(`| ${speed} | ${seed} | ${arm} | ${METRICS.map(mean).join(" | ")} | ${sum("completed")} | ${sum("errors")} | ${hit} | ${peak} | ${e.preemptions ?? ""} |`);
}
