import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/conversation-git.js", import.meta.url), "utf8");
const flush = async () => { for (let i = 0; i < 15; i++) await Promise.resolve(); };
const deferred = () => { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; };

function harness({ enabled = true, visible = true, optimizer = true, status, preview, execute, now = () => Date.now() } = {}) {
  class Element extends FakeElementCore {
    get classList() {
      const classes = () => new Set((this.className || "").split(/\s+/).filter(Boolean));
      return {
        add: (...names) => { this.className = [...new Set([...classes(), ...names])].join(" "); },
        remove: (...names) => { this.className = [...classes()].filter((name) => !names.includes(name)).join(" "); },
        contains: (name) => classes().has(name),
      };
    }
    replaceChildren(...children) { [...this.children].forEach((child) => child.remove()); this.append(...children); }
    getBoundingClientRect() { return { left: 100, top: 600 }; }
    focus() { document.activeElement = this; }
  }
  const document = new Element("document");
  document.createElement = (tag) => new Element(tag);
  document.documentElement = new Element("html");
  document.body = new Element("body");
  document.documentElement.appendChild(document.body);
  document.getElementById = (id) => document.documentElement.querySelector(`#${id}`);
  const host = new Element("div"), anchor = new Element("button");
  host.appendChild(anchor); document.body.appendChild(host);
  const optimize = new Element("button"); optimize.id = "codey-prompt-optimize-button";
  if (optimizer) host.appendChild(optimize);
  const state = { sessionId: "session-a", enabled, visible, contextAvailable: true, targetAvailable: true };
  const window = new Element("window");
  window.innerWidth = 1200; window.innerHeight = 800;
  window.location = { href: "https://codex.local/session-a" };
  window.__codeyPromptOptimize = { composerContext: () => state.contextAvailable
    ? { sessionId: state.sessionId, target: state.targetAvailable ? { host, anchor } : null } : null };
  const calls = [];
  window.__codexSessionDeleteBridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/settings/get") return { conversationGit: { enabled: state.enabled } };
    if (path.endsWith("_status")) return status ? status(payload) : { visible: state.visible, reason: "没有文件改动" };
    if (path.endsWith("_preview")) return preview ? preview(payload) : {
      token: "preview-token", files: ["owned.txt"], diff: "-old\n+new", message: "fix(conversation-git): 校验当前对话提交范围", branch: "main",
    };
    if (path.endsWith("_execute")) return execute ? execute(payload) : {
      status: payload?.push === false ? "committed" : "pushed",
      message: payload?.push === false ? "当前对话文件已提交到本地仓库" : "当前对话文件已提交并推送",
      commit: "abc123",
    };
    throw new Error(`unexpected path: ${path}`);
  };
  const timers = new Map(), intervals = [];
  let timerId = 0;
  let mutation;
  window.__codeyMutationDispatcher = { subscribe(handler) { mutation = handler; } };
  const runTimers = async () => {
    const queued = [...timers.entries()];
    for (const [id, { fn }] of queued) {
      if (!timers.delete(id)) continue;
      fn();
    }
    await flush();
  };
  vm.runInNewContext(source, { window, document,
    Date: class extends Date { static now() { return now(); } },
    setTimeout: (fn, delay) => { const id = ++timerId; timers.set(id, { fn, delay }); return id; },
    clearTimeout: (id) => timers.delete(id), setInterval: (fn) => intervals.push(fn), console });
  return {
    window, document, calls, state, host, optimize,
    button: () => document.getElementById("codey-conversation-git"),
    panel: () => document.getElementById("codey-conversation-git-panel"),
    async tick() { intervals.forEach((fn) => fn()); await flush(); },
    async navigate(id) { state.sessionId = id; mutation([{ target: host }]); await runTimers(); },
    mutate: (mutations = [{ target: host }]) => mutation(mutations),
    runTimers,
    timerDelays: () => [...timers.values()].map(({ delay }) => delay),
    async click(node) { node.dispatchEvent({ type: "click", preventDefault() {}, stopPropagation() {} }); await flush(); },
    async load() { await flush(); },
  };
}

test("shows only enabled conversations with backend-confirmed Git changes beside optimize", async () => {
  for (const [enabled, visible] of [[false, true], [true, false], [true, true]]) {
    const env = harness({ enabled, visible }); await env.load();
    assert.equal(env.window.__codeyConversationGit.snapshot().visible, enabled && visible);
    if (enabled && visible) assert.equal(env.optimize.nextElementSibling, env.button());
    if (!enabled) assert.equal(env.calls.filter((call) => call.path.endsWith("_status")).length, 0);
  }
});

test("keeps confirmed visibility through brief composer loss and blocks stale actions", async () => {
  const env = harness(); await env.load();
  const button = env.button();
  env.state.contextAvailable = false;
  env.mutate();
  assert.equal(button.style.display, "inline-flex");
  assert.equal(button.disabled, true);
  assert.equal(env.window.__codeyConversationGit.snapshot().sessionId, "session-a");
  await env.click(button);
  assert.equal(env.calls.some((call) => call.path.endsWith("_preview")), false);
  env.state.contextAvailable = true;
  env.mutate(); await env.runTimers();
  assert.equal(button.style.display, "inline-flex");
  assert.equal(button.disabled, false);
  assert.equal(env.timerDelays().includes(100), false);
});

test("restores the Git button when the native toolbar removes only the injected button", async () => {
  const env = harness(); await env.load();
  const button = env.button();
  button.remove();
  env.mutate([{ type: "childList", target: env.host, addedNodes: [], removedNodes: [button] }]);
  assert.equal(env.button(), button);
  assert.equal(button.style.display, "inline-flex");
});

test("keeps the button during temporary missing toolbar or conversation ID", async () => {
  const env = harness(); await env.load();
  env.state.targetAvailable = false;
  env.mutate();
  assert.equal(env.button().style.display, "inline-flex");
  env.state.targetAvailable = true;
  env.state.sessionId = null;
  env.mutate();
  assert.equal(env.button().style.display, "inline-flex");
  env.state.sessionId = "session-a";
  env.mutate(); await env.runTimers();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
});

test("a pending status result does not hide the button during toolbar rebuilding", async () => {
  const pending = deferred();
  let count = 0;
  const env = harness({ status: () => ++count === 1 ? { visible: true } : pending.promise });
  await env.load();
  await env.tick();
  env.state.targetAvailable = false;
  env.mutate();
  pending.resolve({ visible: true }); await flush();
  assert.equal(env.button().style.display, "inline-flex");
  assert.equal(env.button().disabled, true);
  env.state.targetAvailable = true;
  env.mutate();
  assert.equal(env.button().disabled, false);
});

test("hides after persistent context loss or navigation to another route", async () => {
  let now = 0;
  const env = harness({ now: () => now }); await env.load();
  env.state.contextAvailable = false;
  env.mutate();
  now = 1_001;
  await env.runTimers();
  assert.equal(env.button().style.display, "none");
  assert.equal(env.window.__codeyConversationGit.snapshot().sessionId, null);
  env.state.contextAvailable = true;
  env.mutate(); await env.runTimers();
  assert.equal(env.button().style.display, "inline-flex");
  env.window.location.href = "https://codex.local/session-b";
  env.state.contextAvailable = false;
  env.mutate();
  assert.equal(env.button().style.display, "none");
});

test("preserves recent visibility on query failure but hides confirmed absence", async () => {
  let result = { visible: true };
  const env = harness({ status: () => {
    if (result instanceof Error) throw result;
    return result;
  } }); await env.load();
  result = new Error("桥接暂时不可用");
  await env.tick();
  assert.equal(env.button().style.display, "inline-flex");
  result = { visible: false, unavailable: true, reason: "读取会话暂时失败" };
  await env.tick();
  assert.equal(env.button().style.display, "inline-flex");
  result = { visible: false, reason: "没有文件改动" };
  await env.tick();
  assert.equal(env.button().style.display, "none");
});

test("does not keep a failed query's visibility beyond the cache lifetime", async () => {
  let now = 0, fail = false;
  const env = harness({ now: () => now, status: () => {
    if (fail) throw new Error("暂时失败");
    return { visible: true };
  } }); await env.load();
  fail = true; now = 30_001;
  await env.tick();
  assert.equal(env.button().style.display, "none");
});

test("works when prompt optimization is disabled and requires preview confirmation before execution", async () => {
  const env = harness({ optimizer: false }); await env.load();
  await env.click(env.button());
  assert.deepEqual(env.panel().querySelectorAll("pre").map((node) => node.textContent), ["fix(conversation-git): 校验当前对话提交范围", "-old\n+new"]);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
  const actionLabels = env.panel().querySelectorAll("button").map((node) => node.textContent);
  assert.ok(actionLabels.includes("取消"));
  assert.ok(actionLabels.includes("提交"));
  assert.ok(actionLabels.includes("提交并推送"));
  const confirm = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交并推送");
  await env.click(confirm);
  const execution = env.calls.find((call) => call.path.endsWith("_execute"));
  assert.equal(JSON.stringify(execution.payload), JSON.stringify({ sessionId: "session-a", token: "preview-token", push: true }));
  assert.equal(env.panel().querySelector("p").textContent, "当前对话文件已提交并推送");
});

test("allows user to choose commit only without pushing", async () => {
  const env = harness(); await env.load();
  await env.click(env.button());
  const commitOnly = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交");
  await env.click(commitOnly);
  const execution = env.calls.find((call) => call.path.endsWith("_execute"));
  assert.equal(JSON.stringify(execution.payload), JSON.stringify({ sessionId: "session-a", token: "preview-token", push: false }));
  assert.equal(env.panel().querySelector("p").textContent, "当前对话文件已提交到本地仓库");
});

test("opening an old conversation shows recovered changes without requiring a new edit", async () => {
  const env = harness({ visible: false }); await env.load();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  env.state.visible = true;
  await env.navigate("old-conversation");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  assert.equal(env.optimize.nextElementSibling, env.button());
  assert.equal(env.calls.at(-1).payload.sessionId, "old-conversation");
  env.state.visible = false;
  await env.navigate("conversation-without-changes");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});

test("lets the current conversation display before an old status request settles", async () => {
  const first = deferred(), latest = deferred();
  const env = harness({ status: ({ sessionId }) => sessionId === "session-a" ? first.promise : latest.promise });
  await env.load();
  await env.navigate("session-b");
  const checks = env.calls.filter((call) => call.path.endsWith("_status"));
  assert.deepEqual(checks.map((call) => call.payload.sessionId), ["session-a", "session-b"]);
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  latest.resolve({ visible: true }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  first.resolve({ visible: false }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  assert.equal(env.calls.filter((call) => call.path.endsWith("_status")).length, 2);
});

test("bounds status queries and checks only the latest queued conversation", async () => {
  const first = deferred(), second = deferred();
  const env = harness({ status: ({ sessionId }) => sessionId === "session-a" ? first.promise
    : sessionId === "session-b" ? second.promise : { visible: true } });
  await env.load();
  await env.navigate("session-b");
  await env.navigate("session-c");
  await env.navigate("session-d");
  assert.equal(env.calls.filter((call) => call.path.endsWith("_status")).length, 2);
  first.resolve({ visible: true }); await flush();
  assert.deepEqual(env.calls.filter((call) => call.path.endsWith("_status")).map((call) => call.payload.sessionId),
    ["session-a", "session-b", "session-d"]);
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  second.resolve({ visible: false }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
});

test("restores a recently visible conversation immediately and revalidates it", async () => {
  const pending = deferred();
  let first = true;
  const env = harness({ status: ({ sessionId }) => {
    if (sessionId === "session-a" && !first) return pending.promise;
    first = false;
    return { visible: true };
  } });
  await env.load();
  await env.navigate("session-b");
  env.state.sessionId = "session-a";
  env.mutate();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  assert.deepEqual(env.timerDelays(), [0]);
  await env.runTimers();
  pending.resolve({ visible: false }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  await env.navigate("session-b");
  env.state.sessionId = "session-a";
  env.mutate();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});

test("expires cached visibility and remounts a confirmed button without another status response", async () => {
  let now = 0;
  const env = harness({ now: () => now }); await env.load();
  const button = env.button();
  button.remove();
  env.mutate([{ type: "childList", target: env.host, addedNodes: [], removedNodes: [button, env.optimize] }]);
  assert.equal(env.button(), button);
  assert.equal(button.style.display, "inline-flex");
  await env.navigate("session-b");
  now = 30_001;
  env.state.sessionId = "session-a";
  env.mutate();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});

test("rechecks the same session after returning while its previous query is pending", async () => {
  const pending = deferred();
  let count = 0;
  const env = harness({ status: ({ sessionId }) => sessionId === "session-a" && ++count === 1
    ? pending.promise : { visible: true } });
  await env.load();
  await env.navigate("session-b");
  await env.navigate("session-a");
  pending.resolve({ visible: false }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  assert.deepEqual(env.calls.filter((call) => call.path.endsWith("_status")).map((call) => call.payload.sessionId),
    ["session-a", "session-b", "session-a"]);
});

test("invalidates cached visibility and pending status results after committing", async () => {
  const pending = deferred();
  let count = 0;
  const env = harness({ status: () => ++count === 3 ? pending.promise : { visible: count < 3 } });
  await env.load();
  await env.navigate("session-b");
  await env.navigate("session-a");
  await env.click(env.button());
  const commitOnly = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交");
  await env.click(commitOnly);
  pending.resolve({ visible: true }); await flush();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  env.state.sessionId = "session-b";
  env.mutate();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});

test("navigation promotes a queued background refresh and ignores its own DOM insertion", async () => {
  const env = harness(); await env.load();
  env.mutate();
  assert.deepEqual(env.timerDelays(), [300]);
  env.state.sessionId = "session-b";
  env.mutate();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  assert.deepEqual(env.timerDelays(), [0]);
  await env.runTimers();
  assert.equal(env.calls.at(-1).payload.sessionId, "session-b");
  env.mutate([{ type: "childList", target: env.host, addedNodes: [env.button()], removedNodes: [] }]);
  assert.deepEqual(env.timerDelays(), []);
});

test("resumes the current conversation check after an old preview finishes", async () => {
  const pending = deferred();
  const env = harness({ preview: () => pending.promise }); await env.load();
  await env.click(env.button());
  await env.navigate("session-b");
  assert.equal(env.calls.at(-1).payload.sessionId, "session-b");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  pending.resolve({ token: "old", files: [], message: "旧对话", diff: "" }); await flush();
  assert.equal(env.panel(), null);
  assert.equal(env.calls.at(-1).payload.sessionId, "session-b");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
});

test("previews shared files as partial commits and requires confirmation", async () => {
  const env = harness({ preview: () => ({ token: "split", files: ["shared.rs"], partialFiles: ["shared.rs"],
    message: "fix(conversation-git): 拆分共享文件改动", diff: "-old\n+mine", branch: "main" }) });
  await env.load(); await env.click(env.button());
  assert.ok(env.panel().querySelectorAll("p").some((node) => node.textContent.includes("其他改动继续保留在工作区：shared.rs")));
  assert.deepEqual(env.panel().querySelectorAll("pre").map((node) => node.textContent), ["fix(conversation-git): 拆分共享文件改动", "-old\n+mine"]);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("discards an old conversation's pending preview after navigation", async () => {
  const pending = deferred();
  const env = harness({ preview: () => pending.promise }); await env.load();
  await env.click(env.button());
  await env.navigate("session-b");
  pending.resolve({ token: "old", files: ["old.txt"], message: "旧对话", diff: "old diff" }); await flush();
  assert.equal(env.panel(), null);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("renders model and stale-file failures as text without executing or changing the composer", async () => {
  const env = harness({ preview: () => ({ status: "failed", message: "文件已变化，请重新预览 <script>" }) }); await env.load();
  await env.click(env.button());
  assert.equal(env.panel().querySelector("[role=alert]").textContent, "文件已变化，请重新预览 <script>");
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("keeps the local commit outcome visible when remote push is rejected and prevents double submission", async () => {
  const pending = deferred();
  const env = harness({ execute: () => pending.promise }); await env.load();
  await env.click(env.button());
  const confirm = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交并推送");
  await env.click(confirm); await env.click(confirm);
  assert.equal(env.calls.filter((call) => call.path.endsWith("_execute")).length, 1);
  pending.resolve({ status: "committed", commit: "abc", message: "本地提交已生成，但推送失败" }); await flush();
  assert.equal(env.panel().querySelector("p").textContent, "本地提交已生成，但推送失败");
});

test("turning off the enhancement closes previews and hides the button", async () => {
  const env = harness(); await env.load(); await env.click(env.button());
  env.state.enabled = false; env.window.dispatchEvent({ type: "codey:config-changed" }); await flush();
  assert.equal(env.panel(), null);
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});
