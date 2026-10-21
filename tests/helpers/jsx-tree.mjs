import { readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import ts from "typescript";

// 极简 React 桩：直接执行组件函数，拿到真实渲染出的元素树。
// 元素树里保留每个组件的 props，因此可以断言事件接线与属性，而不是匹配源码字符串。
// 子组件不会被递归执行——JSX 的 children 在构造元素时就已求值，遍历即足够。

const ELEMENT = Symbol("codey-test-element");

export const isElement = (value) => Boolean(value && value[ELEMENT]);
export const elementType = (node) => (isElement(node) ? node.type : undefined);
export const elementProps = (node) => (isElement(node) ? node.props : undefined);

export function createStub(name) {
  const target = function StubComponent() {
    return null;
  };
  Object.defineProperty(target, "name", { value: name });
  const members = new Map();
  // 支持 Tabs.Panel 这类复合组件：属性访问返回同一份缓存桩，便于用引用比较定位。
  return new Proxy(target, {
    get(object, key) {
      if (key in object) return object[key];
      const member = `${name}.${String(key)}`;
      if (!members.has(member)) members.set(member, createStub(member));
      return members.get(member);
    },
  });
}

/** 未显式声明的依赖用它兜底，避免为一个测试补齐整棵依赖树。 */
export function autoStubModule(prefix = "stub") {
  const stubs = new Map();
  return new Proxy(
    {},
    {
      get(_target, key) {
        const name = `${prefix}:${String(key)}`;
        if (!stubs.has(name)) stubs.set(name, createStub(name));
        return stubs.get(name);
      },
    },
  );
}

function makeElement(type, props, key) {
  const next = { ...props };
  if (key !== undefined) next.key = key;
  return { [ELEMENT]: true, type, props: next };
}

function createReactStub() {
  let cursor = 0;
  let states = [];
  const contextDefaults = new WeakMap();
  const react = {
    createElement: (type, props, ...children) =>
      makeElement(type, {
        ...props,
        ...(children.length
          ? { children: children.length === 1 ? children[0] : children }
          : {}),
      }),
    Fragment: Symbol("Fragment"),
    memo: (component) => component,
    forwardRef: (component) => component,
    useState(initial) {
      const index = cursor++;
      if (!(index in states))
        states[index] = typeof initial === "function" ? initial() : initial;
      return [
        states[index],
        (next) => {
          states[index] =
            typeof next === "function" ? next(states[index]) : next;
        },
      ];
    },
    useRef: (initial) => ({ current: initial }),
    useMemo: (factory) => factory(),
    useCallback: (callback) => callback,
    useEffect: () => {},
    useLayoutEffect: () => {},
    useInsertionEffect: () => {},
    useId: () => ":r0:",
    useDeferredValue: (value) => value,
    // 元素树不执行 Provider；useContext 只读取各个 Context 的默认值。
    createContext(defaultValue) {
      const context = {};
      context.Provider = context;
      contextDefaults.set(context, defaultValue);
      return context;
    },
    useContext: (context) => contextDefaults.get(context),
    useReducer: (_reducer, initial) =>
      [typeof initial === "function" ? initial() : initial, () => {}],
    useTransition: () => [false, (callback) => callback()],
    useSyncExternalStore: (_subscribe, getSnapshot) => getSnapshot(),
    useEffectEvent: (callback) => callback,
    startTransition: (callback) => callback(),
  };
  return { react, reset: () => { cursor = 0; states = []; }, restart: () => { cursor = 0; } };
}

/**
 * 递归编译组件的相对依赖（TS/TSX → CommonJS）：纯函数模块用真实实现，
 * 只有 stubs 里显式声明的模块（UI 组件库、图标、IPC 等）才替换成桩。
 */
export function createModuleGraph(entryUrl, { stubs = {}, autoStub = false } = {}) {
  const { react, reset, restart } = createReactStub();
  const jsxRuntime = {
    Fragment: react.Fragment,
    jsx: (type, props, key) => makeElement(type, props ?? {}, key),
    jsxs: (type, props, key) => makeElement(type, props ?? {}, key),
  };
  const bare = new Map();
  const instances = new Map();

  const resolveBare = (name) => {
    if (bare.has(name)) return bare.get(name);
    let value;
    if (Object.prototype.hasOwnProperty.call(stubs, name)) value = stubs[name];
    else if (name === "react") value = react;
    else if (name === "react/jsx-runtime") value = jsxRuntime;
    else if (autoStub) value = autoStubModule(name);
    else throw new Error(`未处理的模块依赖：${name}`);
    bare.set(name, value);
    return value;
  };

  const resolveFile = (specifier, fromUrl) => {
    const base = new URL(specifier, fromUrl).href;
    const candidates = [
      base,
      `${base}.ts`,
      `${base}.tsx`,
      `${base}/index.ts`,
      `${base}/index.tsx`,
    ];
    for (const candidate of candidates) {
      try {
        if (statSync(fileURLToPath(candidate)).isFile()) return candidate;
      } catch {
        /* 继续尝试下一个候选路径 */
      }
    }
    throw new Error(`无法解析依赖：${specifier}（来自 ${fromUrl}）`);
  };

  const load = (url) => {
    const key = url.replace(/[?#].*$/, "");
    if (instances.has(key)) return instances.get(key);
    if (key.endsWith(".css")) return { default: "" };
    const exports = {};
    // 先登记实例，循环依赖拿到的是同一个 exports 对象。
    instances.set(key, exports);
    const compiled = ts.transpileModule(readFileSync(fileURLToPath(key), "utf8"), {
      fileName: key,
      compilerOptions: {
        esModuleInterop: true,
        // 只有 .tsx 走 JSX 解析：否则 .ts 里的泛型箭头函数（async <T>）会被当成 JSX。
        jsx: key.endsWith(".tsx") ? ts.JsxEmit.ReactJSX : ts.JsxEmit.Preserve,
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    const require = (specifier) =>
      Object.prototype.hasOwnProperty.call(stubs, specifier) ||
      !specifier.startsWith(".")
        ? resolveBare(specifier)
        : load(resolveFile(specifier, key));
    new Function("require", "exports", "module", compiled)(require, exports, {
      exports,
    });
    return exports;
  };

  return { exports: load(entryUrl.href), reset, restart, react };
}

/** 单文件版本的别名，保留给只依赖桩模块的组件测试。 */
export const createComponentModule = async (entryUrl, options) =>
  createModuleGraph(entryUrl, options);

/** 深度遍历元素树（含数组与 children），收集满足条件的元素。 */
export function collectElements(node, predicate = () => true) {
  const found = [];
  const visit = (current) => {
    if (Array.isArray(current)) {
      current.forEach(visit);
      return;
    }
    if (!isElement(current)) return;
    if (predicate(current)) found.push(current);
    visit(current.props.children);
  };
  visit(node);
  return found;
}

/** 拼接元素子树的可见文本，用于按文案定位按钮。 */
export function textContent(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textContent).join("");
  if (!isElement(node)) return "";
  return textContent(node.props.children);
}
