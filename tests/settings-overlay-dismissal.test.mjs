import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("settings overlay covers the page and closes from the mask", async () => {
  const [appSource, overlaySource, overlayCss] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
    readFile(new URL("src/overlay.css", root), "utf8"),
  ]);

  assert.match(
    appSource,
    /const persistedConfigRef = useRef<Config \| null>\(null\)/,
  );
  assert.match(
    appSource,
    /function closeSettings\(\) \{[\s\S]*setConfig\(persistedConfigRef\.current\)[\s\S]*setDirty\(false\)[\s\S]*onClose\?\.\(\)/,
  );
  assert.match(
    appSource,
    /className="panel-titlebar-close"/,
  );
  assert.match(appSource, /onClick=\{handleCloseSettings\}/);
  assert.doesNotMatch(appSource, /traffic-light/);
  assert.doesNotMatch(appSource, /macos-titlebar/);
  assert.match(appSource, /codey-settings-request-close/);
  assert.match(
    appSource,
    /window\.addEventListener\(SETTINGS_REQUEST_CLOSE_EVENT/,
  );
  assert.doesNotMatch(appSource, /aria-label="关闭设置"/);

  // Full-viewport mask with uniform 36px dialog inset.
  assert.match(overlayCss, /:host\(\[data-open\]\)/);
  assert.match(overlayCss, /:host\(\[data-closing\]\)/);
  assert.match(overlayCss, /pointer-events:\s*auto/);
  assert.match(overlayCss, /inset:\s*0/);
  assert.match(overlayCss, /--codey-overlay-inset:\s*36px/);
  assert.match(overlayCss, /padding:\s*var\(--codey-overlay-inset\)/);
  assert.match(overlayCss, /display:\s*grid/);
  assert.match(overlayCss, /height:\s*100%/);
  assert.match(overlayCss, /--codey-overlay-motion:\s*200ms/);
  assert.match(overlayCss, /transition:\s*opacity var\(--codey-overlay-motion\)/);
  assert.match(overlaySource, /inset:\s*"0px"/);

  // Freeze Codex page input while open (menu bar is usually under body).
  assert.match(overlaySource, /codey-settings-overlay-open/);
  assert.match(overlaySource, /setAttribute\("inert"/);
  assert.match(overlaySource, /lockPageInteraction/);
  assert.match(overlaySource, /addEventListener\(type, lockPageInteraction, true\)/);
  assert.match(overlaySource, /data-closing/);
  assert.match(overlaySource, /OVERLAY_MOTION_MS/);
  assert.match(overlaySource, /transitionend/);

  assert.match(overlaySource, /codey-settings-opened/);
  assert.match(overlaySource, /codey-settings-request-close/);
  assert.match(overlaySource, /backdrop\.addEventListener\("click"/);
  assert.match(overlaySource, /event\.target === backdrop/);
  assert.match(overlaySource, /host\.setAttribute\("data-open"/);
  assert.match(overlaySource, /position:\s*"fixed"/);
  assert.match(overlaySource, /zIndex:\s*"2147483647"/);
  // Toggle closes an already-open panel instead of only opening.
  assert.match(
    overlaySource,
    /toggle:\s*\(\)\s*=>\s*\{[\s\S]*if \(host\.hasAttribute\("data-open"\)\) requestClose\(\)/,
  );
});

test("operations tooltips stay inside the settings overlay", async () => {
  const [panelSource, stylesSource] = await Promise.all([
    readFile(new URL("src/OperationsPanel.tsx", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
  ]);
  // CSS ::after tips avoid Semi Tooltip portals escaping the Shadow DOM.
  assert.match(panelSource, /data-codey-tip=\{tip\}/);
  assert.match(stylesSource, /\[data-codey-tip\]::after/);
  assert.match(stylesSource, /pointer-events:\s*none/);
  assert.match(
    stylesSource,
    /\.operations-icon-badge\[data-codey-tip\]::after/,
  );
});
