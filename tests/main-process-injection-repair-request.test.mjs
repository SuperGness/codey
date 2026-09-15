import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const root = new URL("../", import.meta.url);

async function loadCommonJsModule(url, dependencies = {}) {
  const source = await readFile(url, "utf8");
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  const exports = {};
  new Function("require", "exports", "module", compiled)((name) => {
    assert.ok(name in dependencies, name);
    return dependencies[name];
  }, exports, { exports });
  return exports;
}

const api = await loadCommonJsModule(new URL("src/api.ts", root));
const { repairOperationResult } = await loadCommonJsModule(
  new URL("src/injectionRepair.ts", root),
  {
    "./api": api,
    "./appUtils": await loadCommonJsModule(new URL("src/appUtils.ts", root)),
  },
);

test("bridge failures carry an explicit error type", async () => {
  globalThis.window = {
    __codeyInvokeApi: async (command) => {
      if (command === "runtime_status") {
        return { status: "failed", code: "bridge_request_failed", message: "主进程注入正常，无需修复" };
      }
      return { status: "ok" };
    },
  };
  await assert.rejects(
    () => api.invoke("runtime_status"),
    (error) => api.isCodeyApiError(error)
      && error.message === "主进程注入正常，无需修复",
  );
  assert.deepEqual(await api.invoke("clear_diagnostic_storage"), { status: "ok" });
  globalThis.window = {};
  await assert.rejects(
    () => api.invoke("runtime_status"),
    (error) => !api.isCodeyApiError(error) && /bridge 尚未连接/.test(error.message),
  );
});

test("repair request failures stay separate from connection loss", () => {
  const rejected = repairOperationResult(new api.CodeyApiError("尚未确认主进程注入异常"));
  assert.equal(rejected.tone, "error");
  assert.equal(rejected.unconfirmed, false);
  assert.match(rejected.text, /修复请求被拒绝：尚未确认主进程注入异常/);

  const lost = repairOperationResult(new Error("修复请求暂未确认，请稍后重新查询状态"));
  assert.equal(lost.tone, "info");
  assert.equal(lost.unconfirmed, true);
  assert.match(lost.text, /修复请求结果待确认/);
});
