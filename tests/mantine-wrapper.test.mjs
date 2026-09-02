import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { readAppStyles } from "./helpers/read-app-styles.mjs";

const root = new URL("../", import.meta.url);

test("shared controls are backed by Mantine without Semi remnants", async () => {
  const [wrapper, styles, packageSource] = await Promise.all([
    readFile(new URL("src/components/mantine/index.tsx", root), "utf8"),
    readAppStyles(root),
    readFile(new URL("package.json", root), "utf8"),
  ]);

  for (const component of [
    "MantineBadge",
    "MantineButton",
    "MantineCard",
    "MantineCollapse",
    "MantineCheckbox",
    "MantineInput",
    "MantineSelect",
    "MantineSwitch",
    "MantineTooltip",
  ]) {
    assert.match(wrapper, new RegExp(`\\b${component}\\b`));
  }
  assert.match(wrapper, /from "@mantine\/core"/);
  assert.match(packageSource, /"@mantine\/core": "9\.5\.2"/);
  assert.doesNotMatch(`${wrapper}\n${styles}\n${packageSource}`, /@douyinfe|\.semi-|--semi-/);
});

test("operations status details expand through Mantine Collapse", async () => {
  const source = await readFile(
    new URL("src/OperationsPanel.tsx", root),
    "utf8",
  );

  assert.match(source, /import \{ Badge, Button, Card, Collapse \} from "\.\/components\/mantine"/);
  assert.match(source, /<Collapse[\s\S]*expanded=\{Boolean\(activeCardTitle\)\}/);
  assert.match(source, /onTransitionEnd=\{handleCollapseTransitionEnd\}/);
  assert.doesNotMatch(source, /\{activeCardTitle && \(\s*<div\s+className="operations-expanded-grid"/);
});

test("standard selects leave dropdown lifecycle and positioning to Mantine", async () => {
  const wrapper = await readFile(
    new URL("src/components/mantine/index.tsx", root),
    "utf8",
  );

  assert.doesNotMatch(wrapper, /useCloseSelectOnScroll|addEventListener\("scroll"/);
  assert.doesNotMatch(wrapper, /dropdownOpened=\{|onDropdownOpen=|onDropdownClose=/);
  assert.match(wrapper, /<MantineSelect[\s\S]*comboboxProps=\{\{/);
});

test("subagent model picker uses the shared Mantine combobox primitives", async () => {
  const [wrapper, picker] = await Promise.all([
    readFile(new URL("src/components/mantine/index.tsx", root), "utf8"),
    readFile(new URL("src/components/ModelCombobox.tsx", root), "utf8"),
  ]);

  assert.match(wrapper, /export \{ Combobox, InputBase, useCombobox \}/);
  assert.match(
    picker,
    /import \{ Combobox, InputBase, useCombobox \} from "\.\/mantine"/,
  );
  assert.match(picker, /portalProps=\{portalTarget \? \{ target: portalTarget \} : undefined\}/);
  assert.match(picker, /withinPortal=\{Boolean\(portalTarget\)\}/);
  assert.match(picker, /middlewares=\{\{ flip: true, shift: true \}\}/);
});

test("settings overlay stays inside body so Mantine can detect outside clicks", async () => {
  const overlaySource = await readFile(
    new URL("src/overlay.tsx", root),
    "utf8",
  );

  assert.match(
    overlaySource,
    /function getOverlayMountTarget\(\) \{\s*return document\.body \?\? document\.documentElement;/,
  );
  assert.equal(
    overlaySource.match(/getOverlayMountTarget\(\)\.appendChild\(host\)/g)?.length,
    2,
  );
  assert.doesNotMatch(
    overlaySource,
    /document\.documentElement\.appendChild\(host\)/,
  );
});

test("route selectors render their dropdowns inside the settings overlay", async () => {
  const source = await readFile(
    new URL("src/ModelSection.tsx", root),
    "utf8",
  );

  assert.equal(
    source.match(/getPopupContainer=\{\(\) => popupContainer \?\? document\.body\}/g)?.length,
    1,
  );
  assert.equal(
    source.match(/zIndex=\{SETTINGS_OVERLAY_Z_INDEX\}/g)?.length,
    2,
  );
  assert.match(source, /<DialogContent[\s\S]{0,180}zIndex=\{SETTINGS_OVERLAY_Z_INDEX\}/);
});

test("Tailwind is compiled for both the page and Shadow DOM overlay", async () => {
  const [packageSource, viteSource, overlayViteSource, mainSource, overlaySource, tailwindSource] =
    await Promise.all([
      readFile(new URL("package.json", root), "utf8"),
      readFile(new URL("vite.config.ts", root), "utf8"),
      readFile(new URL("vite.overlay.config.ts", root), "utf8"),
      readFile(new URL("src/main.tsx", root), "utf8"),
      readFile(new URL("src/overlay.tsx", root), "utf8"),
      readFile(new URL("src/tailwind.css", root), "utf8"),
    ]);

  assert.match(packageSource, /"tailwindcss": "4\.3\.0"/);
  assert.match(packageSource, /"@tailwindcss\/vite": "4\.3\.0"/);
  assert.match(viteSource, /plugins: \[react\(\), tailwindcss\(\)\]/);
  assert.match(overlayViteSource, /plugins: \[react\(\), tailwindcss\(\)\]/);
  assert.match(mainSource, /import "\.\/tailwind\.css"/);
  assert.match(overlaySource, /import tailwindStyles from "\.\/tailwind\.css\?inline"/);
  assert.match(overlaySource, /cssVariablesSelector=":host"/);
  assert.match(overlaySource, /getRootElement=\{\(\) => host\}/);
  assert.equal(
    overlaySource.match(/setAttribute\("data-mantine-color-scheme", "light"\)/g)?.length,
    4,
  );
  assert.match(tailwindSource, /@import "tailwindcss"/);
});

test("legacy component-library CSS overrides stay removed", async () => {
  const [wrapper, styles, overlaySource] = await Promise.all([
    readFile(new URL("src/components/mantine/index.tsx", root), "utf8"),
    readAppStyles(root),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
  ]);

  assert.doesNotMatch(styles, /!important|\.codey-(?:button|input|select|switch|tag)/);
  assert.doesNotMatch(`${wrapper}\n${overlaySource}`, /styles\.components\.css|overlay\.css|all:\s*initial/);
});

test("Mantine surfaces do not erase page spacing with inline padding", async () => {
  const [wrapper, modalShell, appSource, styles, uiClasses] = await Promise.all([
    readFile(new URL("src/components/mantine/index.tsx", root), "utf8"),
    readFile(new URL("src/SettingsModalShell.tsx", root), "utf8"),
    readFile(new URL("src/App.tsx", root), "utf8"),
    readAppStyles(root),
    readFile(new URL("src/uiClasses.ts", root), "utf8"),
  ]);

  assert.doesNotMatch(wrapper, /<MantineCard[\s\S]{0,180}\bp=\{0\}/);
  assert.match(uiClasses, /surfaceCardPaddingClass = "px-5! py-\[18px\]!"/);
  assert.match(uiClasses, /flushCardClass = "p-0!"/);
  assert.match(modalShell, /inner: "p-3! max-\[760px\]:p-1\.5!"/);
  assert.match(modalShell, /min-h-0![\s\S]*px-5![\s\S]*py-2\.5!/);
  assert.doesNotMatch(
    `${styles}\n${appSource}`,
    /\.config-header-(?:inner|right|actions)|\.config-brand(?:-|\s*\{)/,
  );
  assert.match(
    styles,
    /\.notification-channel-list\s*\{\s*display:\s*grid;\s*grid-template-columns:\s*repeat\(2,\s*minmax\(0,\s*1fr\)\);/,
  );
  assert.doesNotMatch(
    styles,
    /\.notification-channel-list > li:only-child/,
  );
  assert.doesNotMatch(
    styles,
    /\.feature-grid > \.feature-card:last-child:nth-child\(odd\)/,
  );
});

test("notification channel select and input fields preserve proper icon gap and full width", async () => {
  const [dialogSource, uiClasses, wrapper] = await Promise.all([
    readFile(
      new URL("src/notifications/NotificationChannelDialog.tsx", root),
      "utf8",
    ),
    readFile(new URL("src/uiClasses.ts", root), "utf8"),
    readFile(new URL("src/components/mantine/index.tsx", root), "utf8"),
  ]);

  assert.match(dialogSource, /leftSectionWidth=\{38\}/);
  assert.match(dialogSource, /leftSectionPointerEvents="none"/);
  assert.doesNotMatch(
    dialogSource,
    /data-\[position=left\]:ml-/,
    "Left section margin should not offset icon into select label text",
  );
  assert.match(
    uiClasses,
    /\[&_\.mantine-Input-wrapper\]:flex-1/,
    "Input wrappers inside inputShellClass must flex to fill available width",
  );
  assert.match(
    wrapper,
    /className=\{classNames\("min-w-0 flex-1", wrapperClassName\)\}/,
    "Mantine Input wrapper must stretch horizontally by default",
  );
});
