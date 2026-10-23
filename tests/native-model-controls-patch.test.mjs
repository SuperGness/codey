import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";
import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

test("Fast access follows model tiers for every login and third-party routes keep Ultra", async () => {
  const records = [];
  const savedLog = globalThis.__codeyPatchLog;
  globalThis.__codeyPatchLog = (record) => records.push(JSON.parse(record).operation);
  try {
    (0, eval)(await loadStartupPatchTemplate());
    assert.equal(globalThis.__codeyRendererPatchInstalled, true);
    // CDP Fetch interception entry: null means the paused asset keeps its
    // original body, a string means the response is rewritten.
    const patchAsset = (
      fixture,
      url = "app://-/assets/app-initial-native-controls-fixture.js",
    ) => globalThis.__codeyRendererPatchEntry(url, fixture) ?? fixture;

    const ultraQuerySource = [
      "const ultraGate={},queryFactory=(_,factory)=>factory;",
      "const query=queryFactory(null,({includeUltraReasoningEffort:i,isCustomModelProvider:a},",
      "{get:l})=>{let m=i&&l(ultraGate,`1186680773`);",
      "return {select:({data:r,enabledReasoningEfforts:n})=>r.map(e=>({...e,",
      "supportedReasoningEfforts:e.supportedReasoningEfforts.filter(",
      "({reasoningEffort:e})=>(m||e!==`ultra`)&&n.has(e))}))}});",
    ].join("");
    const patchedUltraQuery = await patchAsset(ultraQuerySource);
    const ultraQuery = Function(`${patchedUltraQuery};return query;`)();
    const nativeEfforts = ["low", "medium", "high", "xhigh", "max", "ultra"];
    const routeModels = ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
      .map(model => ({
        model: `relay/${model}`,
        supportedReasoningEfforts: nativeEfforts.map(reasoningEffort => ({reasoningEffort})),
      }));
    routeModels.push({
      model: "relay/other-model",
      supportedReasoningEfforts: [{reasoningEffort: "high"}],
    });
    for (const [customProvider, remoteGate, includeUltra, enabledUltra, expectedUltra] of [
      [true, false, true, true, true],
      [true, true, true, true, true],
      [true, false, false, true, false],
      [true, false, true, false, false],
      [false, false, true, true, false],
      [false, true, true, true, true],
    ]) {
      const result = ultraQuery({
        includeUltraReasoningEffort: includeUltra,
        isCustomModelProvider: customProvider,
      }, {get: () => remoteGate}).select({
        data: routeModels,
        enabledReasoningEfforts: new Set(nativeEfforts.filter(e => enabledUltra || e !== "ultra")),
      });
      for (const model of result.slice(0, 4)) {
        assert.deepEqual(
          model.supportedReasoningEfforts.map(e => e.reasoningEffort),
          nativeEfforts.filter(e => expectedUltra || e !== "ultra"),
        );
      }
      assert.deepEqual(result[4], routeModels[4], "models without ultra must not gain it");
    }

    // Codex 26.1013 app-shared tier helpers: account requirements decide
    // whether a declared Fast or Ultrafast tier may be shown and sent.
    const tierAccessSource = [
      "function Uat(e){let t=e.requirements?.featureRequirements,n=t?.fast_mode!==!1;",
      "return{fast:n,ultrafast:(`supportsIndependentSpeedModes`in e&&e.supportsIndependentSpeedModes===!0||n)&&t?.ultrafast_mode!==!1}}",
      "function Wat(e,t){if(e==null||e===eot)return!0;if(t==null)return!1;let n=Gat(e);return e===`flex`||t[n??`fast`]}",
      "function Gat(e,t){let n=t?.trim().toLowerCase();return e===`priority`||e===`fast`||n===`fast`?`fast`:e===$at||n===`ultrafast`?`ultrafast`:null}",
      "function Xat(e,t,n={fast:!0,ultrafast:!0}){return t===eot||!Wat(t,n)?null:t}",
      "var $at=`ultrafast`,eot=`default`;",
    ].join("");
    const nativeTiers = Function(`${tierAccessSource};return {Uat,Wat}`)();
    const deniedAccess = nativeTiers.Uat({
      requirements: { featureRequirements: { fast_mode: false, ultrafast_mode: false } },
    });
    assert.equal(nativeTiers.Wat("priority", null), false, "API key logins have no native access");
    assert.equal(nativeTiers.Wat("priority", deniedAccess), false);
    const patchedTierAccess = patchAsset(
      tierAccessSource,
      "app://-/assets/app-shared-tier-access-fixture.js",
    );
    const tiers = Function(`${patchedTierAccess};return {Wat,Xat}`)();
    for (const access of [null, deniedAccess]) {
      assert.equal(tiers.Wat("priority", access), true);
      assert.equal(tiers.Wat("ultrafast", access), true);
      assert.equal(tiers.Xat(null, "priority", access), "priority");
    }
    assert.equal(tiers.Xat(null, "default", null), null, "standard speed still sends no tier");

    // Codex 26.1013 app-initial access hook: only finished ChatGPT requirements
    // produce an access object; null hides the picker entry and settings row.
    const accessHookSource = [
      "function Mxo(e,i,query,t){let r=e?.hostId??`local`,a=i?.authMethod===`chatgpt`||i?.authMethod===`personalAccessToken`,o=i?.authMethod??null,s;",
      "t[0]!==r||t[1]!==o?(s={authMethod:o,hostId:r},t[0]=r,t[1]=o,t[2]=s):s=t[2];",
      "let{data:c,isPending:l}=query,u=!!i?.isLoading||a&&l,d;",
      "t[3]!==c||t[4]!==u||t[5]!==a?(d=a&&!u&&c!=null?Tr(c):null,t[3]=c,t[4]=u,t[5]=a,t[6]=d):d=t[6];",
      "let f=d,p;return t[7]!==u||t[8]!==f?(p={serviceTierAccess:f,isLoading:u},t[7]=u,t[8]=f,t[9]=p):p=t[9],p}",
      "function Tr(e){return{fast:e.fast_mode!==!1,ultrafast:!1}}",
    ].join("");
    const accessHook = Function(`${patchAsset(accessHookSource)};return Mxo`)();
    for (const [auth, query] of [
      [{ authMethod: "apikey" }, { data: null, isPending: false }],
      [{ authMethod: "chatgpt" }, { data: null, isPending: true }],
      [{ authMethod: "chatgpt" }, { data: { fast_mode: false }, isPending: false }],
    ]) {
      assert.deepEqual(
        accessHook({ hostId: "local" }, auth, query, []).serviceTierAccess,
        { fast: true, ultrafast: true },
      );
    }
    assert.deepEqual(records, [], "current Fast shapes must patch without compatibility errors");

    // Assets outside the patch set return null so the paused request keeps its
    // original body — the entry never reads or clones them.
    for (const url of [
      "app://-/assets/unrelated-windows-chunk.js",
      "app://-/assets/general-settings-679bf39c78a4.js",
      "app://-/assets/use-service-tier-settings-XUBE8MwV.js",
    ]) {
      assert.equal(
        globalThis.__codeyRendererPatchEntry(url, tierAccessSource),
        null,
      );
    }
  } finally {
    if (savedLog === undefined) delete globalThis.__codeyPatchLog;
    else globalThis.__codeyPatchLog = savedLog;
    delete globalThis.__codeyRendererPatchEntry;
    delete globalThis.__codeyRendererPatchInstalled;
    delete globalThis.__CODEY_RENDERER_PATCH_OPTIONS__;
  }
});

test("starting or restarting Codex replaces the old runtime with one managed by Codey", async () => {
  const [runtimeSource, launcherSource, launcherProcessSource, launcherPlatformSource, appSource] =
    await Promise.all([
    readSource("backend/src/commands/runtime.rs"),
    readSource("backend/src/launcher.rs"),
    readSource("backend/src/launcher/process.rs"),
    readSource("backend/src/launcher/platform.rs"),
    readSource("src/App.tsx"),
  ]);
  const launcherModules =
    `${launcherSource}\n${launcherProcessSource}\n${launcherPlatformSource}`;
  const restartFlow = runtimeSource.slice(
    runtimeSource.indexOf("pub async fn schedule_restart_codey_runtime"),
    runtimeSource.indexOf("pub async fn stop_codey_runtime"),
  );
  const prepareLaunchFlow = launcherProcessSource.slice(
    launcherProcessSource.indexOf("async fn prepare_codex_for_launch"),
    launcherProcessSource.indexOf("fn startup_patch_detail"),
  );

  assert.match(restartFlow, /runtime_operation\.lock\(\)/);
  assert.match(restartFlow, /stop_codey_runtime_locked\(&restart_state\)/);
  assert.match(restartFlow, /launch_codey_inner_locked\(&restart_state\)/);
  assert.match(restartFlow, /runtime_generation/);
  assert.match(restartFlow, /restart_task/);
  assert.match(restartFlow, /oneshot::channel\(\)/);
  assert.match(restartFlow, /is_shutting_down\(\)/);
  assert.match(
    restartFlow,
    /request_shutdown_with_reason\(super::AppShutdownReason::RuntimeRestartFailed\)/,
  );
  assert.match(
    runtimeSource,
    /pub async fn begin_shutdown[\s\S]*?cancel\.send\(\(\)\)[\s\S]*?task\.await/,
  );
  assert.match(
    runtimeSource,
    /let current_generation = exit_state\.runtime_generation\.load\(Ordering::Acquire\);\s*let current_runtime = current_generation == runtime_generation;[\s\S]*?if current_runtime \{\s*exit_state\.request_shutdown\(\)/,
  );
  assert.match(
    launcherModules,
    /stop_codex_processes\([\s\S]*?&self\.codex_app_path,[\s\S]*?self\.process_id,[\s\S]*?self\.process_group_id/,
  );
  // Any Codex install can hold Electron's single-instance lock, so the
  // pre-launch stop sweeps every instance instead of one executable path.
  assert.match(
    prepareLaunchFlow,
    /stop_running_windows_codex_instances\(app_dir\)\s*\.await/,
  );
  assert.doesNotMatch(prepareLaunchFlow, /if already_running \{/);
  assert.match(
    prepareLaunchFlow,
    /if macos_codex_is_running\(app_dir\)\.await\? \{[\s\S]*?terminate_unix_codex_processes\(app_dir, None, None, None\)[\s\S]*?\.await/,
  );
  assert.doesNotMatch(prepareLaunchFlow, /anyhow::bail!/);
  assert.match(launcherModules, /build_codex_executable\(app_dir\)/);
  assert.match(launcherModules, /owned_unix_codex_process_ids/);
  assert.match(launcherModules, /libc::SIGKILL/);
  assert.doesNotMatch(runtimeSource, /"close_codex"/);
  assert.doesNotMatch(runtimeSource, /show_manual_relaunch_prompt/);
  assert.match(appSource, /await withTimeout\(invoke\("restart_codey"\),/);
  assert.match(appSource, /Codey 将自动重新拉起客户端/);
  assert.doesNotMatch(appSource, /关闭 Codex/);
});
