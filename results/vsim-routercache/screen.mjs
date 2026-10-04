// node screen.mjs <dir>... — per scenario CSV and policy: mean over seeds and sweep stages of prefix hit, TTFT
// p50/p90/p99 (ms), plus per-stage hit/p99 means. One line per policy, sorted by hit.
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join } from "node:path";

const perStage = process.env.STAGES === "1";
for (const dir of process.argv.slice(2)) {
  if (!existsSync(dir)) continue;
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".csv"))) {
    const [header, ...lines] = readFileSync(join(dir, file), "utf8").trim().split(/\r?\n/);
    const cols = header.split(",");
    const rows = lines.map((l) => Object.fromEntries(l.split(",").map((v, i) => [cols[i], v])));
    const by = new Map();
    for (const r of rows) {
      if (!by.has(r.policy)) by.set(r.policy, []);
      by.get(r.policy).push(r);
    }
    console.log(`== ${dir}/${file}`);
    const mean = (rs, k) => rs.reduce((a, r) => a + parseFloat(r[k] || "NaN"), 0) / rs.length;
    const out = [...by].map(([policy, rs]) => {
      const stages = [...new Set(rs.map((r) => r.stage))];
      const st = stages.map((s) => {
        const g = rs.filter((r) => r.stage === s);
        return `${(mean(g, "prefix_hit_rate") * 100).toFixed(1)}/${Math.round(mean(g, "ttft_p99_ms"))}`;
      });
      const errs = rs.reduce((a, r) => a + parseInt(r.errors), 0);
      return [mean(rs, "prefix_hit_rate"), `${policy.padEnd(44)} hit ${(mean(rs, "prefix_hit_rate") * 100).toFixed(2)}% p50/p90/p99 ${mean(rs, "ttft_p50_ms").toFixed(0)}/${mean(rs, "ttft_p90_ms").toFixed(0)}/${mean(rs, "ttft_p99_ms").toFixed(0)} ms err ${errs} n=${rs.length}${perStage ? "  [" + st.join(" ") + "]" : ""}`];
    });
    out.sort((a, b) => b[0] - a[0]).forEach(([, l]) => console.log(l));
  }
}
