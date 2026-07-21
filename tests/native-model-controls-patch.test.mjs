import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function loadPatchExpression() {
  const source = await readFile(
    new URL("../backend/src/codex_startup_patch.rs", import.meta.url),
    "utf8",
  );
  const template = source.match(
    /const STARTUP_PATCH_TEMPLATE: &str = r#"\n([\s\S]*?)\n"#;/,
  )?.[1];
  assert.ok(template, "startup patch template should be readable");
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__DISABLE_VOICE__", "false");
}

test("API auth uses Codex's native Spark and service-tier paths", async () => {
  const Module = process.getBuiltinModule("module");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  const NativeWorker = workerThreads.Worker;
  let appProtocolHandler = null;

  class FakeBrowserWindow {}
  const fakeElectron = {
    BrowserWindow: FakeBrowserWindow,
    protocol: {
      handle(scheme, handler) {
        assert.equal(scheme, "app");
        appProtocolHandler = handler;
      },
    },
  };
  Module._load = function nativeControlsTestLoader(request) {
    if (request === "electron") return fakeElectron;
    return Reflect.apply(nativeLoad, this, arguments);
  };

  try {
    assert.equal(
      (0, eval)(await loadPatchExpression()),
      "codey-startup-patch-installed-v5",
    );
    Module._load("electron", undefined, false).protocol.handle(
      "app",
      async (request) =>
        request.response ??
        new Response(request.fixture, {
          headers: { "content-type": "text/javascript" },
        }),
    );

    const patchAsset = async (
      fixture,
      url = "app://-/assets/app-initial~native-controls-fixture.js",
    ) => {
      const response = await appProtocolHandler({
        fixture,
        url,
      });
      return response.text();
    };

    const modelSource = [
      "function Ue({authMethod:e,includeUltraReasoningEffort:i,useHiddenModels:o}){",
      "let s=[],c=null,l=o&&e!==`amazonBedrock`,u=i;",
      "return {gate:l,models:s,defaultModel:c,useHiddenModels:o}}",
    ].join("");
    const patchedModel = await patchAsset(modelSource);
    assert.match(patchedModel, /l=o&&e=== `chatgpt`/);
    assert.doesNotMatch(patchedModel, /!==`amazonBedrock`/);
    const modelGate = Function(
      `${patchedModel};return (authMethod) => ` +
        "Ue({authMethod,includeUltraReasoningEffort:true,useHiddenModels:true}).gate;",
    )();
    assert.equal(modelGate("chatgpt"), true);
    assert.equal(modelGate("apikey"), false);

    const serviceTierUiSource = [
      "function U(e){let o=e,s=o?.authMethod===`chatgpt`,c=o?.authMethod??null,l;",
      "let u=o,f=false,p=s&&!f&&u!=null&&",
      "u?.requirements?.featureRequirements?.fast_mode!==!1,m;",
      "return {authMethod:c,isServiceTierAllowed:p}}",
    ].join("");
    const patchedServiceTierUi = await patchAsset(serviceTierUiSource);
    assert.match(
      patchedServiceTierUi,
      /p=!f/,
    );
    const serviceTierAllowed = Function(
      `${patchedServiceTierUi};return U;`,
    )();
    assert.equal(
      serviceTierAllowed({
        authMethod: "chatgpt",
        requirements: { featureRequirements: { fast_mode: false } },
      }).isServiceTierAllowed,
      true,
    );
    assert.equal(
      serviceTierAllowed({
        authMethod: "apikey",
        requirements: { featureRequirements: { fast_mode: false } },
      }).isServiceTierAllowed,
      true,
    );

    const serviceTierOptionsSource = [
      "const serviceTierMessageIds=[`serviceTier.standard.label`,`serviceTier.fast.label`];",
      "const messages={fastDescription:`Fast response`,fastLabel:`Fast`};",
      "const standard={description:`Default speed`,iconKind:null,label:`Standard`,tier:null,value:null};",
      "function kind(e){return e===`priority`?`fast`:null}",
      "function description(e){return e.description??messages.fastDescription}",
      "function label(e){return e.id===`priority`?messages.fastLabel:e.name}",
      "function options(e){return[standard,...(e?.serviceTiers??[]).map(e=>({",
      "description:description(e),iconKind:kind(e.id),label:label(e),tier:e,value:e.id}))]}",
      "function lookup(e,t){return e?.serviceTiers?.find(e=>e.id===t)??null}",
      "function selected(e,t){return lookup(e,t)?.id??null}",
    ].join("");
    const patchedServiceTierOptions = await patchAsset(serviceTierOptionsSource);
    assert.match(
      patchedServiceTierOptions,
      /e\?\.serviceTiers\?\.length\?e\.serviceTiers:\[\{id:`priority`,name:`Fast`\}\]/,
    );
    assert.match(
      patchedServiceTierOptions,
      /function selected\(e,t\)\{return lookup\(e,t\)\?\.id\?\?t\?\?null\}/,
    );
    const nativeServiceTierHelpers = Function(
      `${patchedServiceTierOptions};return {options,selected};`,
    )();
    assert.deepEqual(
      nativeServiceTierHelpers.options({}).map(({ iconKind, label, value }) => ({
        iconKind,
        label,
        value,
      })),
      [
        { iconKind: null, label: "Standard", value: null },
        { iconKind: "fast", label: "Fast", value: "priority" },
      ],
    );
    assert.deepEqual(
      nativeServiceTierHelpers
        .options({ serviceTiers: [] })
        .map(({ label, value }) => ({ label, value })),
      [
        { label: "Standard", value: null },
        { label: "Fast", value: "priority" },
      ],
    );
    assert.equal(nativeServiceTierHelpers.selected({}, "priority"), "priority");
    assert.deepEqual(
      nativeServiceTierHelpers
        .options({
          serviceTiers: [
            { description: "Lowest latency", id: "ultrafast", name: "Ultrafast" },
          ],
        })
        .map(({ label, value }) => ({ label, value })),
      [
        { label: "Standard", value: null },
        { label: "Ultrafast", value: "ultrafast" },
      ],
    );

    const serviceTierSettingsUiSource = [
      "function Settings(e){let {isServiceTierAllowed:n}=e,",
      "r=e.serviceTierSettings,{selectedServiceTier:s}=r;",
      "if(!n||r.availableOptions.length<=1)return null;",
      "return {availableOptions:r.availableOptions,selectedServiceTier:s}}",
    ].join("");
    const patchedServiceTierSettingsUi = await patchAsset(
      serviceTierSettingsUiSource,
      "app://-/assets/general-settings-BWZCvLqI.js",
    );
    assert.match(
      patchedServiceTierSettingsUi,
      /if\(r\.availableOptions\.length===0\)return null/,
    );
    const nativeSettings = Function(
      `${patchedServiceTierSettingsUi};return Settings;`,
    )();
    assert.deepEqual(
      nativeSettings({
        isServiceTierAllowed: false,
        serviceTierSettings: {
          availableOptions: [{ label: "Standard", value: null }],
          selectedServiceTier: null,
        },
      }).availableOptions,
      [{ label: "Standard", value: null }],
    );

    const serviceTierRequestSource = [
      "async function Qs(e,t){let n=await Js(e,t);",
      "if(n!==`chatgpt`)return!1;",
      "let r=await rt(t);return r.requirements?.featureRequirements?.fast_mode!==!1}",
      "function Zs(){throw Error(`Failed to read service tier for request`)}",
    ].join("");
    const patchedServiceTierRequest = await patchAsset(serviceTierRequestSource);
    assert.match(
      patchedServiceTierRequest,
      /if\(n!==`chatgpt`\)return!0/,
    );
    for (const url of [
      "app://-/assets/app-initial.js",
      "app://-/assets/app-initial-windows.js",
      "app://-/assets/app-initial~windows.js?build=store",
      "app://-/assets/general-settings-BWZCvLqI.js",
      "app://-/assets/windows-model-controls-a1b2c3.js",
    ]) {
      assert.match(
        await patchAsset(serviceTierRequestSource, url),
        /if\(n!==`chatgpt`\)return!0/,
      );
    }
    assert.equal(
      await patchAsset(
        "const unrelatedWindowsChunk = true;",
        "app://-/assets/unrelated-windows-chunk.js",
      ),
      "const unrelatedWindowsChunk = true;",
    );
    const unrelatedResponse = new Response(
      "const unrelatedWindowsChunk = true;",
      { headers: { "content-type": "text/javascript" } },
    );
    Object.defineProperty(unrelatedResponse, "clone", {
      value() {
        throw new Error("unrelated renderer assets must not be cloned");
      },
    });
    const bypassedResponse = await appProtocolHandler({
      response: unrelatedResponse,
      url: "app://-/assets/unrelated-windows-chunk.js",
    });
    assert.equal(bypassedResponse, unrelatedResponse);

    assert.deepEqual(
      {
        modelVisibility: globalThis.__CODEY_RENDERER_GATE_PATCH__.modelVisibility,
        serviceTierOptions:
          globalThis.__CODEY_RENDERER_GATE_PATCH__.serviceTierOptions,
        serviceTierRequest:
          globalThis.__CODEY_RENDERER_GATE_PATCH__.serviceTierRequest,
        serviceTierSettingsUi:
          globalThis.__CODEY_RENDERER_GATE_PATCH__.serviceTierSettingsUi,
        serviceTierUi: globalThis.__CODEY_RENDERER_GATE_PATCH__.serviceTierUi,
      },
      {
        modelVisibility: true,
        serviceTierOptions: true,
        serviceTierRequest: true,
        serviceTierSettingsUi: true,
        serviceTierUi: true,
      },
    );
    assert.equal(globalThis.__CODEY_RENDERER_GATE_PATCH__.lastError, null);
  } finally {
    workerThreads.Worker = NativeWorker;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});

test("closing Codex requires a manual Codey relaunch", async () => {
  const [commandsSource, appSource] = await Promise.all([
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8"),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  ]);
  const closeFlow = commandsSource.slice(
    commandsSource.indexOf("pub async fn schedule_close_codey_runtime"),
    commandsSource.indexOf("pub async fn stop_codey_runtime"),
  );

  assert.match(closeFlow, /stop_codey_runtime\(&close_state\)/);
  assert.match(closeFlow, /show_manual_relaunch_prompt\(\)\.await/);
  assert.doesNotMatch(closeFlow, /launch_codey_runtime/);
  assert.match(appSource, /await invoke\("close_codex"\)/);
  assert.match(appSource, /请按提示手动运行 Codey 重新启动/);
});
