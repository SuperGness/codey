import assert from "node:assert/strict";
import { File } from "node:buffer";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const pluginHelpers = await loadTypeScriptModule(new URL("../src/codeyPlugins.ts", import.meta.url));
const importHelpers = await loadTypeScriptModule(new URL("../src/pluginPackageImport.ts", import.meta.url));
const sources = await Promise.all(["CodeyPluginsSection", "PluginImportPanel"].map(async name => [name, ts.transpileModule(await readFile(new URL(`../src/${name}.tsx`, import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText]));
const flush = () => new Promise(resolve => setImmediate(resolve));
const hash = "a".repeat(64);
const preview = (version = "1.0.0") => ({ uploadId: "upload-demo", path: "staged.codey-plugin", sha256: hash, manifest: { id: "demo", name: "Demo", version, capabilities: [] } });
const plugin = (version = "1.0.0") => ({ id: "demo", name: "Demo", version, status: "disabled", enabled: false, configPath: "config.json", capabilities: [] });
const result = (plugins = [], platform = "windows") => ({ plugins, platform, arch: "x86_64" });
const text = node => node == null || typeof node === "boolean" ? "" : Array.isArray(node) ? node.map(text).join("") : typeof node === "object" ? text(node.props?.children) : String(node);

function flowHarness(context) {
  const previousWindow = globalThis.window;
  const target = Object.assign(new EventTarget(), { innerWidth: 1000, innerHeight: 800, setTimeout, clearTimeout });
  globalThis.window = target;
  const fibers = new Map(), calls = [], notices = [];
  let current, cursor, tree, staleWrites = 0;
  let visible = true;
  const jsx = (type, props, key) => ({ type, props, key });
  const same = (left, right) => left && right && left.length === right.length && left.every((value, index) => Object.is(value, right[index]));
  const react = {
    useState(initial) {
      const fiber = current, index = cursor++;
      if (!(index in fiber.hooks)) {
        const cell = { value: typeof initial === "function" ? initial() : initial };
        cell.set = value => {
          if (!fiber.mounted) { staleWrites++; return; }
          cell.value = typeof value === "function" ? value(cell.value) : value;
        };
        fiber.hooks[index] = cell;
      }
      const cell = fiber.hooks[index];
      return [cell.value, cell.set];
    },
    useRef(initial) { const index = cursor++; return current.hooks[index] ??= { current: initial }; },
    useMemo(factory, deps) {
      const index = cursor++, previous = current.hooks[index];
      if (!previous || !same(previous.deps, deps)) current.hooks[index] = { deps, value: factory() };
      return current.hooks[index].value;
    },
    useCallback(callback, deps) { return react.useMemo(() => callback, deps); },
    useEffect(effect, deps) {
      const fiber = current, index = cursor++, previous = fiber.hooks[index];
      if (previous && same(previous.deps, deps)) return;
      fiber.effects.push(() => {
        previous?.cleanup?.();
        fiber.hooks[index] = { deps, cleanup: effect() };
      });
    },
  };
  const ui = Object.fromEntries(["Badge", "Button", "Checkbox", "Dialog", "DialogContent", "DialogDescription", "DialogFooter", "DialogHeader", "DialogTitle", "Input", "Switch", "Tooltip"].map(name => [name, name]));
  const modules = {
    react,
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "@tabler/icons-react": new Proxy({}, { get: (_, name) => String(name) }),
    "@heroui/react": { cn: (...values) => values.filter(Boolean).join(" "), toast: { success: message => notices.push(message), warning: message => notices.push(message) } },
    "./api": { invoke(command, args = {}) {
      if (args.discardUpload) { calls.push({ command, args }); return Promise.resolve({ status: "ok" }); }
      return new Promise((resolve, reject) => calls.push({ command, args, resolve, reject }));
    } },
    "./appUtils": { errorText: cause => cause.message },
    "./components/ui": ui,
    "./codeyPlugins": pluginHelpers,
    "./pluginPackageImport": importHelpers,
    "./PluginConfigDialog": { PluginConfigDialog: "PluginConfigDialog" },
    "./SettingsPageHeader": { SettingsPageHeader: props => jsx("header", { children: props.actions }) },
    "./formatters": { formatBytes: value => String(value) },
  };
  for (const [name, source] of [...sources].reverse()) {
    const exports = {};
    new Function("require", "exports", source)(specifier => {
      assert.ok(specifier in modules, `unexpected import: ${specifier}`);
      return modules[specifier];
    }, exports);
    modules[`./${name}`] = exports;
  }
  const cleanup = fiber => { fiber.hooks.forEach(hook => hook?.cleanup?.()); fiber.mounted = false; };
  function render() {
    const visited = new Set();
    function visit(node, address) {
      if (Array.isArray(node)) return node.map((child, index) => visit(child, `${address}.${index}`));
      if (!node || typeof node !== "object") return node;
      if (typeof node.type === "function") {
        let fiber = fibers.get(address);
        if (fiber && fiber.type !== node.type) { cleanup(fiber); fiber = null; }
        if (!fiber) { fiber = { type: node.type, hooks: [], effects: [], mounted: true }; fibers.set(address, fiber); }
        visited.add(address);
        current = fiber; cursor = 0;
        return visit(node.type(node.props), `${address}.child`);
      }
      return { ...node, props: { ...node.props, children: visit(node.props?.children, `${address}.children`) } };
    }
    tree = visit(jsx(modules["./CodeyPluginsSection"].CodeyPluginsSection, {}), "root");
    for (const [address, fiber] of fibers) if (!visited.has(address)) { cleanup(fiber); fibers.delete(address); }
    for (const fiber of fibers.values()) for (const effect of fiber.effects.splice(0)) effect();
    return tree;
  }
  function find(type) {
    const walk = node => !node || typeof node !== "object" ? [] : Array.isArray(node) ? node.flatMap(walk) : [...(node.type === type ? [node] : []), ...walk(node.props?.children)];
    return walk(tree);
  }
  function send(type, files = [], values = {}) {
    const event = new Event(type, { cancelable: true });
    Object.assign(event, { dataTransfer: { types: ["Files"], files, dropEffect: "none" }, clientX: 100, clientY: 100, relatedTarget: null, ...values });
    event.composedPath = () => [{}, { hasAttribute: name => name === "data-codey-plugin-drop-zone" }, {}];
    target.dispatchEvent(event);
    render();
    return event;
  }
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    for (const fiber of fibers.values()) cleanup(fiber);
    fibers.clear();
    globalThis.window = previousWindow;
  };
  context.after(dispose);
  render();
  find("section")[0].props.ref({ closest: () => null, getClientRects: () => visible ? [{}] : [] });
  render();
  return { render, find, calls, notices, send, dispose, setVisible: value => { visible = value; }, get text() { return text(tree); }, get staleWrites() { return staleWrites; } };
}

const button = (harness, label) => harness.find("Button").find(node => text(node) === label);
const click = (harness, label) => { const node = button(harness, label); assert.ok(node, `missing button: ${label}`); (node.props.onClick ?? node.props.onPress)(); harness.render(); };
const latest = (harness, command) => harness.calls.filter(call => call.command === command && !call.args.discardUpload).at(-1);
async function ready(harness, plugins = [], platform = "windows") {
  harness.calls[0].resolve(result(plugins, platform)); await flush(); harness.render();
}
async function dropped(harness, version = "1.0.0") {
  harness.send("drop", [new File(["package bytes"], "demo.codey-plugin")]);
  await flush(); harness.render();
  latest(harness, "inspect_codey_plugin").resolve(preview(version)); await flush(); harness.render();
}

test("normal drop inspects first, shows loading, confirms once and refreshes the visible list", async context => {
  const harness = flowHarness(context); await ready(harness);
  const event = harness.send("drop", [new File(["package bytes"], "demo.codey-plugin")]);
  assert.equal(event.defaultPrevented, true);
  assert.match(harness.text, /正在读取并检查/);
  assert.equal(harness.find("Dialog").length, 0, "导入在页面内完成，不再弹窗");
  assert.equal(button(harness, "确认导入").props.disabled, true);
  await flush();
  assert.equal(harness.calls.some(call => call.command === "install_codey_plugin"), false);
  latest(harness, "inspect_codey_plugin").resolve(preview()); await flush(); harness.render();
  assert.match(harness.text, /demo.codey-plugin/);
  const search = harness.find("Input").find(node => node.props["aria-label"] === "搜索插件");
  search.props.onChange({ target: { value: "does-not-match" } }); harness.render();
  const confirm = button(harness, "确认导入").props.onClick;
  confirm(); confirm(); harness.render();
  assert.equal(harness.calls.filter(call => call.command === "install_codey_plugin").length, 1);
  assert.match(harness.text, /正在导入插件/);
  assert.deepEqual(latest(harness, "install_codey_plugin").args, { path: preview().path, sha256: hash });
  latest(harness, "install_codey_plugin").resolve(result([plugin()])); await flush(); harness.render();
  assert.equal(button(harness, "确认导入"), undefined, "安装后导入面板回到初始状态");
  assert.equal(harness.find("article").length, 1);
  assert.match(harness.text, /安装成功，默认处于停用状态/);
  assert.equal(harness.notices.length, 1);
  assert.ok(harness.calls.some(call => call.args.discardUpload === preview().uploadId));
});

test("unsupported and empty files show actionable feedback without inspection or installation", async context => {
  const harness = flowHarness(context); await ready(harness);
  for (const file of [new File(["x"], "demo.zip"), new File([], "demo.codey-plugin")]) {
    harness.send("drop", [file]);
    assert.match(harness.text, /仅支持|为空或已损坏/);
    assert.ok(button(harness, "重新选择"));
    assert.ok(button(harness, "重试检查"));
    assert.equal(button(harness, "确认导入").props.disabled, true);
    click(harness, "取消");
  }
  assert.deepEqual(harness.calls.map(call => call.command), ["list_codey_plugins"]);
});

test("corrupt files retain their source and can retry inspection successfully", async context => {
  const harness = flowHarness(context); await ready(harness);
  harness.send("drop", [new File(["broken"], "demo.codey-plugin")]); await flush();
  latest(harness, "inspect_codey_plugin").reject(new Error("ZIP 格式无效")); await flush(); harness.render();
  assert.match(harness.text, /检查插件包失败.*ZIP 格式无效/);
  click(harness, "重试检查"); await flush();
  latest(harness, "inspect_codey_plugin").resolve(preview()); await flush(); harness.render();
  assert.equal(button(harness, "确认导入").props.disabled, false);
  assert.doesNotMatch(harness.text, /检查插件包失败/);
});

test("duplicate versions are explained and never overwrite installed plugins", async context => {
  const harness = flowHarness(context); await ready(harness, [plugin()]); await dropped(harness);
  assert.match(harness.text, /已安装，无需重复导入/);
  assert.equal(button(harness, "确认升级").props.disabled, true);
  button(harness, "确认升级").props.onClick();
  assert.equal(harness.calls.some(call => call.command === "install_codey_plugin"), false);
  assert.ok(button(harness, "重新选择"));
});

test("higher versions retain the existing explicit upgrade flow", async context => {
  const harness = flowHarness(context); await ready(harness, [plugin()]); await dropped(harness, "2.0.0");
  assert.equal(button(harness, "确认升级").props.disabled, false);
  click(harness, "确认升级");
  latest(harness, "install_codey_plugin").resolve(result([plugin("2.0.0")])); await flush(); harness.render();
  assert.match(harness.text, /升级成功/);
  assert.match(harness.text, /v2.0.0/);
});

test("installation errors refresh state, preserve the preview and offer a working retry", async context => {
  const harness = flowHarness(context); await ready(harness); await dropped(harness);
  click(harness, "确认导入");
  latest(harness, "install_codey_plugin").reject(new Error("磁盘空间不足")); await flush();
  latest(harness, "list_codey_plugins").resolve(result()); await flush(); harness.render();
  assert.match(harness.text, /导入插件失败.*磁盘空间不足/);
  assert.equal(button(harness, "重试导入").props.disabled, false);
  click(harness, "重试导入");
  latest(harness, "install_codey_plugin").resolve(result([plugin()])); await flush(); harness.render();
  assert.match(harness.text, /安装成功/);
  assert.equal(harness.calls.filter(call => call.command === "install_codey_plugin").length, 2);
});

test("drag cancellation resets page highlighting without starting an import", async context => {
  const harness = flowHarness(context); await ready(harness);
  harness.send("dragenter");
  const region = () => harness.find("div").find(node => node.props["aria-label"] === "插件文件拖放区域");
  assert.match(region().props.className, /ring-2/);
  harness.send("keydown", [], { key: "Escape" });
  assert.doesNotMatch(region().props.className, /ring-2/);
  harness.send("dragenter"); harness.send("dragleave", [], { clientX: 0 });
  assert.doesNotMatch(region().props.className, /ring-2/);
  assert.equal(button(harness, "确认导入"), undefined);
  assert.equal(harness.calls.length, 1);
});

test("Linux path inspection is available inline and uses the original command", async context => {
  const harness = flowHarness(context); await ready(harness, [], "linux");
  const input = harness.find("Input").find(node => node.props["aria-label"] === "插件包路径");
  input.props.onChange({ target: { value: "  demo.codey-plugin  " } }); harness.render();
  click(harness, "检查安装包");
  assert.deepEqual(latest(harness, "inspect_codey_plugin").args, { path: "demo.codey-plugin" });
  latest(harness, "inspect_codey_plugin").resolve(preview()); await flush(); harness.render();
  assert.equal(button(harness, "确认导入").props.disabled, false);
});

test("closing a checked import releases staging and busy drops never start another import", async context => {
  const harness = flowHarness(context); await ready(harness);
  harness.send("drop", [new File(["bytes"], "demo.codey-plugin")]); await flush();
  harness.send("drop", [new File(["other"], "other.codey-plugin")]);
  assert.equal(harness.calls.filter(call => call.args.upload).length, 1);
  latest(harness, "inspect_codey_plugin").resolve(preview()); await flush(); harness.render();
  click(harness, "取消");
  assert.equal(button(harness, "确认导入"), undefined);
  assert.ok(harness.calls.some(call => call.args.discardUpload === preview().uploadId));
  assert.equal(harness.calls.some(call => call.command === "install_codey_plugin"), false);
});

test("unmount during upload aborts and discards staging without stale component writes", async context => {
  const harness = flowHarness(context); await ready(harness);
  harness.send("drop", [new File(["bytes"], "demo.codey-plugin")]); await flush();
  const request = latest(harness, "inspect_codey_plugin");
  harness.dispose(); request.resolve(preview()); await flush();
  assert.ok(harness.calls.some(call => call.args.discardUpload === preview().uploadId));
  assert.equal(harness.staleWrites, 0);
});

test("retained hidden plugin pages do not intercept file drags on other screens", async context => {
  const harness = flowHarness(context); await ready(harness);
  harness.setVisible(false);
  assert.equal(harness.send("dragover").defaultPrevented, false);
  assert.equal(harness.send("drop", [new File(["data"], "demo.codey-plugin")]).defaultPrevented, false);
  assert.equal(harness.calls.length, 1);
  assert.equal(harness.find("Dialog").length, 0);
  harness.setVisible(true);
  assert.equal(harness.send("dragover").defaultPrevented, true);
});

test("the inline import panel marks a real DOM drop zone and preserves the native picker", async context => {
  const harness = flowHarness(context); await ready(harness);
  assert.ok(harness.find("div").some(node => Object.hasOwn(node.props, "data-codey-plugin-drop-zone")));
  click(harness, "选择文件");
  latest(harness, "select_codey_plugin_package").resolve(null); await flush(); harness.render();
  assert.equal(button(harness, "确认导入"), undefined, "取消选择后不进入确认步骤");
  click(harness, "选择文件");
  latest(harness, "select_codey_plugin_package").resolve({ ...preview(), path: "picked.codey-plugin" }); await flush(); harness.render();
  click(harness, "确认导入");
  assert.deepEqual(latest(harness, "install_codey_plugin").args, { path: "picked.codey-plugin", sha256: hash });
  latest(harness, "install_codey_plugin").resolve(result([plugin()])); await flush(); harness.render();
  assert.match(harness.text, /安装成功/);
  assert.equal(harness.calls.some(call => call.args.upload), false);
});
