import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("settings overlay only closes explicitly and restores unsaved config", async () => {
  const [appSource, overlaySource] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
  ]);

  assert.match(appSource, /const persistedConfigRef = useRef<Config \| null>\(null\)/);
  assert.match(
    appSource,
    /function closeSettings\(\) \{[\s\S]*setConfig\(persistedConfigRef\.current\)[\s\S]*setDirty\(false\)[\s\S]*onClose\?\.\(\)/,
  );
  assert.match(appSource, /onClick=\{embedded \? closeSettings : undefined\}/);
  assert.doesNotMatch(appSource, /aria-label="关闭设置"/);

  assert.doesNotMatch(overlaySource, /backdrop\.addEventListener\("click"/);
  assert.doesNotMatch(overlaySource, /addEventListener\("keydown"/);
  assert.match(overlaySource, /toggle: open/);
});

test("operations tooltips stay inside the settings overlay", async () => {
  const appSectionsSource = await readFile(
    new URL("src/AppSections.tsx", root),
    "utf8",
  );
  assert.match(appSectionsSource, /const operationsHubRef = useRef<HTMLElement>\(null\)/);
  assert.match(
    appSectionsSource,
    /operationsHubRef\.current\?\.closest<HTMLElement>\("\.app-shell"\) \?\? document\.body/,
  );
  assert.match(appSectionsSource, /ref=\{operationsHubRef\}/);
  assert.match(appSectionsSource, /getPopupContainer=\{getTooltipContainer\}/);
});
