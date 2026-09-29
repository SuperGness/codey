import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("settings drawer keeps dismissal and stacking inside the overlay", async () => {
  const [appSource, draftSource, shellSource, overlaySource, stylesSource, constants] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/useDraftConfig.ts", root), "utf8"),
    readFile(new URL("src/SettingsDrawerShell.tsx", root), "utf8"),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
    loadTypeScriptModule(new URL("../src/overlay.constants.ts", import.meta.url)),
  ]);

  assert.match(
    appSource,
    /function closeSettings\(\) \{[\s\S]*discardDraft\(\)[\s\S]*onClose\?\.\(\)/,
  );
  assert.match(
    draftSource,
    /function discardDraft\(\) \{[\s\S]*setConfig\(persistedConfigRef\.current\)[\s\S]*setDirty\(false\)/,
  );
  assert.match(appSource, /function closeSettings\(\) \{\s*if \(isBusy\) return;/);
  assert.match(
    appSource,
    /aria-label="关闭配置"[\s\S]{0,180}disabled=\{isBusy\}[\s\S]{0,100}onClick=\{handleCloseSettings\}/,
  );
  assert.match(
    shellSource,
    /<Drawer[\s\S]*isOpen=\{visible\}[\s\S]*onOpenChange=\{\(open\) => \{\s*if \(!open\) onCancel\(\);/,
  );
  assert.match(shellSource, /<Drawer\.Backdrop\s+isDismissable\s+isKeyboardDismissDisabled/);
  assert.match(shellSource, /<UNSAFE_PortalProvider getContainer=\{getContainer\}>/);
  assert.match(shellSource, /className="settings-drawer-shell /);
  assert.ok(shellSource.includes('<Drawer.Content placement="right">'));
  assert.doesNotMatch(shellSource, /backdrop-blur|overlayProps=/);
  assert.match(shellSource, /settings-drawer-body flex min-h-0 flex-1 flex-col overflow-hidden/);
  assert.doesNotMatch(overlaySource, /addEventListener\("wheel"/);
  assert.match(
    stylesSource,
    /\.page-scroll\s*\{[\s\S]*overflow-y:\s*auto;[\s\S]*overscroll-behavior:\s*contain;/,
  );
  assert.match(appSource, /onCancel=\{handleCloseSettings\}/);
  assert.doesNotMatch(overlaySource, /codey-overlay-(?:backdrop|dialog)/);
  assert.match(overlaySource, /try \{\s*hashToken = decodeURIComponent/);
  assert.match(overlaySource, /toggle: \(\) => \(visible \? close\(\) : open\(\)\)/);
  assert.equal(constants.SETTINGS_OVERLAY_Z_INDEX, 2_147_483_647);
  assert.equal(constants.SETTINGS_OVERLAY_Z_INDEX_CSS, "2147483647");
});

test("settings controls and popups share the drawer busy and portal boundaries", async () => {
  const [appSource, featurePolicySource, promptSource, channelCardSource, channelDialogSource] =
    await Promise.all([
      readFile(new URL("src/App.tsx", root), "utf8"),
      readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
      readFile(new URL("src/PromptOptimizationCard.tsx", root), "utf8"),
      readFile(new URL("src/notifications/NotificationChannelsCard.tsx", root), "utf8"),
      readFile(new URL("src/notifications/NotificationChannelDialog.tsx", root), "utf8"),
    ]);

  assert.match(featurePolicySource, /className="gpu-mode-fieldset"\s*disabled=\{isBusy\}/);
  for (const setting of [
    "slimCodexPet",
    "disableTraceLogWrites",
    "protectCrashpadPending",
    "hideFullAccessWarning",
  ]) {
    assert.match(
      featurePolicySource,
      new RegExp(`checked=\\{config\\.${setting}\\}[\\s\\S]{0,80}disabled=\\{isBusy\\}`),
    );
  }
  assert.match(appSource, /const popupContainer = modalContainer \?\? null/);
  assert.match(
    channelCardSource,
    /<NotificationChannelDialog[\s\S]*popupContainer=\{popupContainer\}/,
  );
  assert.match(
    featurePolicySource,
    /<NotificationChannelsCard[\s\S]*container=\{popupContainer \?\? null\}[\s\S]*popupContainer=\{popupContainer \?\? null\}/,
  );
  assert.doesNotMatch(
    featurePolicySource,
    /<NotificationChannelsCard[\s\S]*container=\{tooltipContainer/,
  );
  // 弹层容器统一由 UiProvider 的 PortalProvider 决定，业务组件不再各自指定挂载点。
  for (const source of [appSource, featurePolicySource, promptSource, channelCardSource, channelDialogSource]) {
    assert.doesNotMatch(source, /getPopupContainer|zIndex=\{1100\}/);
  }
  assert.match(channelDialogSource, /container=\{container \?\? popupContainer \?\? undefined\}/);
});
