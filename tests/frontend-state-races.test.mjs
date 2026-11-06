import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";
import { autoStubModule, collectElements, createModuleGraph, elementProps, elementType, textContent } from "./helpers/jsx-tree.mjs";

const flush = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const accountResult = (...ids) => ({ accounts: ids.map(id => ({ id, email: `${id}@example.test`, isDefault: false })) });

function accountRequests() {
  const calls = [];
  const module = createModuleGraph(new URL("../src/officialAccountsRequests.ts", import.meta.url), {
    stubs: { "./api": { invoke: () => { const call = deferred(); calls.push(call); return call.promise; } } },
  });
  return { ...module.exports, calls };
}

for (const completion of ["resolve", "reject"]) {
  test(`account mutation supersedes a pending list ${completion} for consumers and cache`, async () => {
    const requests = accountRequests();
    const first = requests.listOfficialAccounts();
    assert.equal(requests.listOfficialAccounts(), first, "in-flight reads are deduplicated");
    const latest = accountResult("new");
    requests.rememberOfficialAccounts(latest);
    requests.calls[0][completion](completion === "resolve" ? accountResult("old") : new Error("old failure"));
    assert.equal(await first, latest);
    assert.equal(await requests.listOfficialAccounts(), latest);
    assert.equal(requests.calls.length, 1);
  });
}

for (const olderFirst of [false, true]) {
  test(`forced account refresh wins with older response ${olderFirst ? "first" : "last"}`, async () => {
    const requests = accountRequests();
    const first = requests.listOfficialAccounts();
    const second = requests.listOfficialAccounts({ force: true });
    assert.equal(requests.listOfficialAccounts(), second);
    const latest = accountResult("latest");
    if (olderFirst) requests.calls[0].resolve(accountResult("old"));
    requests.calls[1].resolve(latest);
    assert.equal(await second, latest);
    if (!olderFirst) requests.calls[0].resolve(accountResult("old"));
    assert.equal(await first, latest);
    assert.equal(await requests.listOfficialAccounts(), latest);
  });
}

test("a failed list can be retried instead of leaving a rejected deduplication promise", async () => {
  const requests = accountRequests();
  const first = requests.listOfficialAccounts();
  requests.calls[0].reject(new Error("offline"));
  await assert.rejects(first, /offline/);
  const retry = requests.listOfficialAccounts();
  requests.calls[1].resolve(accountResult("ok"));
  assert.deepEqual((await retry).accounts.map(account => account.id), ["ok"]);
});

test("a forced account refresh also supersedes a still-fresh cached result for subsequent readers", async () => {
  const requests = accountRequests();
  requests.rememberOfficialAccounts(accountResult("cached"));
  const refresh = requests.listOfficialAccounts({ force: true });
  assert.equal(requests.listOfficialAccounts(), refresh);
  requests.calls[0].resolve(accountResult("refreshed"));
  assert.deepEqual((await refresh).accounts.map(account => account.id), ["refreshed"]);
  assert.equal(requests.calls.length, 1);
});

// 保留 hook 状态和 effect 清理，直接调用真实组件的事件处理器。
function componentHarness(name, props, stubs = {}) {
  let cursor = 0, tree, mounted = true;
  const hooks = [], effects = [];
  const same = (a, b) => a && b && a.length === b.length && a.every((value, index) => Object.is(value, b[index]));
  const react = {
    memo: value => value,
    useState(initial) {
      const index = cursor++;
      const cell = hooks[index] ??= { value: typeof initial === "function" ? initial() : initial };
      cell.set ??= next => { if (mounted) cell.value = typeof next === "function" ? next(cell.value) : next; };
      return [cell.value, cell.set];
    },
    useRef(initial) { return hooks[cursor++] ??= { current: initial }; },
    useMemo(factory, deps) {
      const index = cursor++, previous = hooks[index];
      if (!previous || !same(previous.deps, deps)) hooks[index] = { deps, value: factory() };
      return hooks[index].value;
    },
    useCallback(callback, deps) { return react.useMemo(() => callback, deps); },
    useEffect(effect, deps) {
      const index = cursor++, previous = hooks[index];
      if (previous && same(previous.deps, deps)) return;
      effects.push(() => { previous?.cleanup?.(); hooks[index] = { deps, cleanup: effect() }; });
    },
  };
  const ui = autoStubModule("ui");
  const module = createModuleGraph(new URL(`../src/${name}.tsx`, import.meta.url), { stubs: {
    react,
    "@tabler/icons-react": autoStubModule("icons"),
    "./components/ui": ui,
    "./components/ModelCombobox": autoStubModule(),
    "./components/ModelSettingsFields": autoStubModule(),
    "./SettingsPageHeader": autoStubModule(),
    "./OfficialAccountsPanel": autoStubModule(),
    "./overlayTheme": { readHostTheme: () => "light" },
    "./appUtils": { errorText: error => String(error.message ?? error) },
    ...stubs,
  } });
  const render = () => {
    cursor = 0;
    tree = module.exports[name](props);
    effects.splice(0).forEach(effect => effect());
    return tree;
  };
  const find = predicate => collectElements(tree, node => predicate(elementProps(node), node));
  const button = label => find((_, node) => elementType(node) === ui.Button && textContent(node).trim() === label)[0];
  const click = label => {
    const node = button(label);
    assert.ok(node, `missing button: ${label}`);
    elementProps(node).onClick(); render();
  };
  render();
  return { render, find, click, ui, props, get tree() { return tree; }, get text() { return textContent(tree); },
    dispose() { hooks.forEach(hook => hook.cleanup?.()); mounted = false; },
  };
}

function modelProps() {
  return {
    config: {
      localRouterEnabled: true, miscModel: "", profiles: [{ id: "existing", name: "Existing", shortName: "ex", authMode: "apiKey", baseUrl: "https://existing.test", apiKey: "", upstreamProtocol: "openaiResponses" }],
      selectedModelsByProvider: {}, declaredOfficialModelsByProvider: {}, modelContextByProvider: {}, routeRequestLog: { enabled: false },
    },
    modelState: { officialModels: [], officialModelIds: [], thirdPartyModels: [] },
    currentProvider: null, officialAccountAvailable: true, isBusy: false, active: false,
  };
}

function routeHarness(context) {
  const reads = [];
  const previous = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { clipboard: { readText() { const read = deferred(); reads.push(read); return read.promise; } } } });
  const harness = componentHarness("ModelSection", modelProps(), { "./api": { invoke: async () => ({}) } });
  context.after(() => {
    harness.dispose();
    if (previous) Object.defineProperty(globalThis, "navigator", previous);
    else delete globalThis.navigator;
  });
  return { ...harness, reads, get tree() { return harness.tree; }, get text() { return harness.text; } };
}
const payload = (key = "new-key") => JSON.stringify({ _type: "newapi_channel_conn", key, url: "https://new.test" });
const value = (harness, id) => elementProps(harness.find(props => props.id === id)[0])?.value;

for (const change of ["close", "new-draft", "existing-route"]) {
  test(`delayed clipboard results are ignored after ${change}`, async context => {
    const h = routeHarness(context);
    h.click("新增线路");
    h.click("快速导入");
    if (change === "existing-route") {
      const edit = h.find(props => props["aria-label"] === "编辑线路 Existing")[0];
      assert.ok(edit);
      elementProps(edit).onClick(); h.render();
    } else {
      const dialog = h.find((_, node) => elementType(node) === h.ui.Dialog && elementProps(node).open)[0];
      elementProps(dialog).onOpenChange(false); h.render();
      if (change === "new-draft") h.click("新增线路");
    }
    h.reads[0].resolve(payload()); await flush(); h.render();
    assert.equal(value(h, "route-key-input"), change === "close" ? undefined : "");
    assert.doesNotMatch(h.text, /已从连接信息填入/);
  });
}

test("the latest clipboard request wins when reads complete in reverse order", async context => {
  const h = routeHarness(context);
  h.click("新增线路"); h.click("快速导入"); h.click("快速导入");
  h.reads[1].resolve(payload("latest-key")); await flush(); h.render();
  assert.equal(value(h, "route-key-input"), "latest-key");
  h.reads[0].resolve(payload("stale-key")); await flush(); h.render();
  assert.equal(value(h, "route-key-input"), "latest-key");
});

test("an old clipboard failure does not open the manual import panel in a new draft", async context => {
  const h = routeHarness(context);
  h.click("新增线路"); h.click("快速导入"); h.click("取消"); h.click("新增线路");
  h.reads[0].reject(new Error("denied")); await flush(); h.render();
  assert.equal(h.find(props => props.className === "route-import-panel").length, 0);
});

test("account panel keeps mutation results when its earlier refresh resolves or fails", async () => {
  for (const completion of ["resolve", "reject"]) {
    const list = deferred(), mutation = deferred(), loaded = [], changed = [];
    const h = componentHarness("OfficialAccountsPanel", {
      officialAccountAvailable: true, isBusy: false, active: false, popupContainer: null,
      onAccountsLoaded: accounts => loaded.push(accounts), onAccountsChanged: result => changed.push(result), onNotice() {},
    }, {
      "./officialAccountsRequests": { listOfficialAccounts: () => list.promise, rememberOfficialAccounts() {} },
      "./api": { invoke: command => { assert.equal(command, "import_current_codex_login"); return mutation.promise; } },
    });
    h.click("导入当前登录"); mutation.resolve(accountResult("added")); await flush(); h.render();
    assert.match(h.text, /added@example.test/);
    list[completion](completion === "resolve" ? accountResult("old") : new Error("stale error"));
    await flush(); h.render();
    assert.match(h.text, /added@example.test/);
    assert.doesNotMatch(h.text, /old@example.test|stale error/);
    assert.equal(loaded.length, 0);
    assert.equal(changed.length, 1);
    h.dispose();
  }
});

async function accountPanelHarness(context, ids = ["old"]) {
  const calls = [], loaded = [], changed = [], remembered = [], notices = [], downloads = [];
  const h = componentHarness("OfficialAccountsPanel", {
    officialAccountAvailable: true, isBusy: false, active: false, popupContainer: null,
    onAccountsLoaded: accounts => loaded.push(accounts), onAccountsChanged: result => changed.push(result),
    onNotice: notice => notices.push(notice),
  }, {
    "./officialAccountsRequests": {
      listOfficialAccounts: async () => accountResult(...ids),
      rememberOfficialAccounts: result => remembered.push(result),
    },
    "./api": { invoke: (command, args) => { const call = { command, args, ...deferred() }; calls.push(call); return call.promise; } },
    "./features/codex-extensions/download": {
      downloadResource: (...args) => downloads.push(args),
    },
  });
  context.after(() => h.dispose());
  await flush(); h.render();
  const invalidateUsage = async (index = 0) => {
    const button = h.find(props => props["aria-label"] === "刷新额度")[index];
    assert.ok(button);
    elementProps(button).onClick();
    calls.at(-1).resolve({ status: "error", reason: "official_account_invalid" });
    await flush(); h.render();
    assert.equal(calls.at(-1).command, "refresh_official_account_routes");
    return calls.at(-1);
  };
  return { h, calls, loaded, changed, remembered, notices, downloads, invalidateUsage };
}

test("deactivating a retained account page stops queued usage requests and reopening refreshes", async context => {
  const { h, calls } = await accountPanelHarness(context, ["one", "two", "three"]);
  h.props.active = true;
  h.render();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "query_official_account_usage");
  h.props.active = false;
  h.render();
  calls[0].resolve({ status: "ok" });
  await flush();
  h.render();
  assert.equal(calls.length, 1, "hidden pages must not query the remaining accounts");
  h.props.active = true;
  h.render();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].args.accountId, "one");
  h.props.active = false;
  h.render();
  calls[1].resolve({ status: "ok" });
  await flush();
});

for (const completion of ["resolve", "reject"]) {
  test(`account mutation supersedes an earlier route refresh ${completion}`, async context => {
    const { h, calls, changed, remembered, invalidateUsage } = await accountPanelHarness(context);
    const refresh = await invalidateUsage();
    h.click("导入当前登录");
    const latest = accountResult("added");
    calls.at(-1).resolve(latest); await flush(); h.render();
    refresh[completion](completion === "resolve" ? accountResult("old") : new Error("stale route failure"));
    await flush(); h.render();
    assert.match(h.text, /added@example.test/);
    assert.doesNotMatch(h.text, /old@example.test|stale route failure/);
    assert.deepEqual(changed, [latest]);
    assert.deepEqual(remembered, [latest]);
  });

  test(`the newest route refresh wins over an older ${completion}`, async context => {
    const { h, changed, remembered, invalidateUsage } = await accountPanelHarness(context, ["first", "second"]);
    const first = await invalidateUsage();
    const second = await invalidateUsage();
    const latest = accountResult("latest");
    second.resolve(latest); await flush(); h.render();
    first[completion](completion === "resolve" ? accountResult("stale") : new Error("stale failure"));
    await flush(); h.render();
    assert.match(h.text, /latest@example.test/);
    assert.doesNotMatch(h.text, /stale/);
    assert.deepEqual(changed, [latest]);
    assert.deepEqual(remembered, [latest]);
  });

  test(`unmounted account panel ignores a route refresh ${completion}`, async context => {
    const { h, changed, remembered, invalidateUsage } = await accountPanelHarness(context);
    const refresh = await invalidateUsage();
    h.dispose();
    refresh[completion](completion === "resolve" ? accountResult("stale") : new Error("stale failure"));
    await flush();
    assert.deepEqual(changed, []);
    assert.deepEqual(remembered, []);
  });
}

for (const kind of ["login", "siwc"]) {
  test(`${kind} polling waits between requests and clears the retry timer on close`, async context => {
    context.mock.timers.enable({ apis: ["setTimeout"] });
    const { h, calls } = await accountPanelHarness(context);
    if (kind === "login") h.click("添加账号");
    else elementProps(h.find(props => props["aria-label"] === "启用官方直连")[0]).onClick();
    calls.at(-1).resolve({ loginId: "session", authUrl: "https://auth.test" });
    await flush(); h.render();
    calls.at(-1).resolve({ status: "wait" }); await flush(); h.render();
    assert.equal(calls.filter(call => call.command === `poll_official_account_${kind}`).length, 1);
    context.mock.timers.tick(1499);
    assert.equal(calls.filter(call => call.command === `poll_official_account_${kind}`).length, 1);
    context.mock.timers.tick(1);
    assert.equal(calls.filter(call => call.command === `poll_official_account_${kind}`).length, 2);
    calls.at(-1).resolve({ status: "wait" }); await flush();
    const dialog = h.find((props, node) => elementType(node) === h.ui.Dialog && props.open)[0];
    elementProps(dialog).onOpenChange(false); h.render(); calls.at(-1).resolve({});
    context.mock.timers.tick(1500);
    assert.equal(calls.filter(call => call.command === `poll_official_account_${kind}`).length, 2);
  });

  test(`${kind} polling stops after cancellation or unmount`, async context => {
    for (const finish of ["cancel", "unmount"]) {
      const { h, calls, changed, notices } = await accountPanelHarness(context);
      if (kind === "login") h.click("添加账号");
      else elementProps(h.find(props => props["aria-label"] === "启用官方直连")[0]).onClick();
      calls.at(-1).resolve({ loginId: "session", authUrl: "https://auth.test" });
      await flush(); h.render();
      const poll = calls.at(-1);
      assert.equal(poll.command, `poll_official_account_${kind}`);
      if (finish === "cancel") {
        const dialog = h.find((props, node) => elementType(node) === h.ui.Dialog && props.open)[0];
        elementProps(dialog).onOpenChange(false); h.render();
        assert.equal(calls.at(-1).command, `cancel_official_account_${kind}`);
        calls.at(-1).resolve({});
      } else h.dispose();
      poll.resolve({ ...accountResult("added"), status: "ok" }); await flush();
      assert.deepEqual(changed, []);
      assert.deepEqual(notices, []);
    }
  });

  test(`${kind} polling commits a successful authorization once`, async context => {
    const { h, calls, changed, notices } = await accountPanelHarness(context);
    if (kind === "login") h.click("添加账号");
    else elementProps(h.find(props => props["aria-label"] === "启用官方直连")[0]).onClick();
    calls.at(-1).resolve({ loginId: "session", authUrl: "https://auth.test" });
    await flush(); h.render();
    calls.at(-1).resolve({ ...accountResult("added"), status: "ok" });
    await flush(); h.render();
    assert.match(h.text, /added@example.test/);
    assert.equal(changed.length, 1);
    assert.equal(notices.length, 1);
    assert.equal(h.find((props, node) => elementType(node) === h.ui.Dialog && props.open).length, 0);
  });
}

test("account export downloads the server-named file without account mutations", async context => {
  const { h, calls, downloads, notices } = await accountPanelHarness(context);
  const exportButton = h.find(props => props["aria-label"] === "导出官方账号 old@example.test")[0];
  assert.ok(exportButton);
  let stopped = 0;
  elementProps(exportButton).onClick({ stopPropagation: () => { stopped += 1; } });
  assert.equal(stopped, 1);
  assert.equal(calls.at(-1).command, "export_official_account_credential");
  assert.deepEqual(calls.at(-1).args, { accountId: "old" });
  calls.at(-1).resolve({
    status: "ok",
    fileName: "served-account.json",
    content: "{\"refresh_token\":\"secret\"}",
  });
  await flush(); h.render();
  assert.deepEqual(downloads, [[
    "served-account.json",
    "{\"refresh_token\":\"secret\"}",
    "application/json;charset=utf-8",
  ]]);
  assert.deepEqual(notices, [{ tone: "success", text: "账号文件已导出，请妥善保管，不要分享给他人" }]);
  assert.equal(calls.some(call => call.command === "set_default_official_account"
    || call.command === "remove_official_account"), false);
});

test("account export reports failures and leaves the account idle", async context => {
  const { h, calls, downloads, notices } = await accountPanelHarness(context);
  const exportButton = h.find(props => props["aria-label"] === "导出官方账号 old@example.test")[0];
  elementProps(exportButton).onClick({ stopPropagation() {} });
  calls.at(-1).reject(new Error("export denied"));
  await flush(); h.render();
  assert.equal(downloads.length, 0);
  assert.deepEqual(notices, [{ tone: "error", text: "export denied" }]);
  const rerendered = h.find(props => props["aria-label"] === "导出官方账号 old@example.test")[0];
  assert.equal(elementProps(rerendered).loading, false);
});

test("manual account file content uses the existing import path and stale reads are ignored", async context => {
  const { h, calls } = await accountPanelHarness(context);
  h.click("手动输入");
  const input = () => h.find(props => props["aria-label"] === "选择 Codey 导出的账号文件")[0];
  const firstText = deferred();
  elementProps(input()).onChange({
    target: { value: "chosen", files: [{ name: "first.json", size: 10, text: () => firstText.promise }] },
  });
  h.render();
  h.click("取消");
  h.click("手动输入");
  const secondText = deferred();
  elementProps(input()).onChange({
    target: { value: "chosen", files: [{ name: "second.json", size: 10, text: () => secondText.promise }] },
  });
  secondText.resolve("{\"refresh_token\":\"latest\"}"); await flush(); h.render();
  firstText.resolve("{\"refresh_token\":\"stale\"}"); await flush(); h.render();
  const editor = h.find(props => props["aria-label"] === "Refresh Token 或 OAuth JSON")[0];
  assert.equal(elementProps(editor).value, "{\"refresh_token\":\"latest\"}");
  assert.match(h.text, /已载入 second\.json/);
  assert.doesNotMatch(h.text, /已载入 first\.json/);
  h.click("添加");
  assert.deepEqual(calls.at(-1).args, { credential: "{\"refresh_token\":\"latest\"}" });
  calls.at(-1).resolve(accountResult("imported"));
  await flush(); h.render();
  assert.match(h.text, /imported@example.test/);
});

test("plugin route refresh reports failures without changing the completed plugin operation", async () => {
  // 执行 App 中实际注册的刷新回调，依赖边界使用内存替身。
  const source = ts.createSourceFile("App.tsx", readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let callback;
  const visit = node => {
    if (ts.isBinaryExpression(node) && node.left.getText(source) === "refreshPluginRoutesRef.current") callback = node.right;
    ts.forEachChild(node, visit);
  };
  visit(source);
  assert.ok(callback);
  const compiled = ts.transpileModule(`const callback = ${callback.getText(source)};`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const notices = [], applied = [];
  let result = deferred();
  const refresh = new Function("invoke", "adoptRouteConfig", "setDirty", "setProviderStatus", "setModelState", "setNotice", "errorText", `${compiled}; return callback;`)(
    () => result.promise, config => { applied.push(config); return { dirty: false }; }, () => {}, () => {}, () => {}, notice => notices.push(notice), error => error.message,
  );
  refresh(); result.reject(new Error("offline")); await flush();
  assert.equal(applied.length, 0);
  assert.equal(notices[0].tone, "error");
  assert.match(notices[0].text, /offline/);
  assert.match(notices[0].text, /已完成的插件操作无需重复/);
  result = deferred(); refresh(); result.resolve({ config: { settingsRevision: 2 } }); await flush();
  assert.deepEqual(applied, [{ settingsRevision: 2 }]);
});
