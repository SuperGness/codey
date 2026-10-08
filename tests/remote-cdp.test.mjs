import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Session } from "node:inspector";
import test from "node:test";

const source = readFileSync(new URL("../backend/src/cdp.rs", import.meta.url), "utf8");
const remoteSource = source.slice(source.indexOf("pub(crate) async fn remote_control_request("));
const template = remoteSource.match(/let script = format!\(\s*r#"([\s\S]*?)"#/)[1];

function remoteScript(args) {
  const substitutions = {
    "{{": "{", "}}": "}",
    "{args}": JSON.stringify(args),
    "{expires_at}": String(Date.now() + 60_000),
  };
  return template.replace(/\{\{|\}\}|\{args\}|\{expires_at\}/g, token => substitutions[token]);
}

async function evaluateRemote(args, body) {
  const session = new Session();
  session.connect();
  const evaluate = expression => new Promise((resolve, reject) => {
    // Match the shared bridge: objects are returned as references unless the
    // remote wrapper explicitly serializes them. A hand-written CDP response
    // with an object in result.value would conceal this regression.
    session.post("Runtime.evaluate", {
      expression, awaitPromise: true, allowUnsafeEvalBlockedByCSP: true,
    }, (error, response) => error ? reject(error) : resolve(response));
  });
  try {
    await evaluate(`globalThis.window = { calls: 0, args: null };
      ${body === null ? "" : `window.__codeyRemoteControl = async (args) => {
        window.calls += 1; window.args = args; ${body}
      };`} void 0;`);
    const response = await evaluate(remoteScript(args));
    const observed = await evaluate("JSON.stringify({ calls: window.calls, args: window.args })");
    assert.equal(response.exceptionDetails, undefined);
    assert.equal(response.result.type, "string", "remote results must cross CDP by value");
    return { result: JSON.parse(response.result.value), ...JSON.parse(observed.result.value) };
  } finally {
    await evaluate("delete globalThis.window");
    session.disconnect();
  }
}

test("opening an existing remote conversation returns its acknowledgement through CDP", async () => {
  const args = { action: "resume", threadId: "fixture-thread" };
  const result = await evaluateRemote(args, 'return { status: "ok" };');
  assert.deepEqual(result, { result: { status: "ok" }, calls: 1, args });
});

test("the first remote message returns its conversation and turn status through CDP", async () => {
  const args = { action: "create", params: { input: [{ type: "text", text: '测试 "引号"\n下一行' }] } };
  const result = await evaluateRemote(args, 'return { id: "fixture-thread", firstTurn: "accepted" };');
  assert.deepEqual(result, {
    result: { id: "fixture-thread", firstTurn: "accepted" }, calls: 1, args,
  });
});

test("a missing desktop adapter returns a readable failure through CDP", async () => {
  const result = await evaluateRemote({ action: "resume", threadId: "fixture-thread" }, null);
  assert.equal(result.calls, 0);
  assert.deepEqual(result.result, {
    status: "failed", message: "桌面会话接口尚未就绪，请更新或重启 Codey",
  });
});

test("native and serialization failures are sanitized without replaying the operation", async () => {
  for (const body of [
    'throw new Error("private native detail");',
    "return { unencodable: 1n };",
  ]) {
    const result = await evaluateRemote({ action: "create", params: {} }, body);
    assert.equal(result.calls, 1);
    assert.deepEqual(result.result, {
      status: "failed", message: "桌面未确认远程操作，请检查会话状态后重试",
    });
  }
});
