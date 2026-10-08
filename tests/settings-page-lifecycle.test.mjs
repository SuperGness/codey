import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("../src/SettingsLayout.tsx", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const pages = ["overview", "models", "prompt", "subagents", "remote", "plugins", "mcp", "skills"];

// 与现有组件测试一样，用轻量 hook/组件身份模型验证生命周期，无需 DOM 或新增依赖。
function layoutHarness() {
  const fibers = new Map(), mounts = [], unmounts = [], callbacks = [];
  let current, cursor, tree;
  const react = {
    useState(initial) {
      const fiber = current, index = cursor++;
      if (!(index in fiber.hooks)) fiber.hooks[index] = typeof initial === "function" ? initial() : initial;
      return [fiber.hooks[index], value => {
        assert.equal(fiber.mounted, true, "state must belong to a mounted page");
        fiber.hooks[index] = typeof value === "function" ? value(fiber.hooks[index]) : value;
      }];
    },
    useId() { return react.useState("settings")[0]; },
    useEffect(effect, deps) {
      const fiber = current, index = cursor++, previous = fiber.hooks[index];
      if (previous && deps.length === previous.deps.length && deps.every((value, i) => Object.is(value, previous.deps[i]))) return;
      fiber.effects.push(() => {
        previous?.cleanup?.();
        fiber.hooks[index] = { deps, cleanup: effect() };
      });
    },
  };
  const jsx = (type, props, key) => ({ type, props, key });
  const exports = {};
  new Function("require", "exports", compiled)(name => {
    if (name === "react") return react;
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    if (name === "@tabler/icons-react") return new Proxy({}, { get: () => "icon" });
    throw new Error(`unexpected import: ${name}`);
  }, exports);

  function Page({ id, active }) {
    const [instance] = react.useState(() => ({}));
    const [draft, edit] = react.useState("");
    const [expanded, expand] = react.useState(false);
    const [result, complete] = react.useState(null);
    react.useEffect(() => {
      mounts.push(id);
      return () => unmounts.push(id);
    }, []);
    return jsx("page-probe", { id, active, instance, draft, edit, expanded, expand, result, complete });
  }
  function render() {
    const seen = new Set();
    const cleanup = fiber => { fiber.mounted = false; fiber.hooks.forEach(hook => hook?.cleanup?.()); };
    function visit(node, path) {
      if (Array.isArray(node)) return node.map((child, index) => visit(child, `${path}.${child?.key ?? index}`));
      if (!node || typeof node !== "object") return node;
      const address = `${path}:${node.key ?? ""}`;
      if (typeof node.type === "function") {
        let fiber = fibers.get(address);
        if (fiber && fiber.type !== node.type) { cleanup(fiber); fiber = null; }
        if (!fiber) { fiber = { type: node.type, hooks: [], effects: [], mounted: true }; fibers.set(address, fiber); }
        seen.add(address);
        current = fiber; cursor = 0;
        return visit(node.type(node.props), `${address}.child`);
      }
      return { ...node, props: { ...node.props, children: visit(node.props.children, `${address}.children`) } };
    }
    // 每次父级更新都生成新的内容元素和 render callback，模拟 App 的实际调用。
    const sections = Object.fromEntries(pages.map(id => [id, ["remote", "mcp", "skills"].includes(id)
      ? active => { callbacks.push({ id, active }); return jsx(Page, { id, active }); }
      : jsx(Page, { id })]));
    tree = visit(jsx(exports.SettingsLayout, { sections }), "root");
    for (const [address, fiber] of fibers) if (!seen.has(address)) { cleanup(fiber); fibers.delete(address); }
    for (const fiber of fibers.values()) for (const effect of fiber.effects.splice(0)) effect();
  }
  function find(type) {
    const walk = node => Array.isArray(node) ? node.flatMap(walk) : !node || typeof node !== "object" ? []
      : [...(node.type === type ? [node] : []), ...walk(node.props.children)];
    return walk(tree);
  }
  function select(id) {
    find("button").find(node => node.props.id.endsWith(`-menu-${id}`)).props.onClick();
    render();
  }
  render();
  return { render, select, find, mounts, unmounts, callbacks, page: id => find("page-probe").find(node => node.props.id === id)?.props };
}

test("initial overview mounts alone while every menu retains its section association", () => {
  const harness = layoutHarness();
  assert.deepEqual(harness.mounts, ["overview"]);
  assert.deepEqual(harness.callbacks, []);
  assert.equal(harness.find("section").length, pages.length);
  for (const button of harness.find("button")) {
    const section = harness.find("section").find(node => node.props.id === button.props["aria-controls"]);
    assert.ok(section);
    assert.equal(section.props["aria-labelledby"], button.props.id);
    assert.equal(section.props.hidden, button.props["aria-current"] !== "page");
  }
});

test("first visits mount once and preserve drafts, expansion and late results across switches", async () => {
  const harness = layoutHarness();
  for (const id of pages.slice(1)) {
    harness.select(id);
    const first = harness.page(id);
    first.edit(`draft-${id}`);
    first.expand(true);
    harness.select("overview");
    await Promise.resolve().then(() => first.complete(`result-${id}`));
    harness.render();
    harness.select(id);
    harness.select(id);
    const revisited = harness.page(id);
    assert.equal(revisited.instance, first.instance);
    assert.equal(revisited.draft, `draft-${id}`);
    assert.equal(revisited.expanded, true);
    assert.equal(revisited.result, `result-${id}`);
  }
  assert.deepEqual(harness.mounts, pages);
  assert.deepEqual(harness.unmounts, []);
});

test("visited render callbacks receive active=false while hidden and latest active=true on return", () => {
  const harness = layoutHarness();
  harness.select("mcp");
  assert.equal(harness.page("mcp").active, true);
  assert.equal(harness.page("skills"), undefined);
  harness.select("skills");
  assert.equal(harness.page("mcp").active, false);
  assert.equal(harness.page("skills").active, true);
  harness.select("overview");
  assert.equal(harness.page("mcp").active, false);
  assert.equal(harness.page("skills").active, false);
  harness.select("mcp");
  assert.equal(harness.page("mcp").active, true);
  assert.equal(harness.page("skills").active, false);
});
