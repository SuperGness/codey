import assert from "node:assert/strict";
import test from "node:test";

import { createModuleGraph } from "./helpers/jsx-tree.mjs";

// useExtensionsController 用极简 hooks 桩驱动：挂载 effect、状态更新后手动重渲，
// 用来断言读写序号互不作废与 run 的三态返回值，而不是匹配源码。
const controllerUrl = new URL(
  "../src/features/codex-extensions/useExtensionsController.ts",
  import.meta.url,
);

const settle = async () => {
  for (let index = 0; index < 5; index += 1) await Promise.resolve();
};

function createHarness() {
  const reads = [];
  const writes = [];
  const effects = [];
  const hooks = [];
  let cursor = 0;

  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          hooks[index] = typeof next === "function" ? next(hooks[index]) : next;
        },
      ];
    },
    useRef(initial) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = { current: initial };
      return hooks[index];
    },
    useCallback(callback, deps) {
      const index = cursor++;
      const previous = hooks[index];
      // 依赖变化时必须换新闭包，否则挂载 effect 不会因 scope 变化而重跑。
      if (
        previous &&
        deps?.every((value, i) => Object.is(value, previous.deps[i]))
      )
        return previous.value;
      hooks[index] = { deps, value: callback };
      return callback;
    },
    useEffect(effect, deps) {
      const index = cursor++;
      const previous = hooks[index];
      if (previous && deps.every((value, i) => Object.is(value, previous.deps[i])))
        return;
      effects.push(() => {
        previous?.cleanup?.();
        hooks[index] = { deps, cleanup: effect() };
      });
    },
  };

  const graph = createModuleGraph(controllerUrl, {
    stubs: {
      react,
      "./requests": {
        readInventory: (_request, scope, force) =>
          new Promise((resolve, reject) => reads.push({ force, reject, resolve, scope })),
        invalidateInventory: () => {},
        withTimeout: (promise) => promise,
      },
      "./state": {
        causeText: (cause) =>
          cause instanceof Error ? cause.message : String(cause),
      },
    },
  });

  const request = (payload) =>
    new Promise((resolve, reject) => writes.push({ payload, reject, resolve }));

  const render = (active = true) => {
    cursor = 0;
    const controller = graph.exports.useExtensionsController(request, active);
    effects.splice(0).forEach((effect) => effect());
    return controller;
  };

  return { reads, render, writes };
}

test("提交不会作废在飞的清单刷新，加载态由刷新生命周期管理", async () => {
  const h = createHarness();
  const mounted = h.render();
  assert.equal(h.reads.length, 1, "挂载即开始读取清单");

  const saving = mounted.mutate({ action: "save_mcp" });
  assert.equal(h.writes.length, 1);
  assert.equal(h.render().loading, true, "提交不得清掉刷新中的加载态");

  h.reads[0].resolve({ revision: "r1" });
  await settle();
  assert.deepEqual(h.render().inventory, { revision: "r1" }, "提交不得作废在飞的清单");

  h.writes[0].resolve({ inventory: { revision: "r2" }, message: "已保存" });
  assert.equal(await saving, "ok");
  const settled = h.render();
  assert.equal(settled.loading, false);
  assert.deepEqual(settled.inventory, { revision: "r2" });
  assert.match(settled.notice, /已保存/);
});

test("提交被抢占返回 superseded，并提示刷新确认", async () => {
  const h = createHarness();
  let controller = h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  controller = h.render();

  const saving = controller.mutate({ action: "save_mcp" });
  controller.setScope({ kind: "project", projectPath: "/tmp/demo" });
  controller = h.render();
  assert.equal(h.reads.length, 2, "切换范围必须重新读取");
  h.reads[1].resolve({ revision: "r2" });
  await settle();

  h.writes[0].resolve({ inventory: { revision: "r2" }, message: "已保存" });
  assert.equal(await saving, "superseded", "落库但被抢占必须与失败区分开");
  controller = h.render();
  assert.deepEqual(controller.inventory, { revision: "r2" });
  assert.match(controller.notice, /操作已提交，请刷新确认/);
});

test("提交失败返回 failed 并保留错误，单飞与不确定状态仍然拦截 mutation", async () => {
  const h = createHarness();
  let controller = h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  controller = h.render();

  const first = controller.run({ action: "save_mcp" });
  assert.equal(await controller.run({ action: "save_mcp" }), "failed");
  assert.equal(h.writes.length, 1, "同一时刻只允许一次操作");

  h.writes[0].reject(new Error("上次操作结果尚不确定，请刷新确认。"));
  assert.equal(await first, "failed");
  controller = h.render();
  assert.match(controller.error, /结果尚不确定/);
  assert.equal(controller.uncertain, true);

  assert.equal(await controller.run({ action: "save_mcp" }), "failed");
  assert.equal(h.writes.length, 1, "结果不确定时不得继续提交修改");
  controller.run({ action: "get_mcp", id: "demo" });
  assert.equal(h.writes.length, 2, "只读操作不受不确定状态阻塞");
});

test("保存提示按 applyStatus 区分，重启提示不重复后端消息", async () => {
  const h = createHarness();
  let controller = h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  controller = h.render();

  const saving = controller.mutate({ action: "save_skill" });
  h.writes[0].resolve({
    inventory: { revision: "r2" },
    applyStatus: "restart-required",
    message: "配置已保存。",
  });
  assert.equal(await saving, "ok");
  controller = h.render();
  assert.equal(
    controller.notice,
    "配置已保存，重启 Codex 后生效。",
  );
  assert.equal(controller.noticeSeq, 1);

  // 自动刷新成功的状态直接使用后端说明，不追加重启提示。
  const applying = controller.mutate({ action: "save_mcp" });
  controller = h.render();
  h.writes[1].resolve({
    inventory: { revision: "r3" },
    applyStatus: "applied",
    message: "配置已保存，下一轮对话生效。",
  });
  assert.equal(await applying, "ok");
  controller = h.render();
  assert.equal(
    controller.notice,
    "配置已保存，下一轮对话生效。",
  );
  assert.equal(controller.noticeSeq, 2);
});

test("只标记正在操作的资源，异常后释放锁和标记", async () => {
  const h = createHarness();
  h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  const checking = h.render().inspect({ action: "test_mcp", id: "docs" });
  assert.equal(h.render().busyResourceId, "docs");
  assert.equal(h.render().busyAction, "test_mcp");
  assert.equal(await h.render().run({ action: "read_skill", id: "other" }), "failed");
  h.writes[0].reject(new Error("连接失败"));
  assert.equal(await checking, "failed");
  assert.equal(h.render().busyResourceId, "");
  assert.equal(h.render().busy, false);
});

test("保存成功后晚到的旧清单不能覆盖最新状态", async () => {
  const h = createHarness();
  const controller = h.render();
  const saving = controller.mutate({ action: "save_mcp", id: "docs" });
  h.writes[0].resolve({ inventory: { revision: "r2" }, message: "已保存" });
  assert.equal(await saving, "ok");
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  assert.equal(h.render().inventory.revision, "r2");
  assert.equal(h.render().loading, false);
});

test("预览是只读操作，修改状态不确定时仍可预览且切换范围丢弃旧结果", async () => {
  const h = createHarness();
  h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  const mutation = h.render().mutate({ action: "install_skill" });
  h.writes[0].reject(new Error("结果尚不确定"));
  await mutation;
  let received = false;
  const controller = h.render();
  const previewing = controller.run({ action: "preview_skill_install", sourcePath: "/tmp/skill" }, () => { received = true; });
  assert.equal(h.writes.length, 2);
  controller.setScope({ kind: "project", projectPath: "/project" });
  h.render();
  h.writes[1].resolve({ sourceRevision: "source-1" });
  assert.equal(await previewing, "superseded");
  assert.equal(received, false);
  assert.equal(h.render().busyResourceId, "");
});

test("提交前开始的旧读取不能解除提交超时后的保护", async () => {
  const h = createHarness();
  const controller = h.render();
  const saving = controller.mutate({ action: "save_skill" });
  h.writes[0].reject(new Error("操作超时，结果尚不确定"));
  await saving;
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  assert.equal(h.render().uncertain, true);
  const refreshed = h.render().refresh();
  h.reads[1].resolve({ revision: "r2" });
  await refreshed;
  assert.equal(h.render().uncertain, false);
});
test("预览输入已改变时忽略迟到错误并释放操作锁", async () => {
  const h = createHarness();
  h.render();
  h.reads[0].resolve({ revision: "r1" });
  await settle();
  let current = true;
  const preview = h.render().run(
    { action: "preview_skill_install", sourcePath: "/tmp/old" },
    undefined,
    () => current,
  );
  current = false;
  h.writes[0].reject(new Error("旧来源不存在"));
  assert.equal(await preview, "superseded");
  assert.equal(h.render().error, "");
  assert.equal(h.render().busy, false);
  const retry = h.render().run({ action: "preview_skill_install", sourcePath: "/tmp/new" });
  h.writes[1].reject(new Error("新来源无法读取"));
  assert.equal(await retry, "failed");
  assert.equal(h.render().error, "新来源无法读取");
});
