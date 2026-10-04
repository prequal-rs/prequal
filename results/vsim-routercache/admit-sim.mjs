// node admit-sim.mjs <trace.jsonl> <capacity blocks> — offline ceiling for fleet admission on a Mooncake trace:
// prefix-hit rate (blocks) of one pooled LRU vs a 2Q split (scratch fraction f for prompts whose first unshared
// block was never seen / seen longer ago than the ghost window; the rest in a protected LRU).
import { readFileSync } from "node:fs";

class Lru {
  constructor(cap) { this.cap = cap; this.m = new Map(); }
  has(b) { return this.m.has(b); }
  touch(b) { this.m.delete(b); this.m.set(b, 1); if (this.m.size > this.cap) this.m.delete(this.m.keys().next().value); }
}

const [file, capArg] = process.argv.slice(2);
const reqs = readFileSync(file, "utf8").trim().split("\n").map((l) => JSON.parse(l));
const cap = parseInt(capArg);
const common = (() => { const c = new Map(); for (const r of reqs) c.set(r.hash_ids[0], (c.get(r.hash_ids[0]) || 0) + 1); return c; })();

function run(f, ghostSecs) {
  const parts = f === 0 ? [new Lru(cap)] : [new Lru(Math.round(cap * (1 - f))), new Lru(Math.round(cap * f))];
  const seen = new Map();
  let hit = 0, total = 0, probation = 0;
  for (const r of reqs) {
    const ids = r.hash_ids;
    // Key: first block that isn't the trace-wide shared head (shared by > 20% of requests).
    const k = ids.find((b) => common.get(b) === undefined || common.get(b) < reqs.length * 0.2) ?? ids[ids.length - 1];
    const last = seen.get(k);
    seen.set(k, r.timestamp);
    const recurring = last !== undefined && (r.timestamp - last) / 1000 <= ghostSecs;
    // A request is served by the partition holding its longest prefix past the shared head, else by its class.
    const depth = ids.indexOf(k);
    let best = 0, bestPart = recurring || parts.length === 1 ? 0 : 1;
    parts.forEach((p, i) => { let n = 0; while (n < ids.length && p.has(ids[n])) n++; if (n > best) { best = n; if (n > depth) bestPart = i; } });
    hit += best; total += ids.length;
    if (!recurring && parts.length > 1 && best <= depth) probation++;
    for (const b of ids) parts[bestPart].touch(b);
  }
  return [hit / total, probation / reqs.length];
}

const [base] = run(0, 0);
console.log(`${file.split("/").pop()} cap ${cap} blocks: pooled LRU ${(base * 100).toFixed(2)}%`);
for (const f of [0.1, 0.2, 0.3, 0.5]) {
  for (const g of [60, 300, 1200, 1e9]) {
    const [h, p] = run(f, g);
    console.log(`  scratch ${f} ghost ${g}s: ${(h * 100).toFixed(2)}% (probation ${(p * 100).toFixed(1)}% of requests)`);
  }
}
