import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const root = new URL("../", import.meta.url);

test("renderer core waits for sidebar interaction before loading session tools", async () => {
  const [inject, sessionTools, petShield, voiceShield] = await Promise.all([
    readFile(new URL("public/renderer-inject.js", root), "utf8"),
    readFile(new URL("public/codey-inject.js", root), "utf8"),
    readFile(new URL("public/pet-control-shield.js", root), "utf8"),
    readFile(new URL("public/voice-control-shield.js", root), "utf8"),
  ]);

  assert.match(inject, /const queryWithin = \(root, selector\)/);
  assert.match(inject, /const sessionToolsLoadPath = "\/internal\/codey\/session-tools\/load"/);
  assert.match(inject, /const sidebarSelector = \[/);
  assert.match(inject, /const loadSessionTools = \(\) =>/);
  assert.match(inject, /sidebarDetected\(root\)\) armSessionToolsInteraction\(\)/);
  assert.match(inject, /document\.addEventListener\("pointerover", loadSessionToolsFromInteraction/);
  assert.match(inject, /document\.addEventListener\("focusin", loadSessionToolsFromInteraction/);
  assert.match(inject, /bootstrapObserver\?\.disconnect\(\)/);
  assert.match(inject, /new MutationObserver\(\(mutations\) =>/);
  assert.match(inject, /scheduleScan\(element\)/);
  assert.doesNotMatch(inject, /new MutationObserver\(\(\) => \{[\s\S]*setTimeout\(scan,/);
  assert.doesNotMatch(inject, /characterData:\s*true/);
  assert.doesNotMatch(inject, /mutation\.type === "characterData"/);
  assert.doesNotMatch(inject, /const sidebarTitleCache = new Map\(\)/);
  assert.doesNotMatch(inject, /callBridge\("\/session\/wake-watcher"\)/);
  assert.match(sessionTools, /const sidebarTitleCache = new Map\(\)/);
  assert.match(sessionTools, /syncSidebarTitles\(root\)/);
  assert.match(sessionTools, /callBridge\("\/session\/wake-watcher"\)/);
  assert.match(sessionTools, /document\.addEventListener\("pointerdown", wakeSessionWatcher/);
  assert.match(sessionTools, /document\.addEventListener\("keydown", wakeSessionWatcherFromKey/);
  assert.doesNotMatch(inject, /__codeyBlockNativePetControls/);
  assert.match(petShield, /const block = \(root = document\)/);
  assert.match(petShield, /if \(!enabled\) \{/);
  assert.match(petShield, /controlObserver = new MutationObserver/);
  assert.match(voiceShield, /const block = \(root = document\)/);
  assert.match(voiceShield, /if \(!enabled\) \{/);
});

test("plugin bridge fast-paths unrelated IPC payloads without a DOM observer", async () => {
  const source = await readFile(new URL("public/plugin-marketplace-fix.js", root), "utf8");
  const nativeCalls = [];
  const window = {
    __codeyCall: async () => ({
      plugins: [{
        id: "local-tool@local",
        marketplaceName: "local",
        name: "local-tool",
      }],
    }),
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView(...args) {
        nativeCalls.push(args);
        return Promise.resolve({
          plugins: [{
            hidden: true,
            id: "remote-tool@remote",
            marketplace: "remote",
            name: "remote-tool",
          }],
        });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {
      constructor(type, options = {}) {
        this.type = type;
        this.detail = options.detail;
      }
    },
    console,
    window,
  });
  await Promise.resolve();

  const cyclicPayload = { channel: "thread-update" };
  cyclicPayload.self = cyclicPayload;
  await window.electronBridge.sendMessageFromView(cyclicPayload);
  assert.equal(nativeCalls[0][0], cyclicPayload);

  const response = await window.electronBridge.sendMessageFromView({
    channel: "list-plugins",
    options: { includeHidden: false, includeRemote: false },
  });
  assert.equal(nativeCalls[1][0].options.includeHidden, true);
  assert.equal(nativeCalls[1][0].options.includeRemote, true);
  assert.equal(response.plugins[0].hidden, false);
  assert.equal(response.plugins.some((plugin) => plugin.id === "local-tool@local"), true);

  await window.electronBridge.sendMessageFromView({
    channel: "invoke",
    payload: {
      method: "list-plugins",
      options: { includeHidden: false, includeRemote: false },
    },
  });
  assert.equal(nativeCalls[2][0].payload.options.includeHidden, true);
  assert.equal(nativeCalls[2][0].payload.options.includeRemote, true);

  await window.electronBridge.sendMessageFromView({
    type: "invoke",
    payload: {
      request: {
        method: "list-plugins",
        options: { includeHidden: false, includeRemote: false },
      },
    },
  });
  assert.equal(nativeCalls[3][0].payload.request.options.includeHidden, true);
  assert.equal(nativeCalls[3][0].payload.request.options.includeRemote, true);

  const cyclicPluginPayload = {
    channel: "list-plugins",
    options: { includeHidden: false },
  };
  cyclicPluginPayload.self = cyclicPluginPayload;
  await window.electronBridge.sendMessageFromView(cyclicPluginPayload);
  assert.equal(nativeCalls[4][0].options.includeHidden, true);
  assert.equal(nativeCalls[4][0].self, nativeCalls[4][0]);

  const throwingPayload = {};
  Object.defineProperty(throwingPayload, "channel", {
    enumerable: true,
    get() {
      throw new Error("hostile getter");
    },
  });
  await window.electronBridge.sendMessageFromView(throwingPayload);
  assert.equal(nativeCalls[5][0], throwingPayload);

  assert.doesNotMatch(source, /JSON\.stringify\(args\)/);
  assert.doesNotMatch(source, /new MutationObserver/);
  assert.match(source, /directRequestKeys/);
  assert.match(source, /bridgeRetryDelay = Math\.min\(bridgeRetryDelay \* 2, 2_000\)/);
  assert.match(source, /const delay = fastRetry \? bridgeRetryDelay : 30_000/);

  const replacementCalls = [];
  window.electronBridge.sendMessageFromView = (...args) => {
    replacementCalls.push(args);
    return Promise.resolve({ plugins: [] });
  };
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });
  await window.electronBridge.sendMessageFromView({
    channel: "list-plugins",
    options: { includeHidden: false },
  });
  assert.equal(replacementCalls[0][0].options.includeHidden, true);
});

test("plugin mutations queue one trailing list refresh while a refresh is in flight", async () => {
  const source = await readFile(new URL("public/plugin-marketplace-fix.js", root), "utf8");
  const listResolvers = [];
  let listCalls = 0;
  const window = {
    __codeyCall() {
      listCalls += 1;
      return new Promise((resolve) => listResolvers.push(resolve));
    },
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });

  await window.electronBridge.sendMessageFromView({ method: "install-plugin" });
  assert.equal(listCalls, 1);
  listResolvers.shift()({ plugins: [] });
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(listCalls, 2);

  listResolvers.shift()({ plugins: [] });
  await Promise.resolve();
});
