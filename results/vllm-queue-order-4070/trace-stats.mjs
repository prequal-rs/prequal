// Usage: node trace-stats.mjs <trace.jsonl> [max-context] — request count, span and token-length quantiles.
import { readFileSync } from "fs";

const lines = readFileSync(process.argv[2], "utf8").trim().split("\n").map((l) => JSON.parse(l));
const max = Number(process.argv[3] ?? 32000);
const q = (a, p) => a.slice().sort((x, y) => x - y)[Math.floor(p * (a.length - 1))];
const fit = lines.filter((l) => l.input_length + l.output_length <= max);
const input = fit.map((l) => l.input_length), output = fit.map((l) => l.output_length);
const span = (lines.at(-1).timestamp - lines[0].timestamp) / 1e3;
const sum = (a) => a.reduce((s, v) => s + v, 0);
console.log({
  requests: lines.length, fit: fit.length, span_s: span, rps: fit.length / span,
  in_p50: q(input, 0.5), in_p90: q(input, 0.9), in_max: q(input, 1),
  out_p50: q(output, 0.5), out_p90: q(output, 0.9), out_max: q(output, 1),
  in_tok_per_s: sum(input) / span, out_tok_per_s: sum(output) / span,
});
