// Usage: node tools/queue-order-summary.mjs [dir] — tools/vsim-queue-order.sh's CSVs as one markdown table per trace:
// each queue order's metrics (mean over seeds and measured stages) and their change against FCFS.
import { readdirSync, readFileSync } from "fs";
import { join } from "path";

const dir = process.argv[2] ?? "results/vsim-queue-order";
const METRICS = ["ttft_mean_ms", "ttft_p50_ms", "ttft_p90_ms", "ttft_p99_ms", "e2e_mean_ms", "e2e_p99_ms", "tokens_per_s", "prefix_hit_rate"];
const groups = new Map();
for (const file of readdirSync(dir).filter((f) => f.endsWith(".csv"))) {
  const [header, ...lines] = readFileSync(join(dir, file), "utf8").trim().split(/\r?\n/);
  const cols = header.split(",");
  for (const line of lines) {
    const row = Object.fromEntries(line.split(",").map((v, i) => [cols[i], v]));
    const order = row.policy.match(/\+queue-(.+)$/)?.[1] ?? "fcfs";
    const key = `${row.trace}|${row.speed}|${order}`;
    if (!groups.has(key)) groups.set(key, { rows: [], errors: 0 });
    const g = groups.get(key);
    g.rows.push(row);
    g.errors += Number(row.errors);
  }
}
const mean = (rows, m) => rows.reduce((s, r) => s + Number(r[m]), 0) / rows.length;
const traces = [...new Set([...groups.keys()].map((k) => k.split("|")[0]))].sort();
for (const trace of traces) {
  console.log(`\n### ${trace}\n`);
  console.log(`| speed | order | ${METRICS.join(" | ")} | errors |`);
  console.log(`|${"---|".repeat(METRICS.length + 3)}`);
  const keys = [...groups.keys()].filter((k) => k.startsWith(`${trace}|`)).sort((a, b) => {
    const [, sa, oa] = a.split("|"), [, sb, ob] = b.split("|");
    return sa - sb || (oa === "fcfs" ? -1 : ob === "fcfs" ? 1 : oa.localeCompare(ob));
  });
  for (const key of keys) {
    const [, speed, order] = key.split("|");
    const g = groups.get(key), base = groups.get(`${trace}|${speed}|fcfs`);
    const cells = METRICS.map((m) => {
      const v = mean(g.rows, m);
      const shown = m === "prefix_hit_rate" ? (v * 100).toFixed(1) + "%" : v.toFixed(0);
      if (order === "fcfs" || !base) return shown;
      const delta = (v / mean(base.rows, m) - 1) * 100;
      return `${shown} (${delta >= 0 ? "+" : ""}${delta.toFixed(0)}%)`;
    });
    console.log(`| ${speed} | ${order} | ${cells.join(" | ")} | ${g.errors} |`);
  }
}
