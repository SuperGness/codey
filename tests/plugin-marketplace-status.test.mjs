import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("plugin marketplace repair is explicit and status checks stay read-only", async () => {
  const [marketplaceSource, commandSource, launcherSource, appSource, sectionsSource] =
    await Promise.all([
      readFile(new URL("backend/src/plugin_marketplace.rs", root), "utf8"),
      readFile(new URL("backend/src/commands.rs", root), "utf8"),
      readFile(new URL("backend/src/launcher.rs", root), "utf8"),
      readFile(new URL("src/App.tsx", root), "utf8"),
      readFile(new URL("src/AppSections.tsx", root), "utf8"),
    ]);

  const statusFunction = commandSource.match(
    /pub async fn plugin_marketplace_status\(\)[\s\S]*?\n}\n\npub async fn repair_plugin_marketplace/,
  )?.[0] || "";
  const repairFunction = commandSource.match(
    /pub async fn repair_plugin_marketplace\(\)[\s\S]*?\n}\n\nfn decorate_plugin_marketplace_status/,
  )?.[0] || "";

  assert.match(marketplaceSource, /pub fn marketplaces_status\(home: &Path\) -> Value/);
  assert.doesNotMatch(statusFunction, /ensure_marketplaces/);
  assert.match(statusFunction, /marketplaces_status/);
  assert.match(repairFunction, /ensure_marketplaces/);
  assert.doesNotMatch(launcherSource, /plugin_marketplace::ensure_marketplaces/);
  assert.match(launcherSource, /plugin_marketplace::marketplaces_status/);

  assert.match(appSource, /invoke<PluginMarketplaceStatus>\("plugin_marketplace_status"\)/);
  assert.match(appSource, /invoke<PluginMarketplaceStatus>\("repair_plugin_marketplace"\)/);
  assert.match(sectionsSource, /仅检查当前状态，不会在打开配置页时自动修复/);
  assert.match(sectionsSource, /onRepairPluginMarketplace/);
  assert.match(sectionsSource, /手动修复/);
});
