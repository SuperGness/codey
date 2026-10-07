import assert from "node:assert/strict";
import test from "node:test";
import { readSource } from "./helpers/read-source.mjs";
import {
  autoStubModule, collectElements, createComponentModule, elementProps, elementType, textContent,
} from "./helpers/jsx-tree.mjs";

const ui = autoStubModule("ui");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const stopped = { running: false, urls: [], devices: [] };
const publicUrl = "https://remote.example.test";
const localUrl = "http://192.168.1.8:43129";
const running = { running: true, urls: [publicUrl, localUrl], devices: [], tunnel: { status: "running", message: "HTTPS 隧道已开启" } };
const pairing = { url: `${publicUrl}/#pair=${"a".repeat(64)}`, qrCode: "data:image/png;base64,fixture", expiresIn: 600 };
const flush = () => new Promise(setImmediate);
const nodes = (tree, predicate) => collectElements(tree, node => predicate(elementProps(node), elementType(node)));
const button = (tree, label) => nodes(tree, (props, type) => type === ui.Button && textContent(props.children) === label)[0];
const input = (tree, label) => {
  const field = nodes(tree, (props, type) => type === "label" && textContent(props.children).startsWith(label))[0];
  return nodes(field, (_props, type) => type === ui.Input)[0];
};

async function fixture(t, initial = stopped, options = {}) {
  let status = structuredClone(initial);
  const calls = [];
  let poll;
  let cleared = false;
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    __codeyRemoteClient: options.remote ?? false,
    setInterval(callback) { poll = callback; return 1; },
    clearInterval() { cleared = true; },
  } });
  t.after(() => {
    if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
    else delete globalThis.window;
  });
  const module = await createComponentModule(new URL("../src/RemoteControlPanel.tsx", import.meta.url), {
    stubs: {
      "./components/ui": ui,
      "@tabler/icons-react": autoStubModule("icons"),
      "./api": { invoke: async (command, args) => {
        calls.push({ command, args });
        if (options.invoke) return options.invoke(command, args);
        if (command === "remote_control_status") return structuredClone(status);
        if (command === "start_remote_control") { status = structuredClone(running); return status; }
        if (command === "stop_remote_control") { status = structuredClone(stopped); return; }
        if (command === "pair_remote_control") return { ...pairing, url: `${args.url}/#pair=${"a".repeat(64)}` };
        if (command === "revoke_remote_device") status.devices = status.devices.filter(device => device.id !== args.id);
      } },
    },
  });
  let effect;
  module.react.useEffect = callback => { effect = callback; };
  const render = () => { module.restart(); return module.exports.RemoteControlPanel({ active: options.active ?? true }); };
  render();
  const cleanup = effect();
  await flush();
  return {
    render, calls, cleanup,
    setStatus(next) { status = next; },
    async poll() { await poll?.(); },
    get cleared() { return cleared; },
    async click(label) {
      const target = button(render(), label);
      assert.ok(target, `button ${label} exists`);
      assert.equal(elementProps(target).disabled, false);
      elementProps(target).onClick();
      await flush();
    },
    changeMode(value) { elementProps(nodes(render(), props => props.type === "radio" && props.value === value)[0]).onChange(); },
  };
}

test("未开启时展示完整引导，配对不可用，网络设置默认收起", async t => {
  const f = await fixture(t);
  const tree = f.render();
  assert.match(textContent(tree), /未开启/);
  assert.match(textContent(tree), /还没有配对设备/);
  assert.equal(elementProps(button(tree, "生成一次性配对码")).disabled, true);
  assert.equal(nodes(tree, (_props, type) => type === "details")[0].props.open, undefined);
  assert.equal(nodes(tree, props => props.type === "radio").length, 3);
});

test("三种连接方式使用原有接口，运行时锁定网络配置", async t => {
  for (const mode of ["tunnel", "lan", "custom"]) {
    const f = await fixture(t);
    f.changeMode(mode);
    if (mode === "custom") elementProps(input(f.render(), "外网 HTTPS 地址")).onChange({ target: { value: publicUrl } });
    await f.click("开启远程控制");
    assert.deepEqual(f.calls.find(call => call.command === "start_remote_control").args, {
      bindAddress: "0.0.0.0", port: 43129, publicUrl: mode === "custom" ? publicUrl : "", tunnel: mode === "tunnel",
    });
    const tree = f.render();
    assert.match(textContent(tree), /服务已开启/);
    assert.equal(nodes(tree, (_props, type) => type === "fieldset")[0].props.disabled, true);
    assert.equal(elementProps(input(tree, "端口")).disabled, true);
    f.cleanup();
  }
});

test("错误端口与空自定义地址阻止开启并显示可读错误", async t => {
  for (const value of ["0", "65536", "1.5", "text", ""]) {
    const f = await fixture(t);
    elementProps(input(f.render(), "端口")).onChange({ target: { value } });
    await f.click("开启远程控制");
    assert.match(textContent(nodes(f.render(), props => props.role === "alert")[0]), /1–65535/);
    assert.equal(f.calls.some(call => call.command === "start_remote_control"), false);
  }
  const f = await fixture(t);
  f.changeMode("custom");
  await f.click("开启远程控制");
  assert.match(textContent(f.render()), /请输入已转发到本机服务的 HTTPS 地址/);
});

test("开启过程中锁定操作并显示加载状态，完成后恢复控件", async t => {
  let resolve;
  const f = await fixture(t, stopped, { invoke: command => command === "remote_control_status"
    ? Promise.resolve(stopped)
    : new Promise(done => { resolve = done; }) });
  elementProps(button(f.render(), "开启远程控制")).onClick();
  const pending = elementProps(button(f.render(), "开启远程控制"));
  assert.equal(pending.loading, true);
  assert.equal(pending.disabled, true);
  assert.equal(elementProps(input(f.render(), "端口")).disabled, true);
  resolve(running);
  await flush();
  assert.equal(elementProps(button(f.render(), "关闭远程控制")).disabled, false);
});

test("配对、切换地址、刷新失效地址和关闭服务均更新二维码", async t => {
  const f = await fixture(t, running);
  await f.click("生成一次性配对码");
  assert.equal(nodes(f.render(), (_props, type) => type === "img")[0].props.src, pairing.qrCode);
  assert.equal(elementProps(input(f.render(), "配对链接")).value, pairing.url);
  const select = nodes(f.render(), (_props, type) => type === "select")[0];
  elementProps(select).onChange({ target: { value: localUrl } });
  assert.equal(nodes(f.render(), (_props, type) => type === "img").length, 0);
  await f.click("生成一次性配对码");
  f.setStatus({ ...running, urls: [publicUrl] });
  // Header actions are passed as a prop to SettingsPageHeader.
  const header = nodes(f.render(), props => props.id === "remote-control-title")[0];
  elementProps(button(header.props.actions, "刷新状态")).onClick();
  await flush();
  assert.equal(nodes(f.render(), (_props, type) => type === "img").length, 0);
  await f.click("生成一次性配对码");
  await f.click("关闭远程控制");
  assert.equal(nodes(f.render(), (_props, type) => type === "img").length, 0);
  assert.equal(elementProps(button(f.render(), "生成一次性配对码")).disabled, true);
});

test("隧道等待时不可自动配对，失败后仍可选择局域网地址", async t => {
  const f = await fixture(t, { ...running, urls: [localUrl], tunnel: { status: "starting", message: "正在连接" } });
  assert.equal(elementProps(button(f.render(), "生成一次性配对码")).disabled, true);
  f.setStatus({ ...running, urls: [localUrl], tunnel: { status: "failed", message: "外网连接失败" } });
  await f.poll();
  assert.match(textContent(f.render()), /外网连接失败/);
  assert.equal(elementProps(button(f.render(), "生成一次性配对码")).disabled, false);
  await f.click("生成一次性配对码");
  assert.equal(f.calls.find(call => call.command === "pair_remote_control").args.url, localUrl);
});

test("撤销设备更新设备列表，接口错误展示后可重试", async t => {
  const f = await fixture(t, { ...running, devices: [{ id: "device-1", name: "我的手机" }] });
  await f.click("撤销");
  assert.equal(f.calls.find(call => call.command === "revoke_remote_device").args.id, "device-1");
  assert.match(textContent(f.render()), /还没有配对设备/);
  let attempts = 0;
  const failed = await fixture(t, stopped, { invoke: async command => {
    if (command === "remote_control_status") return stopped;
    if (++attempts === 1) throw new Error("端口已被占用");
    return running;
  } });
  await failed.click("开启远程控制");
  assert.match(textContent(failed.render()), /端口已被占用/);
  await failed.click("开启远程控制");
  assert.equal(nodes(failed.render(), props => props.role === "alert").length, 0);
});

test("手机端和非活动页面不请求状态；卸载取消轮询并忽略迟到响应", async t => {
  for (const options of [{ remote: true }, { active: false }]) {
    const f = await fixture(t, stopped, options);
    assert.equal(f.calls.length, 0);
    if (options.remote) {
      assert.match(textContent(f.render()), /此设备已连接/);
      assert.equal(nodes(f.render(), (_props, type) => type === ui.Button).length, 0);
    }
  }
  let resolve;
  const f = await fixture(t, stopped, { invoke: () => new Promise(done => { resolve = done; }) });
  f.cleanup();
  resolve(running);
  await flush();
  assert.equal(f.cleared, true);
  assert.doesNotMatch(textContent(f.render()), /服务已开启/);
});

test("内嵌入口注册远程页面样式，避免普通 CSS 导入被 ShadowRoot 隔离", async () => {
  const source = await readSource("src/overlay.tsx");
  assert.match(source, /import remoteControlStyles from "\.\/remote-control\.css\?inline"/);
  assert.match(source, /shadowStyleSheet\([\s\S]*?remoteControlStyles[\s\S]*?\)/);
});
