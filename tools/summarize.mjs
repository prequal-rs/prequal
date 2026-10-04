// Usage: node tools/summarize.mjs results/http/grid.csv — averages seeds per config and load as a markdown table.
import { readFileSync } from "node:fs";

const [header, ...rows] = readFileSync(process.argv[2] ?? "results/http/grid.csv", "utf8").trim().split(/\r?\n/);
const cols = header.split(",");
const records = rows.map((line) => Object.fromEntries(line.split(",").map((v, i) => [cols[i], v])));

const label = (r) =>
  r.policy === "Prequal" ? `Prequal p=${r.probes_per_query}${r.piggyback === "true" ? "+pb" : ""} ${r.estimator}` : r.policy;
const metrics = ["p99_ms", "p999_ms", "errors", "client_cpu_us_per_req", "server_cpu_us_per_req", "client_peak_mb"];

const groups = new Map();
for (const r of records) {
  const key = `${r.load}|${label(r)}`;
  groups.set(key, [...(groups.get(key) ?? []), r]);
}

console.log(`| load | config | runs | ${metrics.join(" | ")} |`);
console.log(`|${"---|".repeat(metrics.length + 3)}`);
for (const [key, group] of [...groups].sort()) {
  const [load, name] = key.split("|");
  const mean = (m) => group.reduce((s, r) => s + Number(r[m]), 0) / group.length;
  console.log(`| ${load} | ${name} | ${group.length} | ${metrics.map((m) => mean(m).toFixed(0)).join(" | ")} |`);
}
