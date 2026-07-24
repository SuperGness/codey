import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadStartupPatchExpression() {
  const source = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.rs", import.meta.url),
    "utf8",
  ));
  const template = source.match(
    /const STARTUP_PATCH_TEMPLATE: &str = r#"\n([\s\S]*?)\n"#;/,
  )?.[1];
  assert.ok(template);
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__DISABLE_VOICE__", "false");
}

test("an incompatible optional renderer patch never blocks the Codex module response", async () => {
  const Module = process.getBuiltinModule("module");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  let installedHandler = null;
  class FakeBrowserWindow {}
  const fakeElectron = {
    BrowserWindow: FakeBrowserWindow,
    protocol: {
      handle(scheme, handler) {
        assert.equal(scheme, "app");
        installedHandler = handler;
      },
    },
  };
  Module._load = function testElectronLoader(request) {
    if (request === "electron") return fakeElectron;
    return Reflect.apply(nativeLoad, this, arguments);
  };

  const nativeConsoleError = console.error;
  const patchErrors = [];
  console.error = (...args) => { patchErrors.push(args); };

  try {
    assert.equal(
      (0, eval)(await loadStartupPatchExpression()),
      "codey-startup-patch-installed-v10",
    );
    const electron = Module._load("electron", undefined, false);
    const upstreamHandler = async () => new Response([
      "useHiddenModels:",
      "availableModels:",
      "includeUltraReasoningEffort",
      "amazonBedrock",
    ].join(" "));
    electron.protocol.handle("app", upstreamHandler);
    assert.equal(typeof installedHandler, "function");

    const response = await installedHandler({
      url: "app://-/assets/app-initial-new-codex-build.js",
    });
    assert.equal(response.ok, true);
    assert.match(await response.text(), /useHiddenModels:/);
    assert.equal(patchErrors.length, 1);
    assert.match(String(patchErrors[0][0]), /incompatible Codex renderer patch/);
  } finally {
    console.error = nativeConsoleError;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});
