import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

test("notices are consumed once across remounts and repeated effects", async () => {
  const shown = [];
  let effect;
  const react = {
    memo: (component) => component,
    useEffect: (callback) => { effect = callback; },
    useSyncExternalStore: (_subscribe, snapshot) => snapshot(),
  };
  async function load(path, imports) {
    const source = await readFile(new URL(path, import.meta.url), "utf8");
    const exports = {};
    new Function("require", "exports", ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText)((name) => imports[name] ?? {}, exports);
    return exports;
  }
  const { createExternalStore } = await load("../src/externalStore.ts", { react });
  const { NoticeToast } = await load("../src/useAppNotice.tsx", {
    react,
    "@heroui/react": { toast: Object.fromEntries(
      ["success", "danger", "info"].map((tone) => [tone, (text) => shown.push([tone, text])]),
    ) },
  });
  const controller = createExternalStore({ tone: "success", text: "操作完成" });
  const mount = () => { NoticeToast({ controller }); effect(); };
  mount();
  effect();
  mount();
  assert.deepEqual(shown, [["success", "操作完成"]]);
  assert.equal(controller.getSnapshot().text, "");

  controller.set({ tone: "success", text: "操作完成" });
  mount();
  controller.set({ tone: "error", text: "同步失败" });
  mount();
  mount();
  assert.deepEqual(shown, [
    ["success", "操作完成"], ["success", "操作完成"], ["danger", "同步失败"],
  ]);
});
