import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";
import {
  autoStubModule,
  collectElements,
  createComponentModule,
  elementProps,
  elementType,
  textContent,
} from "./helpers/jsx-tree.mjs";

const [dialogSource, appSource, stylesSource, operationsSource] = await Promise.all([
  readSource("src/SystemSettingsDialog.tsx"),
  readSource("src/App.tsx"),
  readSource("src/styles.css"),
  readSource("src/OperationsPanel.tsx"),
]);

// 组件库与图标按名字生成桩：断言的是真实组件渲染出的元素树（含事件接线），
// 而不是源码里的字符串，因此把 onClick/onCheckedChange 改成空操作一定会失败。
const ui = autoStubModule("ui");
const hero = autoStubModule("heroui");
const icons = autoStubModule("icon");
const { exports: dialog, reset } = await createComponentModule(
  new URL("../src/SystemSettingsDialog.tsx", import.meta.url),
  { stubs: { "./components/ui": ui, "@heroui/react": hero, "@tabler/icons-react": icons } },
);

const clipboardWrites = [];
Object.defineProperty(globalThis, "navigator", {
  configurable: true,
  value: {
    clipboard: {
      writeText(value) {
        clipboardWrites.push(value);
        return Promise.resolve();
      },
    },
  },
});

function renderDialog(overrides = {}) {
  reset();
  const calls = { repair: 0, autoCheck: [] };
  const props = {
    open: true,
    onOpenChange: () => {},
    container: null,
    appVersion: "1.1.1",
    codexAppVersion: "0.4.0",
    codexAppPath: "/Applications/ChatGPT.app",
    isBusy: false,
    busy: null,
    onRepairCodexConfig: () => {
      calls.repair += 1;
    },
    configRepairNotice: null,
    codeyUpdatePolicy: "stable",
    onCodeyUpdatePolicyChange: (checked) => {
      calls.autoCheck.push(checked);
    },
    ...overrides,
  };
  return { calls, props, tree: dialog.SystemSettingsDialog(props) };
}

const byLabel = (tree, label) =>
  collectElements(tree, (element) => elementProps(element)["aria-label"] === label);
const byText = (tree, text) =>
  collectElements(
    tree,
    (element) =>
      typeof elementProps(element).onClick === "function" &&
      textContent(element).replace(/\s+/g, "") === text,
  );
const policies = (tree) => collectElements(tree, element => elementType(element) === hero.Tabs);

async function deviceDialog(invoke, overrides = {}) {
  const module = await createComponentModule(
    new URL("../src/SystemSettingsDialog.tsx", import.meta.url),
    { stubs: { "./components/ui": ui, "@heroui/react": hero, "@tabler/icons-react": icons, "./api": { invoke } } },
  );
  let effect;
  module.react.useEffect = callback => { effect = callback; };
  const props = { ...renderDialog().props, ...overrides };
  const render = () => {
    module.restart();
    return module.exports.SystemSettingsDialog(props);
  };
  const tree = render();
  let cleanup;
  return {
    tree,
    props,
    render,
    runEffect: () => {
      cleanup?.();
      cleanup = effect();
      return cleanup;
    },
  };
}

test("设备号按接口原值显示和复制，反馈与目录复制独立", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  clipboardWrites.length = 0;
  const machineNo = "m_release_admin_AbC-123_XyZ";
  const calls = [];
  const fixture = await deviceDialog(async command => { calls.push(command); return machineNo; });
  assert.match(textContent(fixture.tree), /读取中/);
  assert.equal(elementProps(byLabel(fixture.tree, "复制设备号")[0]).disabled, true);
  fixture.runEffect();
  await new Promise(setImmediate);
  const tree = fixture.render();
  assert.deepEqual(calls, ["get_device_machine_no"]);
  assert.ok(textContent(tree).includes(machineNo));
  const button = byLabel(tree, "复制设备号")[0];
  assert.equal(elementProps(button).disabled, false);
  await elementProps(button).onClick();
  assert.deepEqual(clipboardWrites, [machineNo]);
  const copiedTree = fixture.render();
  assert.equal(collectElements(byLabel(copiedTree, "复制设备号")[0], e => elementType(e) === icons.IconCheck).length, 1);
  assert.equal(collectElements(byLabel(copiedTree, "复制 Codex 路径")[0], e => elementType(e) === icons.IconCheck).length, 0);
  t.mock.timers.tick(2000);
  assert.equal(collectElements(byLabel(fixture.render(), "复制设备号")[0], e => elementType(e) === icons.IconCheck).length, 0);
});

test("设备号读取失败可重试，未登记时不可复制占位文字", async () => {
  let attempts = 0;
  const fixture = await deviceDialog(async () => {
    if (++attempts === 1) throw new Error("offline");
    return null;
  });
  fixture.runEffect();
  await new Promise(setImmediate);
  const failedTree = fixture.render();
  assert.match(textContent(failedTree), /读取失败/);
  assert.equal(byLabel(failedTree, "复制设备号").length, 0);
  elementProps(byLabel(failedTree, "重新读取设备号")[0]).onClick();
  fixture.render();
  fixture.runEffect();
  await new Promise(setImmediate);
  const emptyTree = fixture.render();
  assert.equal(attempts, 2);
  assert.match(textContent(emptyTree), /尚未登记/);
  assert.equal(elementProps(byLabel(emptyTree, "复制设备号")[0]).disabled, true);
  clipboardWrites.length = 0;
  await elementProps(byLabel(emptyTree, "复制设备号")[0]).onClick();
  assert.deepEqual(clipboardWrites, []);
});

test("设备号显示具体错误原因，空错误使用可理解的提示", async () => {
  for (const [error, expected] of [
    [new Error("无法读取设备信息，请检查文件访问权限。"), "无法读取设备信息，请检查文件访问权限。"],
    ["设备登记失败，请检查网络连接后重试。", "设备登记失败，请检查网络连接后重试。"],
    [new Error("  "), "暂时无法读取设备号，请稍后重试。"],
    [null, "暂时无法读取设备号，请稍后重试。"],
  ]) {
    const fixture = await deviceDialog(async () => { throw error; });
    fixture.runEffect();
    await new Promise(setImmediate);
    const tree = fixture.render();
    const status = collectElements(tree, element => elementProps(element)["aria-live"] === "polite")[0];
    assert.ok(textContent(status).includes("读取失败"));
    assert.ok(textContent(status).includes(expected));
    assert.equal(byLabel(tree, "重新读取设备号").length, 1);
    assert.equal(byLabel(tree, "复制设备号").length, 0);
  }
});

for (const lateOutcome of ["成功", "失败"]) {
  test(`设备号超时后可重试，忽略旧请求的迟到${lateOutcome}`, async (t) => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const pending = [];
    const fixture = await deviceDialog(() => new Promise((resolve, reject) => {
      pending.push({ resolve, reject });
    }));
    fixture.runEffect();
    t.mock.timers.tick(19999);
    assert.match(textContent(fixture.render()), /读取中/);
    t.mock.timers.tick(1);
    const timedOut = fixture.render();
    assert.match(textContent(timedOut), /读取失败/);
    assert.match(textContent(timedOut), /读取超时，请检查网络连接后重试/);
    elementProps(byLabel(timedOut, "重新读取设备号")[0]).onClick();
    fixture.render();
    fixture.runEffect();
    if (lateOutcome === "成功") pending[0].resolve("m_old_response");
    else pending[0].reject(new Error("旧请求失败"));
    await new Promise(setImmediate);
    const retrying = fixture.render();
    assert.equal(pending.length, 2);
    assert.match(textContent(retrying), /读取中/);
    assert.doesNotMatch(textContent(retrying), /m_old_response|旧请求失败|读取超时|读取失败/);
    pending[1].resolve("m_retry_success");
    await new Promise(setImmediate);
    t.mock.timers.tick(20000);
    const completed = fixture.render();
    assert.match(textContent(completed), /m_retry_success/);
    assert.doesNotMatch(textContent(completed), /读取失败|读取超时/);
    assert.equal(elementProps(byLabel(completed, "复制设备号")[0]).disabled, false);
  });
}

test("设备号超时后，原请求迟到成功也不会替换超时提示", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let resolve;
  const fixture = await deviceDialog(() => new Promise(done => { resolve = done; }));
  fixture.runEffect();
  t.mock.timers.tick(20000);
  resolve("m_timed_out_response");
  await new Promise(setImmediate);
  const tree = fixture.render();
  assert.match(textContent(tree), /读取超时/);
  assert.doesNotMatch(textContent(tree), /m_timed_out_response/);
});

test("关闭的弹窗不请求设备号，关闭后清理等待并忽略迟到结果", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const clearTimer = t.mock.method(globalThis, "clearTimeout");
  const closed = await deviceDialog(() => { throw new Error("不应请求"); }, { open: false });
  closed.runEffect();
  let resolve;
  const fixture = await deviceDialog(() => new Promise(done => { resolve = done; }));
  fixture.runEffect();
  fixture.props.open = false;
  fixture.render();
  fixture.runEffect();
  assert.equal(clearTimer.mock.callCount(), 1);
  t.mock.timers.tick(20000);
  resolve("m_late_response");
  await new Promise(setImmediate);
  assert.doesNotMatch(textContent(fixture.render()), /m_late_response|读取失败|读取超时/);
});

test("设备号复制失败不显示成功反馈", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  t.mock.method(navigator.clipboard, "writeText", async () => { throw new Error("denied"); });
  const fixture = await deviceDialog(async () => "m_registered");
  fixture.runEffect();
  await new Promise(setImmediate);
  await elementProps(byLabel(fixture.render(), "复制设备号")[0]).onClick();
  const tree = fixture.render();
  assert.equal(collectElements(byLabel(tree, "复制设备号")[0], e => elementType(e) === icons.IconCheck).length, 0);
  assert.equal(collectElements(tree, e => elementProps(e).content === "复制失败，请选中设备号手动复制").length, 1);
});

test("复制路径、修复配置与更新策略都接到真实回调", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  clipboardWrites.length = 0;
  const { calls, tree } = renderDialog();
  const text = textContent(tree);
  assert.match(text, /v1\.1\.1/);
  assert.match(text, /v0\.4\.0/);
  assert.match(text, /\/Applications\/ChatGPT\.app/);

  const [copyButton] = byLabel(tree, "复制 Codex 路径");
  assert.ok(copyButton, "复制按钮必须带 aria-label");
  await elementProps(copyButton).onClick();
  assert.deepEqual(clipboardWrites, ["/Applications/ChatGPT.app"]);

  // 未配置 Codex 路径时复制默认路径，按钮依旧可用。
  clipboardWrites.length = 0;
  const fallback = renderDialog({ codexAppPath: "" });
  await elementProps(byLabel(fallback.tree, "复制 Codex 路径")[0]).onClick();
  assert.deepEqual(clipboardWrites, ["/Applications/ChatGPT.app"]);

  const [repairButton] = byText(tree, "修复");
  assert.ok(repairButton, "修复按钮必须存在");
  assert.equal(elementProps(repairButton).disabled, false);
  elementProps(repairButton).onClick();
  assert.equal(calls.repair, 1, "点击修复必须触发修复回调");

  const [slider] = policies(tree);
  assert.ok(slider);
  assert.equal(elementProps(slider).selectedKey, "stable");
  assert.equal(elementType(byLabel(tree, "Codey 更新策略")[0]), hero.Tabs.List);
  const tabs = collectElements(slider, element => elementType(element) === hero.Tabs.Tab);
  assert.deepEqual(tabs.map(element => [elementProps(element).id, textContent(element)]), [
    ["off", "不检查"], ["stable", "稳定版"], ["experimental", "实验版"],
  ]);
  assert.ok(tabs.every(element => elementProps(element).isDisabled === false));
  assert.equal(collectElements(slider, element => elementType(element) === hero.Tabs.Indicator).length, 3);
  for (const value of ["off", "stable", "experimental"]) elementProps(slider).onSelectionChange(value);
  assert.deepEqual(calls.autoCheck, ["off", "stable", "experimental"]);
  assert.equal(elementProps(slider).selectedKey, "stable");
  assert.ok(
    collectElements(tree, (e) => /仍可手动检查稳定版/.test(elementProps(e).content ?? "")).length > 0 ||
    /仍可手动检查稳定版/.test(textContent(tree)),
  );
});

test("更新策略跟随配置、修复中显示进行态", () => {
  const off = renderDialog({ codeyUpdatePolicy: "off" });
  assert.equal(elementProps(policies(off.tree)[0]).selectedKey, "off");

  const busy = renderDialog({ busy: "repair-codex-config", isBusy: true });
  const [busyButton] = byText(busy.tree, "修复中…");
  assert.ok(busyButton, "修复进行中必须显示进行态");
  assert.equal(elementProps(busyButton).disabled, true);
  assert.ok(collectElements(policies(busy.tree)[0], element => elementType(element) === hero.Tabs.Tab).every(element => elementProps(element).isDisabled));
  assert.equal(busy.calls.repair, 0);
});

test("系统设置不出现冗余状态文案", () => {
  const { tree } = renderDialog();
  const text = textContent(tree);
  assert.doesNotMatch(text, /当前应用/);
  assert.doesNotMatch(text, /运行中/);
  assert.doesNotMatch(text, /检查配置文件并修复异常/);
  assert.doesNotMatch(text, /后台每隔 30 分钟自动检查新版本/);
  assert.doesNotMatch(dialogSource, /检查配置文件并修复异常/);
});

test("App 把系统设置对话框接到修复命令与自动更新保存入口", () => {
  assert.match(appSource, /onRepairCodexConfig=\{askRepairCodexConfig\}/);
  assert.match(appSource, /onCodeyUpdatePolicyChange=\{changeUpdatePolicy\}/);
  assert.match(appSource, /codeyUpdatePolicy=\{config\.codeyUpdatePolicy\}/);
  assert.match(appSource, /invoke<[\s\S]{0,240}?>\("repair_codex_config"\)/);
  assert.match(appSource, /className="sidebar-footer-bar"/);
  assert.match(appSource, /className="sidebar-footer-left"/);
  assert.doesNotMatch(appSource, /className="sidebar-footer-brand"/);
  assert.match(appSource, /className="sidebar-footer-version font-mono"/);
  assert.match(appSource, /<IconCircleArrowUp size=\{15\}/);
  assert.match(appSource, /handleFooterUpdateClick/);
  assert.match(appSource, /className="sidebar-footer-right"/);
  assert.match(appSource, /setSystemSettingsOpen\(true\)/);
  assert.match(appSource, /<SystemSettingsDialog[\s\S]*open=\{systemSettingsOpen\}/);
});

test("实验版必须先确认，取消不改设置，切回稳定版无需确认", async () => {
  const { default: ts } = await import("typescript");
  const handler = appSource.slice(appSource.indexOf("  function changeUpdatePolicy("), appSource.indexOf("  async function persist("));
  let config = { codeyUpdatePolicy: "stable" };
  let confirmation = null;
  const factory = new Function("config", "isBusy", "editConfig", "setConfirmation", "updateDraft", ts.transpile(handler) + "\nreturn changeUpdatePolicy;");
  const change = factory(config, false, value => { config = value; }, value => { confirmation = value; }, update => { config = update(config); });
  change("experimental");
  assert.equal(config.codeyUpdatePolicy, "stable");
  assert.match(confirmation.description, /尚未经过充分验证/);
  assert.match(confirmation.description, /备份/);
  confirmation = null;
  assert.equal(config.codeyUpdatePolicy, "stable");
  change("experimental");
  confirmation.run();
  assert.equal(config.codeyUpdatePolicy, "experimental");
  confirmation = null;
  change("stable");
  assert.equal(config.codeyUpdatePolicy, "stable");
  assert.equal(confirmation, null);
});

test("styles define sidebar footer layout and responsive behavior", () => {
  assert.match(stylesSource, /\.sidebar-footer-bar\s*\{/);
  assert.match(stylesSource, /\.sidebar-footer-left\s*\{/);
  assert.match(stylesSource, /\.sidebar-footer-right\s*\{/);
  assert.match(stylesSource, /\.sidebar-footer-version\s*\{/);
});

test("OperationsPanel removes redundant top hero card and config repair button", () => {
  assert.doesNotMatch(operationsSource, /operations-hero-card/);
  assert.doesNotMatch(operationsSource, /Codex 运行状态/);
  assert.doesNotMatch(operationsSource, /修复 Codex 配置/);
  assert.doesNotMatch(operationsSource, /expanded-tray-toolbar/);
  assert.match(operationsSource, /已生效 \$\{enabledOptimizationFeatures\.length\} 项功能/);
});
