import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("settings modal keeps dismissal and stacking inside the overlay", async () => {
  const [appSource, shellSource, overlaySource, stylesSource, constants] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/SettingsModalShell.tsx", root), "utf8"),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
    loadTypeScriptModule(new URL("../src/overlay.constants.ts", import.meta.url)),
  ]);

  assert.match(
    appSource,
    /function closeSettings\(\) \{[\s\S]*setConfig\(persistedConfigRef\.current\)[\s\S]*setDirty\(false\)[\s\S]*onClose\?\.\(\)/,
  );
  assert.match(appSource, /function closeSettings\(\) \{\s*if \(isBusy\) return;/);
  assert.match(
    appSource,
    /aria-label="关闭配置"[\s\S]{0,180}disabled=\{isBusy\}[\s\S]{0,100}onClick=\{handleCloseSettings\}/,
  );
  assert.match(
    shellSource,
    /<Modal[\s\S]*closeOnClickOutside=\{false\}[\s\S]*closeOnEscape=\{false\}[\s\S]*onClose=\{onCancel\}/,
  );
  assert.match(shellSource, /lockScroll=\{false\}/);
  assert.match(shellSource, /data-codey-settings-shell="true"/);
  assert.doesNotMatch(
    shellSource,
    /overlay:\s*"bg-black\/25|backdrop-blur|overlayProps=/,
  );
  assert.match(shellSource, /content:\s*\n\s*"[^"]*overflow-hidden!/);
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

test("settings controls and popups share the modal busy and portal boundaries", async () => {
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
  for (const source of [featurePolicySource, promptSource, channelDialogSource]) {
    assert.match(source, /getPopupContainer=\{\(\) => popupContainer \?\? document\.body\}/);
  }
});
