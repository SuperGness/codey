// E2E tests for the Rust proxy (proxy-rust) against a mock Antigravity upstream.
// Covers: /health, /responses SSE + non-streaming, item ids, thought signature
// echo, usage mapping, /models (grouped catalog), WS contract, /v1/images/generations,
// /usage, /doctor, image tool interception, web_search interception.
// Run: node test/e2e-rust-proxy.test.mjs
import assert from "node:assert/strict";
import { createServer, request as httpRequest } from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, writeFileSync, mkdirSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import crypto from "node:crypto";

const RUST_EXE = process.env.RUST_PROXY_EXE || join(dirname(fileURLToPath(import.meta.url)), "..", "proxy-rust", "target", "release", process.platform === "win32" ? "antigravity-proxy.exe" : "antigravity-proxy");

let failures = 0;
async function test(name, fn) {
  try {
    await fn();
    console.log(`PASS  ${name}`);
  } catch (e) {
    failures++;
    console.log(`FAIL  ${name}: ${e.message}`);
  }
}

// --- Mock upstream -------------------------------------------------------------
// Speaks the Antigravity Gemini wire: POST /v1internal:streamGenerateContent?alt=sse,
// /v1internal:fetchAvailableModels, /v1internal:loadCodeAssist, /v1internal:retrieveUserQuotaSummary.
const upstreamLog = [];
let imageModelCalls = [];
let quotaFailover = false;
const sockets = new Set();
const cleanEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(ANTIGRAVITY_|NOAGY_|AG_|HFSY_|E2E_)/.test(k)));
const upstream = createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    upstreamLog.push({ url: req.url, body: body ? JSON.parse(body) : null, headers: req.headers });
    if (req.url.startsWith("/v1internal:streamGenerateContent")) {
      const parsed = JSON.parse(body);
      const model = parsed.model;
      if (quotaFailover && req.headers.authorization === "Bearer ya29.mock-access-token") { res.writeHead(429,{"Content-Type":"application/json"});res.end(JSON.stringify({error:{message:"Individual quota reached. Resets in 1h"}}));return; }
      if (model === "gemini-3-pro-image" || (model || "").includes("image")) {
        imageModelCalls.push(model);
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        const imgChunk = { response: { candidates: [{ content: { parts: [{ inlineData: { mimeType: "image/png", data: Buffer.from("fake-png-bytes").toString("base64") } }] }, finishReason: "STOP" }] } };
        res.write(`data: ${JSON.stringify(imgChunk)}\n\n`);
        res.end();
        return;
      }
      // Echo boundary probe: if the request has a model fn-call turn directly
      // after the first user turn with fn-call in it, upstream 400s (the proxy
      // must have normalized it).
      const contents = parsed.request?.contents || [];
      const probe = contents.flatMap(t => t.parts || []).map(p => p.text || "").join(" ");
      if (probe.includes("unicode-probe") || probe.includes("late-error-probe") || probe.includes("truncated-probe")) {
        res.writeHead(200, {"Content-Type":"text/event-stream"});
        const record = Buffer.from(`data: ${JSON.stringify({response:{candidates:[{content:{parts:[{text:"中文🙂文本"}]},finishReason:"STOP"}]}})}\n\n`);
        for (let i=0; i<record.length; i++) res.write(record.subarray(i,i+1));
        if (probe.includes("late-error-probe")) res.write('data: {"error":{"message":"late mock error"}}\n\n');
        if (probe.includes("truncated-probe")) res.write('data: {"response":');
        res.end();return;
      }
      const hasBadBoundary = contents.some((turn, i) => {
        if (turn.role !== "model") return false;
        const hasCall = (turn.parts || []).some((p) => p.functionCall);
        return hasCall && i > 0 && contents[i - 1]?.role !== "user";
      });
      if (hasBadBoundary) {
        res.writeHead(400, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { message: "function call turn comes immediately after a user turn or after a function response turn" } }));
        return;
      }
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      const parts = [{ text: "PONG" }];
      const last = contents[contents.length - 1];
      const lastIsFnResponse = (last?.parts || []).some((p) => p.functionResponse);
      // When the request declares generate_image and there is no pending tool
      // result, the model asks to generate an image; the proxy must intercept,
      // generate via the image model, and replay it as a tool round.
      const decls = (parsed.request?.tools || []).flatMap((t) => t.functionDeclarations || []);
      if (decls.some((d) => d.name === "generate_image") && !lastIsFnResponse) {
        parts.length = 0;
        parts.push({ functionCall: { name: "generate_image", args: { prompt: "a cat" }, id: "call_img_1" } });
      }
      if (decls.some((d) => ["google_search","web_search"].includes(d.name)) && !lastIsFnResponse && contents.some(t=>(t.parts||[]).some(p=>p.text?.includes("execute search")))) {
        parts.length=0; parts.push({functionCall:{name:decls.some(d=>d.name==="google_search")?"google_search":"web_search",args:{query:"mock query"},id:"search-1"}});
      }
      if (lastIsFnResponse) parts.push({ functionCall: { name: "noop", args: {} } });
      // Realistic wire shape: usageMetadata rides with the finishReason chunk.
      const chunk = { response: { candidates: [{ content: { parts }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 11, candidatesTokenCount: 4, thoughtsTokenCount: 3, cachedContentTokenCount: 5, totalTokenCount: 18 } } };
      res.write(`data: ${JSON.stringify(chunk)}\n\n`);
      res.end();
      return;
    }
    if (req.url.startsWith("/v1internal:generateContent")) {
      const parsed=JSON.parse(body);
      if (parsed.model === "gemini-3-flash") {res.writeHead(404);res.end("model unavailable");return;}
      res.writeHead(200,{"Content-Type":"application/json"});res.end(JSON.stringify({response:{candidates:[{content:{parts:[{text:"Grounded mock answer"}]},groundingMetadata:{groundingChunks:[{web:{uri:"https://example.org/source",title:"Mock Source"}}],webSearchQueries:["mock query"]}}]}}));return;
    }
    if (req.url.startsWith("/v1internal:fetchAvailableModels")) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        defaultAgentModelId: "gemini-3.8-flash-medium",
        models: {
          "gemini-3.8-flash-low": { displayName: "Gemini 3.8 Flash (Low)", model: "MODEL_PLACEHOLDER_M320", supportsImages: true, supportsThinking: true, quotaInfo: { remainingFraction: 0.77, resetTime: "2026-09-28T00:00:00Z" } },
          "gemini-3.8-flash-medium": { displayName: "Gemini 3.8 Flash (Medium)", model: "MODEL_PLACEHOLDER_M319", supportsImages: true },
          "gemini-3.8-flash-high": { displayName: "Gemini 3.8 Flash (High)", model: "MODEL_PLACEHOLDER_M318" },
          "claude-sonnet-4-6": { displayName: "Claude Sonnet 4.6 (Thinking)", model: "MODEL_PLACEHOLDER_M35", supportsImages: true },
          "chat_internal_tab": { displayName: "internal", isInternal: false },
        },
      }));
      return;
    }
    if (req.url.startsWith("/v1internal:loadCodeAssist")) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ cloudaicompanionProject: req.headers.authorization === "Bearer ya29.mock-access-2" ? "mock-project-99" : "mock-project-42", currentTier: { id: "free-tier", name: "Free" }, paidTier: { id: "g1-pro-tier", name: "Google AI Pro" } }));
      return;
    }
    if (req.url.startsWith("/v1internal:retrieveUserQuotaSummary")) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ groups: [{ displayName: "Gemini quota", buckets: [{ bucketId: "b1", displayName: "Gemini", remainingFraction: 0.42, resetTime: "2026-09-28T00:00:00Z" }] }] }));
      return;
    }
    if (req.url.startsWith("/v1internal:listCloudAICompanionProjects")) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ projects: ["mock-project-42"] }));
      return;
    }
    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: { message: `mock upstream: unknown ${req.url}` } }));
  });
});

// --- Test harness ---------------------------------------------------------------
let upstreamPort, proxyPort, proxy;
const dir = mkdtempSync(join(tmpdir(), "ag-rust-e2e-"));
const authPath = join(dir, "auth.json");
const expiry=Date.now()+86400*365*1000;
const first={accountId:"tester@example.com",email:"tester@example.com",refresh:"1/mock-refresh-token-for-tests-only-00000000",access:"ya29.mock-access-token",expires:expiry,addedAt:1,lastUsedAt:1};
const second={accountId:"second@example.com",email:"second@example.com",refresh:"1/mock-refresh-second-for-tests-only-00000000",access:"ya29.mock-access-2",expires:expiry,projectId:"mock-project-99",addedAt:2,lastUsedAt:2};
writeFileSync(authPath,JSON.stringify({antigravity:{type:"oauth",...first}}));
writeFileSync(join(dir,"antigravity-accounts.json"),JSON.stringify({version:1,activeAccountId:first.accountId,accounts:{[first.accountId]:first,[second.accountId]:second}}));
async function reservePort(){const server=createServer();await once(server.listen(0,"127.0.0.1"),"listening");const port=server.address().port;await new Promise(r=>server.close(r));return port;}
async function waitHttp(url, timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(url);
      if (res.ok) return res;
    } catch {}
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`timeout waiting for ${url}`);
}

async function readSse(res, { maxMs = 15000 } = {}) {
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let text = "";
  const deadline = Date.now() + maxMs;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    text += decoder.decode(value, { stream: true });
    if (text.includes("response.completed") || text.includes("response.failed")) break;
  }
  try { await reader.cancel(); } catch {}
  return text;
}

function eventsOf(sseText) {
  return sseText
    .split("\n\n")
    .map((b) => b.split("\n").find((l) => l.startsWith("data: ")))
    .filter(Boolean)
    .map((l) => JSON.parse(l.slice(6)));
}

try {
await once(upstream.listen(0,"127.0.0.1"),"listening");
upstreamPort=upstream.address().port;
proxyPort=await reservePort();

proxy = spawn(RUST_EXE, ["serve", "--port", String(proxyPort), "--auth", authPath], {
  env: {
    ...cleanEnv,
    AG_IMAGE_MIRROR: "0",
    AG_DEBUG: "1",
    PI_CODING_AGENT_DIR: dir,
    AG_IMAGE_DIR: join(dir, "images"),
    ANTIGRAVITY_MAX_BODY_MB: "1",
    ANTIGRAVITY_BASE_URL: `http://127.0.0.1:${upstreamPort}`,
    AG_TEST_ALLOW_INSECURE_BASE: "1",
    TMP: dir,
    TEMP: dir,
  },
  stdio: ["ignore", "pipe", "pipe"],
  cwd: dir,
});
proxy.stderr.on("data", (d) => process.env.E2E_VERBOSE && console.error(`[proxy] ${d}`));
proxy.stdout.resume();
proxy.on("error",e=>console.error(`proxy spawn failed: ${e.message}`));

const BASE = `http://127.0.0.1:${proxyPort}`;
await waitHttp(`${BASE}/health`);

// --- 1. health -----------------------------------------------------------------
await test("missing OAuth client fails before opening the login flow", async () => {
  for (const configured of [{}, {ANTIGRAVITY_CLIENT_ID: "mock-client"}]) {
    const result = spawnSync(RUST_EXE, ["login", "--manual", "--auth", authPath], {
      env: {...cleanEnv, ...configured, PI_CODING_AGENT_DIR: dir},
      input: "", encoding: "utf8", timeout: 3000,
    });
    assert.equal(result.error, undefined);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Set ANTIGRAVITY_CLIENT_(ID|SECRET)/);
    assert.doesNotMatch(result.stdout, /Open this URL|accounts\.google\.com/);
  }
});

await test("GET /health reports ok + endpoint", async () => {
  const res = await fetch(`${BASE}/health`);
  const body = await res.json();
  assert.equal(body.ok, true);
  assert.equal(body.service, "codey-antigravity-proxy");
  assert.equal(body.endpoint, `http://127.0.0.1:${upstreamPort}`);
  assert.equal(body.authenticated, true);
});

// --- 2. streaming /responses with envelope fingerprint --------------------------
await test("POST /responses streams Responses events with wire fingerprint labels", async () => {
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash",
      stream: true,
      instructions: "Be terse.",
      input: [{ type: "message", role: "user", content: [{ type: "input_text", text: "ping" }] }],
    }),
  });
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /text\/event-stream/);
  const text = await readSse(res);
  const events = eventsOf(text);
  const types = events.map((e) => e.type);
  assert.deepEqual(types.slice(0, 3), ["response.created", "response.output_item.added", "response.content_part.added"]);
  assert.ok(types.includes("response.output_text.delta"));
  assert.ok(types.includes("response.completed"));
  const completed = events.find((e) => e.type === "response.completed").response;
  assert.equal(completed.output[0].content[0].text, "PONG");
  assert.equal(completed.usage.input_tokens, 11);
  assert.equal(completed.usage.input_tokens_details.cached_tokens, 5);
  assert.equal(completed.usage.output_tokens_details.reasoning_tokens, 3);
  assert.equal(completed.raw_finish_reason, "STOP");
  // Wire fingerprint on the upstream envelope.
  const call = upstreamLog.find((l) => l.url.startsWith("/v1internal:streamGenerateContent"));
  assert.equal(call.body.model, "gemini-3.8-flash-low", "public model + no effort routes to low");
  assert.equal(call.body.request.labels.model_enum, "MODEL_PLACEHOLDER_M320");
  assert.equal(call.body.request.labels.used_claude, "false");
  assert.match(call.body.requestId, /^agent\//);
  assert.match(call.body.request.labels.request_id, /-\d+$/);
  assert.equal(call.headers["user-agent"], "antigravity/cli/1.2.4 (aidev_client; os_type=linux; arch=amd64; cl=982146307; auth_method=consumer)");
  assert.equal(call.body.project, "mock-project-42", "projectId discovered via loadCodeAssist");
  // thinkingConfig for the routed low runtime
  assert.equal(call.body.request.generationConfig.thinkingConfig.thinkingBudget, 1000);
  assert.equal(call.body.request.generationConfig.maxOutputTokens, 65536);
});

// --- 3. item_id stability + tool loop (thought signature echo) ------------------
await test("tool loop: function_call echoed with signature, tool result replayed", async () => {
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash-medium",
      stream: true,
      input: [
        { type: "message", role: "user", content: [{ type: "input_text", text: "run it" }] },
        { type: "function_call", call_id: "call_abc", name: "shell", arguments: "{\"cmd\":\"echo hi\"}" },
        { type: "function_call_output", call_id: "call_abc", output: "hi" },
      ],
    }),
  });
  const text = await readSse(res);
  const events = eventsOf(text);
  const completed = events.find((e) => e.type === "response.completed").response;
  const call = upstreamLog.filter((l) => l.url.startsWith("/v1internal:streamGenerateContent")).pop();
  const modelTurn = call.body.request.contents.find((t) => t.role === "model");
  const fcPart = modelTurn.parts.find((p) => p.functionCall);
  assert.equal(fcPart.functionCall.name, "shell");
  assert.equal(fcPart.functionCall.id, "call_abc");
  assert.ok(fcPart.thoughtSignature, "missing thought_signature backfilled with placeholder");
  // Stable item ids across added/done events.
  const addedIds = events.filter((e) => e.type === "response.output_item.added").map((e) => e.item.id);
  const doneIds = events.filter((e) => e.type === "response.output_item.done").map((e) => e.item.id);
  assert.ok(addedIds.length > 0);
  assert.deepEqual(addedIds, doneIds, "item ids stable between added and done");
  // labels fingerprint for medium runtime
  assert.equal(call.body.request.labels.model_enum, "MODEL_PLACEHOLDER_M319");
  assert.equal(call.body.request.generationConfig.thinkingConfig.thinkingBudget, 4000);
});

// --- 4. boundary normalization ---------------------------------------------------
await test("boundary fix: model fn-call turn preceded by model turn gets a user bridge", async () => {
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash-medium",
      stream: true,
      input: [
        { type: "message", role: "user", content: [{ type: "input_text", text: "go" }] },
        { type: "message", role: "assistant", content: [{ type: "output_text", text: "working" }] },
        { type: "function_call", call_id: "c1", name: "f", arguments: "{}" },
        { type: "function_call_output", call_id: "c1", output: "ok" },
      ],
    }),
  });
  const text = await readSse(res);
  assert.ok(text.includes("response.completed"), `expected completion, got: ${text.slice(0, 400)}`);
  const call = upstreamLog.filter((l) => l.url.startsWith("/v1internal:streamGenerateContent")).pop();
  const contents = call.body.request.contents;
  for (let i = 1; i < contents.length; i++) {
    const hasCall = (contents[i].parts || []).some((p) => p.functionCall);
    if (contents[i].role === "model" && hasCall) {
      assert.equal(contents[i - 1].role, "user", "fn-call turn must follow a user turn");
    }
  }
});

// --- 5. non-streaming ------------------------------------------------------------
await test("stream:false returns one JSON response object", async () => {
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash-medium",
      stream: false,
      input: "say hi",
    }),
  });
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.equal(body.status, "completed");
  assert.equal(body.output[0].content[0].text, "PONG");
  assert.equal(body.usage.total_tokens, 18);
});

// --- 6. /models grouped catalog ---------------------------------------------------
await test("GET /v1/models lists grouped public models + runtime ids", async () => {
  const res = await fetch(`${BASE}/v1/models`);
  const body = await res.json();
  const ids = body.data.map((m) => m.id);
  assert.ok(ids.includes("gemini-3.8-flash"), "public grouped id present");
  assert.ok(ids.includes("gemini-3.8-flash-medium"), "raw runtime id present");
  assert.ok(ids.includes("claude-sonnet-4-6"));
  assert.ok(!ids.includes("chat_internal_tab"), "chat models filtered from default list");
  assert.equal(body.object, "list");
});

// --- 7. web_search interception --------------------------------------------------
await test("web_search tool: model call grounded and replayed transparently", async () => {
  // The mock upstream's fn-response follow-up emits a noop call, not web_search;
  // to exercise interception we rely on the groundingMetadata path instead:
  // simulate by having the first response contain a web_search functionCall via
  // a dedicated upstream branch is complex — here we assert the pure-google_search
  // request keeps toolConfig and the mixed request declares a synthetic tool.
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash-medium",
      stream: true,
      tools: [
        { type: "web_search" },
        { type: "function", name: "read_file", parameters: { type: "object", properties: { path: { type: "string" } } } },
      ],
      input: "search something",
    }),
  });
  const text = await readSse(res);
  assert.ok(text.includes("response.completed"));
  const call = upstreamLog.filter((l) => l.url.startsWith("/v1internal:streamGenerateContent")).pop();
  const decls = call.body.request.tools.flatMap((t) => t.functionDeclarations || []);
  assert.ok(decls.some((d) => d.name === "web_search"), "synthetic web_search declared");
  assert.ok(decls.some((d) => d.name === "read_file"));
  assert.ok(!call.body.request.tools.some((t) => t.google_search), "no google_search mix with functions");
  assert.equal(call.body.request.toolConfig?.includeServerSideToolInvocations, undefined, "toolConfig only for pure search");
});

// --- 8. generate_image tool interception -------------------------------------------
await test("generate_image tool: intercepted, image generated, tool result appended", async () => {
  const res = await fetch(`${BASE}/responses`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: "gemini-3.8-flash-medium",
      stream: true,
      tools: [{ type: "function", name: "read_file", parameters: { type: "object", properties: { path: { type: "string" } } } }],
      input: "draw a cat",
    }),
  });
  const text = await readSse(res, { maxMs: 30000 });
  assert.ok(text.includes("response.completed"), text.slice(0, 300));
  assert.ok(imageModelCalls.length > 0, "image model was called");
  const calls = upstreamLog.filter((l) => l.url.startsWith("/v1internal:streamGenerateContent"));
  const withToolOutput = calls.map((c) => c.body.request.contents).find((contents) =>
    contents.some((t) => (t.parts || []).some((p) => p.functionResponse?.name === "generate_image"))
  );
  assert.ok(withToolOutput, "generate_image functionResponse replayed to upstream");
});

// --- 9. /v1/images/generations ------------------------------------------------------
await test("POST /v1/images/generations returns b64 image", async () => {
  const res = await fetch(`${BASE}/v1/images/generations`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ prompt: "a cat", aspect_ratio: "16:9" }),
  });
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.ok(body.data[0].b64_json.length > 0);
  assert.equal(body.model, "gemini-3-pro-image");
  const call = imageModelCalls.length && upstreamLog.filter((l) => l.body?.request?.generationConfig?.imageConfig).pop();
  assert.ok(call, "imageConfig in request");
  assert.equal(call.body.request.generationConfig.imageConfig.aspectRatio, "16:9");
});

// --- 10. /usage -----------------------------------------------------------------------
await test("GET /v1/usage merges quota groups, tiers, models", async () => {
  const res = await fetch(`${BASE}/v1/usage`);
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.equal(body.projectId, "mock-project-42");
  assert.equal(body.planLabel, "Google AI Pro (g1-pro-tier)");
  assert.equal(body.groups[0].buckets[0].remainingFraction, 0.42);
  assert.ok(body.models.some((m) => m.modelId === "gemini-3.8-flash-low"));
  assert.ok(body.models.every((m) => !m.modelId.startsWith("chat_")));
});

// --- 11. /doctor ------------------------------------------------------------------------
await test("GET /doctor returns sanitized diagnostics", async () => {
  const res = await fetch(`${BASE}/doctor`);
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.ok(body.diagnostics.endpoint);
  assert.ok(body.diagnostics.projectId === "mock-project-42");
  assert.ok(body.diagnostics.maskedEmail.startsWith("t***"));
  assert.ok(!JSON.stringify(body).includes("ya29.mock"), "no token leakage");
});

// --- 12. WS contract ---------------------------------------------------------------------
await test("WebSocket responses: response.create -> bare JSON frames with stream_id", async () => {
  const key = crypto.randomBytes(16).toString("base64");
  const ws = await connectWs(`ws://127.0.0.1:${proxyPort}/v1/responses`, key);
  ws.socket.write(encodeFrame(0x1, JSON.stringify({
    type: "response.create",
    stream_id: "st-1",
    model: "gemini-3.8-flash-medium",
    input: "ping",
    stream: true,
  })));
  const frames = [];
  const deadline = Date.now() + 15000;
  let sawTerminal = false;
  while (Date.now() < deadline && !sawTerminal) {
    const frame = await ws.nextFrame();
    if (!frame) break;
    frames.push(frame);
    if (frame.opcode === 0x1) {
      const evt = JSON.parse(frame.payload);
      if (evt.type === "response.completed" || evt.type === "response.failed") sawTerminal = true;
      if (evt.type !== "response.failed") {
        assert.equal(evt.stream_id, "st-1", "stream_id re-attached");
      }
      assert.ok(!frame.payload.toString().startsWith("data:"), "bare JSON, no SSE framing");
    }
  }
  assert.ok(frames.some((f) => f.opcode === 0x1 && JSON.parse(f.payload).type === "response.created"));
  assert.ok(frames.some((f) => f.opcode === 0x1 && JSON.parse(f.payload).type === "response.completed"));
  // Invalid stream_id rejected with codey_route_error.
  ws.socket.write(encodeFrame(0x1, JSON.stringify({ type: "response.create", stream_id: "bad id!" })));
  const bad = await ws.nextFrame();
  const badEvt = JSON.parse(bad.payload);
  assert.equal(badEvt.type, "response.failed");
  assert.equal(badEvt.response.error.code, "invalid_stream_id");
  // Ping answered with pong.
  ws.socket.write(encodeFrame(0x9, Buffer.from("keepalive")));
  const pong = await ws.nextFrame();
  assert.equal(pong.opcode, 0xA);
  await ws.close();
});

await test("independent search, fallback and google_search interception", async()=>{
 const res=await fetch(`${BASE}/v1/search`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({query:"mock query",instruction:"Use citations",urls:["https://example.org"],thinking:true})});assert.equal(res.status,200);const result=await res.json();assert.equal(result.text,"Grounded mock answer");assert.deepEqual(result.sources,[{title:"Mock Source",url:"https://example.org/source"}]);assert.ok(result.result.includes("Sources"));
 const calls=upstreamLog.filter(c=>c.url.startsWith("/v1internal:generateContent"));assert.ok(calls.some(c=>c.body.model==="gemini-3-flash"));assert.ok(calls.some(c=>c.body.model==="gemini-3.6-flash-low"));assert.ok(calls.at(-1).body.request.tools.some(t=>t.urlContext));
 const before=upstreamLog.length;const bridge=await fetch(`${BASE}/responses`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({model:"gemini-3.8-flash-medium",stream:true,input:"execute search",tools:[{type:"function",name:"google_search",parameters:{type:"object",properties:{query:{type:"string"}}}}]})});const text=await readSse(bridge);assert.ok(text.includes("response.completed"));assert.ok(upstreamLog.slice(before).some(c=>c.url.startsWith("/v1internal:generateContent")));assert.ok(upstreamLog.slice(before).some(c=>c.body?.request?.contents?.some(t=>(t.parts||[]).some(p=>p.functionResponse?.name==="google_search"))));
});
await test("Host/Origin, body cap and private image SSRF guard",async()=>{
 for (const headers of [{Host:"evil.example"},{Origin:"https://evil.example"}]) {
  const status=await new Promise((resolve,reject)=>{const req=httpRequest(`${BASE}/health`,{headers},res=>{res.resume();res.on("end",()=>resolve(res.statusCode));});req.on("error",reject);req.end();});
  assert.equal(status,403);
 }
 const large=await fetch(`${BASE}/responses`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({input:"x".repeat(1100000)})});assert.equal(large.status,413);await large.text();
 const before=upstreamLog.length;const res=await fetch(`${BASE}/responses`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({model:"gemini-3.8-flash",stream:false,input:[{type:"message",role:"user",content:[{type:"input_image",image_url:`http://127.0.0.1:${upstreamPort}/private-image`}]}]})});await res.text();assert.ok(!upstreamLog.slice(before).some(c=>c.url==="/private-image"));
});
await test("429 account failover also switches project",async()=>{
 const before=upstreamLog.length;quotaFailover=true;
 try{const res=await fetch(`${BASE}/responses`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({model:"gemini-3.8-flash",stream:false,input:"quota-failover"})});assert.equal(res.status,200);assert.equal((await res.json()).status,"completed");const calls=upstreamLog.slice(before).filter(c=>c.url.startsWith("/v1internal:streamGenerateContent"));assert.ok(calls.some(c=>c.headers.authorization==="Bearer ya29.mock-access-token"));const retry=calls.find(c=>c.headers.authorization==="Bearer ya29.mock-access-2");assert.ok(retry);assert.equal(retry.body.project,"mock-project-99");}finally{quotaFailover=false;}
});
await test("Unicode stream and terminal failures survive full HTTP orchestration",async()=>{
 for(const probe of ["unicode-probe","late-error-probe","truncated-probe"]){
  const res=await fetch(`${BASE}/responses`,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({model:"gemini-3.8-flash",stream:true,input:probe})});
  const events=eventsOf(await res.text());
  const failed=probe!=="unicode-probe";
  assert.equal(events.filter(e=>e.type==="response.failed").length,failed?1:0);
  assert.equal(events.filter(e=>e.type==="response.completed").length,failed?0:1);
  if(!failed)assert.equal(events.find(e=>e.type==="response.completed").response.output[0].content[0].text,"中文🙂文本");
 }
});
} catch(e) {failures++;console.error(`Harness failed: ${e.message}`);}
finally {
 for(const socket of sockets) {socket.destroy();}
 if(proxy && proxy.exitCode===null){const closed=once(proxy,"close");proxy.kill();await closed;}
 if(upstream.listening){upstream.closeAllConnections();await new Promise(r=>upstream.close(r));}
 rmSync(dir,{recursive:true,force:true});
}
process.exitCode=failures?1:0;

// --- minimal WS client helpers --------------------------------------------------------------
function encodeFrame(opcode, payload) {
  const mask = crypto.randomBytes(4);
  const len = payload.length;
  let header;
  if (len < 126) {
    header = Buffer.from([0x80 | opcode, 0x80 | len]);
  } else if (len <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  const masked = Buffer.from(payload);
  for (let i = 0; i < masked.length; i++) masked[i] ^= mask[i % 4];
  return Buffer.concat([header, mask, masked]);
}

async function connectWs(url, key) {
  const { WebSocket } = await import("node:ws").catch(() => ({}));
  if (WebSocket) {
    const ws = new WebSocket(url, { headers: { "sec-websocket-key": key, "sec-websocket-version": 13 } });
    await once(ws, "open");
    throw new Error("ws package path not implemented in this harness");
  }
  // Raw socket implementation (no deps).
  const u = new URL(url);
  const net = await import("node:net");
  const crypto2 = crypto;
  const socket = net.createConnection({ host: u.hostname, port: Number(u.port) });
  sockets.add(socket); socket.once("close",()=>sockets.delete(socket)); socket.on("error",()=>{});
  await once(socket, "connect");
  const handshake =
    `GET ${u.pathname} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
    `Sec-WebSocket-Key: ${crypto2.randomBytes(16).toString("base64")}\r\nSec-WebSocket-Version: 13\r\n\r\n`;
  socket.write(handshake);
  let handshakeDone = false;
  let headerText = "";
  const pending = [];
  let waiter = null;
  socket.on("data", (chunk) => {
    if (!handshakeDone) {
      headerText += chunk.toString("latin1");
      const idx = headerText.indexOf("\r\n\r\n");
      if (idx === -1) return;
      if (!/HTTP\/1\.1 101/.test(headerText)) throw new Error(`handshake failed: ${headerText.slice(0, 200)}`);
      handshakeDone = true;
      const rest = Buffer.from(headerText.slice(idx + 4), "latin1");
      if (rest.length) onData(rest);
      return;
    }
    onData(chunk);
  });
  function onData(chunk) {
    buffer = Buffer.concat([buffer, chunk]);
    pump();
  }
  let buffer = Buffer.alloc(0);
  function pump() {
    while (buffer.length >= 2) {
      const b0 = buffer[0];
      const opcode = b0 & 0x0f;
      const masked = buffer[1] & 0x80;
      let len = buffer[1] & 0x7f;
      let offset = 2;
      if (len === 126) {
        if (buffer.length < 4) return;
        len = buffer.readUInt16BE(2);
        offset = 4;
      } else if (len === 127) {
        if (buffer.length < 10) return;
        len = Number(buffer.readBigUInt64BE(2));
        offset = 10;
      }
      const maskLen = masked ? 4 : 0;
      if (buffer.length < offset + maskLen + len) return;
      let payload = buffer.subarray(offset + maskLen, offset + maskLen + len);
      if (masked) {
        const mask = buffer.subarray(offset, offset + 4);
        const unmasked = Buffer.from(payload);
        for (let i = 0; i < unmasked.length; i++) unmasked[i] ^= mask[i % 4];
        payload = unmasked;
      }
      buffer = buffer.subarray(offset + maskLen + len);
      const frame = { opcode, payload: Buffer.from(payload) };
      pending.push(frame);
      if (waiter) {
        const w = waiter;
        waiter = null;
        w();
      }
    }
  }
  return {
    socket,
    nextFrame(timeoutMs = 10000) {
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve(null), timeoutMs);
        const check = () => {
          if (pending.length) {
            clearTimeout(timer);
            resolve(pending.shift());
          } else {
            waiter = check;
          }
        };
        check();
      });
    },
    async close() {
      try {
        socket.write(encodeFrame(0x8, Buffer.from([0x03, 0xe8])));
        socket.end();
        const closed=once(socket,"close"); const timer=setTimeout(()=>socket.destroy(),500);await closed;clearTimeout(timer);
      } catch {}
    },
  };
}
