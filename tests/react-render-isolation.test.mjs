import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("settings panels keep stable handlers and skip unrelated parent renders", async () => {
  const [
    app,
    appUpdates,
    notice,
    confirmation,
    sections,
    modelSelection,
  ] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/useAppUpdates.ts", root), "utf8"),
    readFile(new URL("src/useAppNotice.tsx", root), "utf8"),
    readFile(new URL("src/useConfirmationDialog.tsx", root), "utf8"),
    Promise.all(
      [
        "OperationsPanel.tsx",
        "ModelSection.tsx",
        "FeaturePolicyCard.tsx",
      ].map((file) => readFile(new URL(`src/${file}`, root), "utf8")),
    ).then((sources) => sources.join("\n")),
    readFile(new URL("src/useModelSelection.ts", root), "utf8"),
  ]);

  assert.doesNotMatch(app, /useState<Notice>/);
  assert.doesNotMatch(app, /useState<Confirmation/);
  assert.match(notice, /useSyncExternalStore\(/);
  assert.match(notice, /export const NoticeToast = memo\(/);
  assert.match(confirmation, /useSyncExternalStore\(/);
  assert.match(confirmation, /export const ConfirmationDialogHost = memo\(/);
  assert.doesNotMatch(app, /CodexAppPathDialog/);
  assert.doesNotMatch(app, /async function checkForUpdates\(/);
  assert.match(appUpdates, /export function useAppUpdates/);
  assert.match(appUpdates, /invoke<UpdateCheck>\("check_for_updates"\)/);
  assert.equal(
    appUpdates.match(/invoke<UpdateCheck>\("check_for_updates"\)/g)?.length,
    1,
  );
  assert.match(
    appUpdates,
    /updateCheckInFlightRef = useRef<Promise<UpdateCheck> \| null>/,
  );
  assert.match(appUpdates, /const result = await requestUpdateCheck\(\)/);
  assert.match(appUpdates, /invoke<UpdateDownload>\("download_update"\)/);
  assert.match(appUpdates, /invoke\("install_downloaded_update"/);
  assert.match(app, /onRepairPluginMarketplace=\{handleRepairPluginMarketplace\}/);
  assert.match(app, /onRefresh=\{handleRefreshTraceLogStats\}/);
  assert.match(app, /onToggleDraftModel=\{toggleDraftModel\}/);
  assert.match(app, /onSaveRoute=\{handleSaveRoute\}/);
  assert.doesNotMatch(app, /onActivateRoute|handleActivateRoute/);
  assert.doesNotMatch(app, /activeProfileId:\s*route\.id/);
  assert.match(app, /onDeleteRoute=\{handleDeleteRoute\}/);
  assert.match(app, /onFetchRouteModels=\{handleFetchRouteModels\}/);
  assert.match(app, /onSetDefaultModel=\{handleSetRouteDefaultModel\}/);
  assert.match(app, /onToggleLocalRouter=\{handleToggleLocalRouter\}/);
  assert.match(app, /onToggleRouteRequestLog=\{handleToggleRouteRequestLog\}/);
  assert.match(
    app,
    /onToggleAccountUsage=\{handleToggleAccountUsage\}/,
  );
  assert.match(app, /onSave=\{saveModelSelection\}/);
  assert.doesNotMatch(modelSelection, /withTimeout/);
  assert.match(modelSelection, /routeId: modelPickerRouteId/);
  assert.match(app, /routeModelState/);
  assert.doesNotMatch(app, /"activate_route"/);
  assert.doesNotMatch(app, /onSetDefaultModel=\{\(.*=>/);
  assert.doesNotMatch(
    app,
    /onRepairPluginMarketplace=\{\(\) => void repairPluginMarketplace\(\)\}/,
  );
  assert.match(sections, /aria-labelledby="route-protocol-label"/);
  assert.match(sections, /OpenAI Responses/);
  assert.match(sections, /OpenAI Chat Completions/);
  assert.match(sections, /Anthropic Messages/);
  assert.doesNotMatch(sections, /第三方 Responses 兼容/);
  assert.doesNotMatch(sections, /route-auth-mode-label/);
  assert.equal(sections.match(/<Select\s/g)?.length, 2);
  assert.equal(sections.match(/<ModelCombobox\s/g)?.length, 1);
  assert.doesNotMatch(sections, /<select|route-native-select/);
  assert.match(sections, /route-manager route-manager-balanced/);
  assert.match(sections, /route-manager-current/);
  assert.match(sections, /className="provider-model-groups"/);
  assert.match(sections, /modelState\.officialModelIds/);
  assert.match(sections, /checked=\{showAccountUsageInHeader\}/);
  assert.match(sections, /checked=\{config\.localRouterEnabled\}/);
  assert.match(sections, /const routeConfigReadOnly = !config\.localRouterEnabled/);
  assert.match(sections, /checked=\{checked\}/);
  assert.match(sections, /额度显示/);
  assert.match(
    sections,
    /routeConfigReadOnly && group\.official && \([\s\S]*provider-model-usage-toggle[\s\S]*checked=\{showAccountUsageInHeader\}/,
  );
  assert.match(sections, /<DialogTitle>/);
  assert.match(sections, /config\.localRouterEnabled && \(/);
  assert.match(sections, /checked=\{config\.routeRequestLog\.enabled\}/);
  assert.match(sections, /onCheckedChange=\{onToggleRouteRequestLog\}/);
  assert.match(sections, /查看请求日志/);
  assert.match(sections, /当前线路模型/);
  assert.match(sections, /仅展示 Codex 当前线路，可同步模型/);
  assert.match(sections, /if \(routeConfigReadOnly\) return nativeProfile \? \[nativeProfile\] : \[\]/);
  assert.match(sections, /disabled=\{!canSyncCurrentProvider \|\| isBusy\}/);
  assert.match(app, /const canSyncCurrentProvider = !dirty \|\| pendingNativeRouterToggle/);
  assert.match(app, /canSyncCurrentProvider=\{canSyncCurrentProvider\}/);
  assert.match(app, /if \(shouldPersistNativeToggle\) \{\s*await persist\(config\)/);
  assert.match(app, /if \(nativeMode\) \{\s*openModelPicker\(/);
  assert.match(app, /result\.providerStatus\.provider\.official \? null : result\.providerStatus\.provider\.id/);
  assert.match(app, /if \(nativeMode \|\| route\.authMode === "officialAccount"\) \{\s*await syncCurrentProvider\(\);\s*return/);
  assert.match(
    sections,
    /disabled=\{\s*routeConfigReadOnly \|\|\s*isBusy \|\|\s*dirty \|\|\s*config\.profiles\.length <= 1\s*\}/,
  );
  assert.match(sections, /统一模型目录/);
  assert.doesNotMatch(sections, /catalog-search|searchQuery|搜索模型\.\.\./);
  assert.match(sections, /第三方线路同时接入统一路由/);
  assert.match(sections, /已接入路由/);
  assert.doesNotMatch(sections, /aria-pressed|route-list-select/);
  assert.doesNotMatch(sections, /role="radiogroup"/);
  assert.doesNotMatch(sections, /activeRouteLocked/);

  for (const component of [
    "OperationsPanel",
    "ModelSection",
    "FeaturePolicyCard",
  ]) {
    assert.match(sections, new RegExp(`export const ${component} = memo\\(`));
  }
});

test("runtime polling preserves referentially stable status slices", async () => {
  const { reconcileRuntimeStatus } = await loadTypeScriptModule(
    new URL("../src/runtimeStatusSnapshot.ts", import.meta.url),
  );
  const current = {
    running: false,
    appVersion: "1.0.0",
    maintenance: { sessionStatus: "ready", sessionFilesFixed: 2 },
    injectionScripts: [{ id: "bridge", status: "effective" }],
    traceLogStats: { pending: false, rows: 3 },
    crashpadPendingStats: { pending: false, reports: 1 },
  };

  const equalSnapshot = structuredClone(current);
  assert.equal(reconcileRuntimeStatus(current, equalSnapshot), current);

  const changedRoot = reconcileRuntimeStatus(current, {
    ...structuredClone(current),
    running: true,
  });
  assert.notEqual(changedRoot, current);
  assert.equal(changedRoot.maintenance, current.maintenance);
  assert.equal(changedRoot.injectionScripts, current.injectionScripts);
  assert.equal(changedRoot.traceLogStats, current.traceLogStats);
  assert.equal(changedRoot.crashpadPendingStats, current.crashpadPendingStats);

  const changedMaintenance = reconcileRuntimeStatus(current, {
    ...structuredClone(current),
    maintenance: { sessionStatus: "error", sessionFilesFixed: 2 },
  });
  assert.notEqual(changedMaintenance.maintenance, current.maintenance);
});
