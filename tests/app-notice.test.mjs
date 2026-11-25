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

test("config apply failures render via danger toast with deduplication, cleanup, and retry action", async () => {
  const events = [];
  let nextToastId = 0;
  let effects = [];
  let cleanups = [];
  const refs = [];
  const react = {
    memo: (component) => component,
    useRef: (initial) => {
      const index = refs.cursor++;
      if (!(index in refs)) refs[index] = { current: initial };
      return refs[index];
    },
    useEffect: (callback) => { effects.push(callback); },
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
  const { ConfigApplyErrorToast } = await load("../src/useAppNotice.tsx", {
    react,
    "@heroui/react": {
      toast: {
        danger: (title, options) => {
          const id = `toast-${++nextToastId}`;
          events.push(["danger", id, title, options]);
          return id;
        },
        close: (id) => { events.push(["close", id]); },
      },
    },
  });

  function render(props) {
    refs.cursor = 0;
    effects = [];
    ConfigApplyErrorToast(props);
    // Run main effect (index 0); keep unmount effect (index 1) for final teardown.
    const cleanup = effects[0]?.();
    if (cleanup) cleanups.push(cleanup);
    if (cleanups.length === 0 && effects[1]) cleanups.push(effects[1]());
  }

  // Does not fire before active && ready, then fires once ready.
  const failed = { revision: 3, state: "failed", error: "子代理模型不可用" };
  render({ active: false, ready: false, configApply: failed });
  render({ active: true, ready: false, configApply: failed });
  assert.deepEqual(events, []);

  render({ active: true, ready: true, configApply: failed });
  render({ active: true, ready: true, configApply: { ...failed } });
  assert.equal(events.length, 1);
  assert.equal(events[0][0], "danger");
  assert.equal(events[0][2], "配置已保存，但未能完全应用");
  assert.equal(events[0][3].description, "子代理模型不可用");

  // Recovery closes active toast and resets deduplication.
  render({ active: true, ready: true, configApply: { revision: 4, state: "applied" } });
  assert.deepEqual(events.at(-1), ["close", "toast-1"]);

  // Polling error shows retry action and invokes onRetry on press.
  let retries = 0;
  render({
    active: true,
    ready: true,
    configApply: { revision: 5, state: "pending" },
    configApplyError: "配置已保存，暂时无法确认是否应用，请重新查询状态",
    onRetry: () => { retries += 1; },
  });
  const pollEvent = events.at(-1);
  assert.equal(pollEvent[0], "danger");
  assert.equal(pollEvent[2], "配置应用状态未确认");
  assert.equal(pollEvent[3].actionProps.children, "重新查询状态");
  pollEvent[3].actionProps.onPress();
  assert.equal(retries, 1);

  // Closing the modal dismisses the toast without re-firing on the same error until resolved.
  render({
    active: false,
    ready: false,
    configApply: { revision: 5, state: "pending" },
    configApplyError: "配置已保存，暂时无法确认是否应用，请重新查询状态",
  });
  assert.deepEqual(events.at(-1), ["close", "toast-2"]);
});
