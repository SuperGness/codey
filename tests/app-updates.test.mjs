import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("../src/useAppUpdates.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;

function harness(enabled, installReport = null) {
  const hooks = [], effects = [], timers = new Map(), requests = [];
  let cursor = 0, timerId = 0, result;
  const options = {
    embedded: !enabled, configLoaded: true, codeyUpdatePolicy: "stable",
    isBusy: false, setBusy() {}, setNotice() {}, setConfirmation() {}, beforeInstall: async () => {},
  };
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = initial;
      return [hooks[index], next => { hooks[index] = typeof next === "function" ? next(hooks[index]) : next; }];
    },
    useRef(initial) { return react.useState({ current: initial })[0]; },
    useCallback(callback) { return react.useState(callback)[0]; },
    useEffect(effect, deps) {
      const index = cursor++, previous = hooks[index];
      if (previous && deps.every((value, i) => Object.is(value, previous.deps[i]))) return;
      effects.push(() => {
        previous?.cleanup?.();
        hooks[index] = { deps, cleanup: effect() };
      });
    },
  };
  const window = new EventTarget();
  window.setTimeout = callback => { timers.set(++timerId, callback); return timerId; };
  window.clearTimeout = id => timers.delete(id);
  const storage = new Map();
  const sessionStorage = {
    getItem: key => (storage.has(key) ? storage.get(key) : null),
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: key => storage.delete(key),
  };
  window.sessionStorage = sessionStorage;
  const exports = {};
  new Function("require", "exports", "window", compiled)(name => {
    if (name === "react") return react;
    if (name === "./api") {
      return {
        invoke: (command, args) => command === "update_install_report"
          // 安装结果查询不占用更新检查的请求队列，测试里直接返回预置结果。
          ? Promise.resolve(installReport)
          : new Promise((resolve, reject) => requests.push({ command, args, resolve, reject })),
      };
    }
    if (name === "./appUtils") return { withTimeout: promise => promise, errorText: String };
    if (name === "./formatters") return { formatBytes: String };
    throw new Error(`Unexpected import ${name}`);
  }, exports, window);
  const render = () => {
    cursor = 0;
    result = exports.useAppUpdates(options);
    effects.splice(0).forEach(effect => effect());
    return result;
  };
  render();
  return { options, render, requests, timers, window, sessionStorage, storage };
}

const available = { currentVersion: "1.1.1", latestVersion: "1.2.0", updateAvailable: true };
const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };

test("嵌入页面不重复定时检查，仍可手动检查", async () => {
  const h = harness(false);
  assert.equal(h.requests.length, 0);
  const checking = h.render().checkForUpdates();
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].command, "check_for_updates");
  assert.deepEqual(h.requests[0].args, { forceRefresh: true, manual: true });
  h.requests[0].resolve(available);
  await checking;
  assert.deepEqual(h.render().updateCheck, available);
});

for (const automaticFirst of [true, false]) {
  test(`手动检查发起新请求，${automaticFirst ? "先" : "后"}返回的自动检查不覆盖结果`, async () => {
    const h = harness(true);
    let confirmation = null;
    h.options.setConfirmation = value => { confirmation = value; };
    const manual = h.render().checkForUpdates();
    assert.equal(h.requests.length, 2);
    assert.deepEqual(h.requests[0].args, { forceRefresh: false });
    assert.deepEqual(h.requests[1].args, { forceRefresh: true, manual: true });
    const oldUpdate = {
      ...available,
      selectedAsset: { fileName: "Codey-1.2.0.dmg", size: 1048576, url: "https://example.com/old" },
    };
    const newUpdate = {
      ...oldUpdate,
      latestVersion: "1.3.0",
      selectedAsset: { ...oldUpdate.selectedAsset, fileName: "Codey-1.3.0.dmg", url: "https://example.com/new" },
    };
    if (automaticFirst) {
      h.requests[0].resolve(oldUpdate);
      await settle();
      assert.equal(h.render().updateCheck, null);
      assert.equal(confirmation, null);
    }
    h.requests[1].resolve(newUpdate);
    await manual;
    if (!automaticFirst) {
      h.requests[0].resolve(oldUpdate);
      await settle();
    }
    assert.deepEqual(h.render().updateCheck, newUpdate);
    assert.deepEqual(h.window.__codeyUpdateAvailability, newUpdate);
    assert.match(confirmation.title, /1\.3\.0/);
  });
}

test("手动重新检查可清除之前发现的更新", async () => {
  const h = harness(false);
  const first = h.render().checkForUpdates();
  h.requests[0].resolve(available);
  await first;
  const second = h.render().checkForUpdates();
  assert.equal(h.requests.length, 2);
  assert.deepEqual(h.requests[1].args, { forceRefresh: true, manual: true });
  const latest = { currentVersion: "1.1.1", latestVersion: "1.1.1", updateAvailable: false };
  h.requests[1].resolve(latest);
  await second;
  assert.deepEqual(h.render().updateCheck, latest);
  assert.equal(h.window.__codeyUpdateAvailability, null);
});

for (const scenario of [
  { name: "版本与安装包一致", change: {}, action: "install-update" },
  { name: "发布了更新版本", change: { latestVersion: "1.3.0" }, action: "download-update" },
  { name: "同版本安装包已变更", change: { sha256: "b".repeat(64) }, action: "download-update" },
  { name: "发布批次已变更", change: { publishId: "release-2" }, action: "download-update" },
  { name: "同版本授权已变更", change: { policyId: "policy-2" }, action: "download-update" },
]) {
  test(`安装包已下载时仍先手动查询：${scenario.name}`, async () => {
    const h = harness(false);
    let confirmation = null;
    h.options.setConfirmation = value => { confirmation = value; };
    const asset = {
      platform: "macos", arch: "aarch64", packageType: "dmg",
      fileName: "Codey-1.2.0.dmg", size: 1048576,
      url: "https://example.com/update.dmg", sha256: "a".repeat(64),
    };
    const update = { ...available, selectedAsset: asset, publishId: "release-1", policyId: "policy-1" };
    const first = h.render().checkForUpdates();
    h.requests[0].resolve(update);
    await first;
    const downloaded = {
      latestVersion: update.latestVersion, publishId: update.publishId, policyId: update.policyId,
      filePath: "/updates/Codey-1.2.0.dmg", fileName: asset.fileName,
      size: asset.size, sha256: asset.sha256, asset,
    };
    const download = h.render().downloadUpdate();
    assert.equal(h.requests[1].command, "download_update");
    assert.deepEqual(h.requests[1].args, {
      expectedVersion: update.latestVersion, expectedPolicyId: update.policyId,
    });
    h.requests[1].resolve(downloaded);
    await download;
    assert.deepEqual(h.render().downloadedUpdate, downloaded);
    confirmation = null;

    const checking = h.render().checkForUpdates();
    assert.equal(h.requests.length, 3);
    assert.deepEqual(h.requests[2].args, { forceRefresh: true, manual: true });
    assert.equal(confirmation, null);
    h.requests[2].resolve({
      ...update,
      latestVersion: scenario.change.latestVersion ?? update.latestVersion,
      publishId: scenario.change.publishId ?? update.publishId,
      policyId: scenario.change.policyId ?? update.policyId,
      selectedAsset: { ...asset, sha256: scenario.change.sha256 ?? asset.sha256 },
    });
    await checking;
    assert.equal(confirmation.action, scenario.action);
    assert.deepEqual(
      h.render().downloadedUpdate,
      scenario.action === "install-update" ? downloaded : null,
    );
  });
}

for (const policyId of [undefined, null]) {
  test(`下载更新时将 ${policyId} 授权标识转换为 null`, async () => {
    const h = harness(false);
    const asset = { fileName: "Codey-1.2.0.dmg", size: 1048576, url: "https://example.com/update.dmg" };
    const update = { ...available, selectedAsset: asset, policyId };
    const checking = h.render().checkForUpdates();
    h.requests[0].resolve(update);
    await checking;
    const download = h.render().downloadUpdate();
    assert.equal(h.requests[1].command, "download_update");
    assert.deepEqual(h.requests[1].args, {
      expectedVersion: update.latestVersion, expectedPolicyId: null,
    });
    const downloaded = { latestVersion: update.latestVersion, filePath: "/updates/Codey-1.2.0.dmg", ...asset };
    h.requests[1].resolve(downloaded);
    await download;
    assert.deepEqual(h.render().downloadedUpdate, downloaded);
  });
}

test("disabling an in-flight automatic check ignores its result and clears pending", async () => {
  const h = harness(true);
  assert.equal(h.render().automaticallyChecking, true);
  h.options.codeyUpdatePolicy = "off";
  h.render();
  assert.equal(h.render().automaticallyChecking, false);
  h.requests[0].resolve(available);
  await settle();
  assert.equal(h.render().updateCheck, null);
  assert.equal(h.window.__codeyUpdateAvailability, null);
  assert.equal(h.timers.size, 0);
});

test("disabling cancels the next automatic check and enabling starts again", async () => {
  const h = harness(true);
  h.requests[0].resolve({ ...available, updateAvailable: false });
  await settle();
  h.render();
  assert.equal(h.timers.size, 1);
  h.options.codeyUpdatePolicy = "off";
  h.render();
  assert.equal(h.timers.size, 0);
  h.options.codeyUpdatePolicy = "stable";
  h.render();
  assert.equal(h.requests.length, 2);
});

test("detecting an update with an asset prompts the confirmation dialog", async () => {
  const h = harness(false);
  let confirmation = null;
  h.options.setConfirmation = (c) => {
    confirmation = c;
  };
  const updateWithAsset = {
    ...available,
    selectedAsset: { fileName: "Codey-1.2.0.dmg", size: 1048576, url: "https://example.com" },
  };
  const checking = h.render().checkForUpdates();
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve(updateWithAsset);
  await checking;
  assert.ok(confirmation);
  assert.equal(confirmation.action, "download-update");
  assert.match(confirmation.title, /1\.2\.0/);
});

test("发布更新日志会同时显示在检查结果和下载确认中", async () => {
  const h = harness(false);
  let confirmation = null;
  h.options.setConfirmation = value => { confirmation = value; };
  const update = {
    ...available,
    releaseNotes: "- 修复启动稳定性\n- 优化更新说明",
    selectedAsset: { fileName: "Codey-1.2.0.dmg", size: 1048576, url: "https://example.com" },
  };
  const checking = h.render().checkForUpdates();
  h.requests[0].resolve(update);
  await checking;
  assert.ok(confirmation);
  assert.equal(confirmation.releaseNotes, update.releaseNotes);
  assert.doesNotMatch(confirmation.description, /修复启动稳定性/);
  assert.match(confirmation.description, /当前版本/);
  assert.match(confirmation.description, /是否立即下载更新/);
  assert.match(h.render().updateResult.text, /修复启动稳定性/);
});

test("发现更新后定时器链仍在，可用更新被清空后继续自动检查", async () => {
  const h = harness(true);
  h.requests[0].resolve({ ...available, updateAvailable: true });
  await settle();
  h.render();
  // 暂停只影响本次检查：下一次 tick 必须仍然排程，否则手动清空状态后自动检查会永久失效。
  assert.equal(h.timers.size, 1, "发现可用更新后仍要保留下一次检查");

  const manual = h.render().checkForUpdates();
  h.requests[1].resolve({ currentVersion: "1.1.1", latestVersion: "1.1.1", updateAvailable: false });
  await manual;
  await settle();
  h.render();

  const [tick] = [...h.timers.values()];
  assert.ok(tick, "下一次检查必须已排程");
  h.timers.clear();
  tick();
  await settle();
  assert.equal(h.requests.length, 3, "状态清空后自动检查必须重新发起请求");
});

test("点「稍后」会记下这个版本，自动检查不再重复弹窗", async () => {
  const h = harness(true);
  await settle();
  const updateWithAsset = {
    ...available,
    selectedAsset: { fileName: "Codey-1.2.0-setup.exe", size: 1048576, url: "https://example.com" },
  };

  // 「稍后」按钮 = 对话框关闭回调，这里直接驱动它。
  let confirmation = null;
  h.options.setConfirmation = (value) => {
    confirmation = value;
  };
  h.render().askDownloadUpdate(updateWithAsset);
  assert.ok(confirmation, "应当弹出下载确认");
  assert.equal(confirmation.action, "download-update");
  confirmation.onDismiss();
  assert.equal(
    h.window.sessionStorage.getItem("codey.deferredUpdateVersion"),
    "1.2.0",
    "推迟必须被记录",
  );

  // 首次自动检查返回"无更新"，hook 收尾并排程下一次检查。
  h.requests[0].resolve({ ...available, updateAvailable: false });
  await settle();
  h.render();
  assert.equal(
    h.window.sessionStorage.getItem("codey.deferredUpdateVersion"),
    "1.2.0",
    "推迟必须被记录",
  );

  // 下一次检查发现同一个版本：检查照常进行，但不该再打扰用户。
  const [tick] = [...h.timers.values()];
  assert.ok(tick, "自动检查必须已排程");
  h.timers.clear();
  confirmation = null;
  tick();
  await settle();
  assert.equal(h.requests.length, 2, "自动检查本身仍然继续");
  h.requests[1].resolve(updateWithAsset);
  await settle();
  assert.equal(confirmation, null, "已推迟的版本不应再次弹窗");
});

test("上一次安装失败会在下次启动时提示而不是静默", async () => {
  const notices = [];
  const h = harness(false, {
    version: "1.2.0",
    status: "failed",
    message: "安装没有生效：Codey.exe 未更新到 v1.2.0",
    writtenAt: 100,
  });
  h.options.setNotice = (notice) => {
    notices.push(notice);
  };
  h.render();
  await settle();

  assert.equal(notices.length, 1);
  assert.equal(notices[0].tone, "error");
  assert.match(notices[0].text, /1\.2\.0/);
  assert.match(notices[0].text, /安装没有生效/);
});

test("安装成功的报告不打扰用户，卡在 started 的报告按未完成提示", async () => {
  const installed = [];
  const installedHarness = harness(false, {
    version: "1.2.0", status: "installed", message: "", writtenAt: 100,
  });
  installedHarness.options.setNotice = (notice) => installed.push(notice);
  installedHarness.render();
  await settle();
  assert.equal(installed.length, 0, "安装成功本身不需要额外提示");

  const started = [];
  const startedHarness = harness(false, {
    version: "1.2.0", status: "started", message: "正在安装更新", writtenAt: 100,
  });
  startedHarness.options.setNotice = (notice) => started.push(notice);
  startedHarness.render();
  await settle();
  assert.equal(started.length, 1);
  assert.equal(started[0].tone, "info");
  assert.match(started[0].text, /更新未完成/);
});

const rollbackUpdate = {
  currentVersion: '2.0.0', latestVersion: '1.0.0', updateAvailable: true,
  policyId: 'rollback-1', publishId: 'rollback-1',
  rollback: { id: 'rollback-1', sourceVersion: '2.0.0', targetVersion: '1.0.0', reason: '启动异常' },
  selectedAsset: { fileName: 'Codey-1.0.0.dmg', size: 1024, url: 'https://example.com/old.dmg', sha256: 'a'.repeat(64) },
};

test('回退需要下载与安装两次确认，下载携带用户确认的授权', async () => {
  const h = harness(false); let confirmation; let saved = false;
  h.options.setConfirmation = value => { confirmation = value; };
  h.options.beforeInstall = async () => { saved = true; };
  const checking = h.render().checkForUpdates();
  h.requests[0].resolve(rollbackUpdate); await checking;
  assert.equal(confirmation.title, '回退 Codey 至 v1.0.0');
  assert.match(confirmation.description, /启动异常/);
  assert.equal(confirmation.confirmLabel, '下载回退版本');
  confirmation.onDismiss();
  assert.equal(h.sessionStorage.getItem('codey.deferredUpdateVersion'), 'rollback:rollback-1');
  confirmation.run();
  assert.equal(h.requests[1].command, 'download_update');
  assert.deepEqual(h.requests[1].args, { expectedVersion: '1.0.0', expectedPolicyId: 'rollback-1' });
  const downloaded = { ...rollbackUpdate, filePath: '/updates/old.dmg', fileName: 'Codey-1.0.0.dmg', size: 1024 };
  h.requests[1].resolve(downloaded); await settle(); h.render();
  assert.equal(confirmation.title, '确认回退并重启');
  assert.equal(confirmation.confirmLabel, '回退并重启');
  assert.match(confirmation.description, /再次验证回退授权/);
  assert.equal(h.requests.length, 2);
  confirmation.run(); await settle();
  assert.equal(saved, true);
  assert.equal(h.requests[2].command, 'install_downloaded_update');
  h.requests[2].resolve(); await settle();
});

test('下载前回退授权已失效时展示错误且不进入安装确认', async () => {
  const h = harness(false); let confirmation;
  h.options.setConfirmation = value => { confirmation = value; };
  const checking = h.render().checkForUpdates(); h.requests[0].resolve(rollbackUpdate); await checking;
  const download = h.render().downloadUpdate();
  h.requests[1].reject(new Error('发布策略已变化，请重新检查更新')); await download;
  assert.equal(h.render().updateResult.tone, 'error');
  assert.match(h.render().updateResult.text, /发布策略已变化/);
  assert.equal(h.render().downloadedUpdate, null);
  assert.equal(confirmation.action, 'download-update');
});

for (const deferred of ['rollback:rollback-1', 'rollback:older-rollback', '1.0.0']) {
  test(`自动回退提醒按授权批次区分：${deferred}`, async () => {
    const h = harness(false); let confirmation = null;
    h.options.setConfirmation = value => { confirmation = value; };
    h.sessionStorage.setItem('codey.deferredUpdateVersion', deferred); h.options.embedded = false; h.render();
    h.requests[0].resolve(rollbackUpdate); await settle();
    assert.equal(confirmation !== null, deferred !== 'rollback:rollback-1');
  });
}

test("不检查策略停止自动检查，仍可手动检查、下载和安装稳定版", async () => {
  const fixture = harness(false);
  let confirmation = null;
  let saved = false;
  fixture.options.setConfirmation = value => { confirmation = typeof value === "function" ? value(confirmation) : value; };
  fixture.options.beforeInstall = async () => { saved = true; };
  fixture.options.codeyUpdatePolicy = "off";
  fixture.options.embedded = false;
  fixture.render();
  assert.equal(fixture.requests.length, 0);
  assert.equal(fixture.timers.size, 0);
  const checking = fixture.render().checkForUpdates();
  assert.equal(fixture.requests.length, 1);
  assert.deepEqual(fixture.requests[0].args, { forceRefresh: true, manual: true });
  const stable = { ...available, selectedAsset: { fileName: "Codey-1.2.0.dmg", size: 1024, sha256: "a".repeat(64) } };
  fixture.requests[0].resolve(stable);
  await checking;
  assert.deepEqual(fixture.render().updateCheck, stable);
  assert.equal(confirmation.action, "download-update");
  confirmation.run();
  assert.equal(fixture.requests[1].command, "download_update");
  assert.deepEqual(fixture.requests[1].args, { expectedVersion: "1.2.0", expectedPolicyId: null });
  const downloaded = { latestVersion: "1.2.0", filePath: "/updates/Codey-1.2.0.dmg", fileName: "Codey-1.2.0.dmg", size: 1024 };
  fixture.requests[1].resolve(downloaded);
  await settle();
  assert.equal(confirmation.action, "install-update");
  confirmation.run();
  await settle();
  assert.equal(saved, true);
  assert.equal(fixture.requests[2].command, "install_downloaded_update");
  fixture.requests[2].resolve();
  await settle();
  assert.equal(fixture.timers.size, 0);
});

test("不检查策略的手动检查不接受实验版", async () => {
  const fixture = harness(false);
  let confirmation = null;
  fixture.options.setConfirmation = value => { confirmation = typeof value === "function" ? value(confirmation) : value; };
  fixture.options.codeyUpdatePolicy = "off";
  const checking = fixture.render().checkForUpdates();
  fixture.requests[0].resolve({ ...available, latestVersion: "1.2.0-beta.1", selectedAsset: { fileName: "beta.dmg" } });
  await checking;
  assert.equal(fixture.render().updateCheck, null);
  assert.equal(confirmation, null);
  assert.match(fixture.render().updateResult.text, /仅检查稳定版/);
});

test("切换频道丢弃进行中的旧请求并清除已发现的更新", async () => {
  const fixture = harness(false);
  const checking = fixture.render().checkForUpdates();
  fixture.options.codeyUpdatePolicy = "experimental";
  fixture.render();
  fixture.requests[0].resolve(available);
  await checking;
  assert.equal(fixture.render().updateCheck, null);
  assert.equal(fixture.window.__codeyUpdateAvailability, null);
  fixture.options.embedded = false;
  fixture.render();
  assert.equal(fixture.requests.length, 2);
});
