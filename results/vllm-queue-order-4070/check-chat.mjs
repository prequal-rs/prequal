// Usage: node check-chat.mjs — status of /v1/chat/completions with priority 0 and 5 on the running vLLM.
for (const priority of [0, 5]) {
  const response = await fetch("http://127.0.0.1:8000/v1/chat/completions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      model: process.env.MODEL ?? "Qwen/Qwen2.5-1.5B-Instruct",
      messages: [{ role: "user", content: "hi" }],
      max_tokens: 4,
      priority,
    }),
  });
  const text = await response.text();
  console.log(`chat priority ${priority}: HTTP ${response.status}${response.ok ? "" : " " + text.slice(0, 200)}`);
}
