// node trace-stats.mjs <trace.jsonl>... — working set and reuse structure of Mooncake traces.
import { readFileSync } from "node:fs";

for (const file of process.argv.slice(2)) {
  const reqs = readFileSync(file, "utf8").trim().split("\n").map((l) => JSON.parse(l));
  const seen = new Map();
  const heads = new Map();
  let inTok = 0, outTok = 0, hitBlocks = 0, blocks = 0;
  const gaps = [];
  for (const r of reqs) {
    inTok += r.input_length;
    outTok += r.output_length;
    let prefix = true;
    for (const h of r.hash_ids) {
      blocks++;
      if (prefix && seen.has(h)) hitBlocks++;
      else prefix = false;
      if (seen.has(h)) gaps.push(r.timestamp - seen.get(h));
      seen.set(h, r.timestamp);
    }
    const head = r.hash_ids.slice(0, 2).join(",");
    heads.set(head, (heads.get(head) || 0) + 1);
  }
  gaps.sort((a, b) => a - b);
  const q = (p) => (gaps[Math.floor(p * (gaps.length - 1))] / 1000).toFixed(1);
  const top = [...heads.values()].sort((a, b) => b - a).slice(0, 5);
  const span = reqs.at(-1).timestamp / 1000;
  console.log(`${file}: ${reqs.length} reqs over ${span.toFixed(0)} s (${(reqs.length / span).toFixed(2)}/s)`);
  console.log(`  in/out mean ${(inTok / reqs.length).toFixed(0)}/${(outTok / reqs.length).toFixed(0)} tokens; distinct blocks ${seen.size} = ${((seen.size * 512) / 1e6).toFixed(1)}M tokens`);
  console.log(`  infinite-cache prefix hit (blocks) ${(hitBlocks / blocks).toFixed(3)}; block reuse gap p10/p50/p90 ${q(0.1)}/${q(0.5)}/${q(0.9)} s`);
  console.log(`  distinct 2-block heads ${heads.size}; top head counts ${top.join(" ")}`);
}
