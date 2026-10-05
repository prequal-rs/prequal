// Usage: node check-priority.mjs [accept|order] — what vLLM on :8000 does with the `priority` body field.
// accept: status codes for priority 0, non-zero, and duplicated keys where one value is invalid.
// order: fills the KV cache with blockers, then queues A {"priority":1,"priority":9} before
//        B {"priority":9,"priority":1}; vLLM runs the lowest priority first, so whoever starts first shows which
//        duplicate it kept.
const URL = "http://127.0.0.1:8000/v1/completions";
const MODEL = process.env.MODEL ?? "Qwen/Qwen2.5-1.5B-Instruct";

const post = (body) => fetch(URL, { method: "POST", headers: { "content-type": "application/json" }, body });
const body = (fields, prompt = '"hi"', maxTokens = 4) =>
  `{"model":"${MODEL}","prompt":${prompt},"max_tokens":${maxTokens},"ignore_eos":true,${fields}}`;

async function accept() {
  const cases = {
    "priority 0": '"priority":0',
    "priority 5": '"priority":5',
    "priority -3": '"priority":-3',
    'duplicate "x" then 1': '"priority":"x","priority":1',
    'duplicate 1 then "x"': '"priority":1,"priority":"x"',
    "duplicate 5 then 0": '"priority":5,"priority":0',
    "duplicate 0 then 5": '"priority":0,"priority":5',
  };
  for (const [name, fields] of Object.entries(cases)) {
    const response = await post(body(fields));
    const text = await response.text();
    console.log(`${name}: HTTP ${response.status}${response.ok ? "" : " " + text.slice(0, 160)}`);
  }
}

const ids = (seed, n) => JSON.stringify(Array.from({ length: n }, (_, i) => 1000 + ((seed * 7919 + i * 31) % 20000)));

/** Milliseconds from `origin` to the first streamed chunk. */
async function firstChunkAt(origin, fields, seed, maxTokens) {
  const response = await post(body(`"stream":true,${fields}`, ids(seed, 30000), maxTokens));
  if (!response.ok) throw new Error(`HTTP ${response.status} ${await response.text()}`);
  const reader = response.body.getReader();
  await reader.read();
  const at = Date.now() - origin;
  while (!(await reader.read()).done);
  return at;
}

async function order() {
  const origin = Date.now();
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const blockers = [400, 1200, 2000, 2000].map((n, i) => firstChunkAt(origin, '"priority":0', i + 1, n));
  await sleep(1500);
  const a = firstChunkAt(origin, '"priority":1,"priority":9', 11, 50);
  await sleep(300);
  const b = firstChunkAt(origin, '"priority":9,"priority":1', 12, 50);
  const [ta, tb] = await Promise.all([a, b]);
  console.log(`blockers first chunk at ${(await Promise.all(blockers)).join(", ")} ms`);
  console.log(`A {"priority":1,"priority":9} sent 1500 ms, first chunk ${ta} ms`);
  console.log(`B {"priority":9,"priority":1} sent 1800 ms, first chunk ${tb} ms`);
  console.log(tb < ta ? "B ran first: the LAST duplicate wins" : "A ran first: the FIRST duplicate wins (or no queue formed)");
}

await { accept, order }[process.argv[2] ?? "accept"]();
