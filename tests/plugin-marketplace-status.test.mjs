import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

test("plugin marketplace repair is explicit and status checks stay read-only", async () => {
  const [marketplaceSource, coreMarketplaceSource, embeddedSnapshot, pluginCommands, launcherSource, appSource, sectionsSource] =
    await Promise.all([
      readFile(new URL("backend/src/plugin_marketplace.rs", root), "utf8")
        .then(normalizeLineEndings),
      readFile(new URL("vendor/CodeyRuntime/crates/codey-runtime-core/src/plugin_marketplace.rs", root), "utf8")
        .then(normalizeLineEndings),
      readFile(new URL("vendor/CodeyRuntime/assets/plugin-marketplaces/openai-curated-remote.zip", root)),
      readFile(new URL("backend/src/commands/plugins.rs", root), "utf8")
        .then(normalizeLineEndings),
      readFile(new URL("backend/src/launcher.rs", root), "utf8")
        .then(normalizeLineEndings),
      readFile(new URL("src/App.tsx", root), "utf8")
        .then(normalizeLineEndings),
      readFile(new URL("src/OperationsPanel.tsx", root), "utf8")
        .then(normalizeLineEndings),
    ]);

  const statusFunction = pluginCommands.match(
    /pub\(super\) async fn plugin_marketplace_status\(\)[\s\S]*?\n}\n\npub\(super\) async fn repair_plugin_marketplace/,
  )?.[0] || "";
  const repairFunction = pluginCommands.match(
    /pub\(super\) async fn repair_plugin_marketplace\(\)[\s\S]*?\n}\n\nfn decorate_plugin_marketplace_status/,
  )?.[0] || "";

  assert.match(marketplaceSource, /pub fn marketplaces_status\(home: &Path\) -> Value/);
  assert.match(marketplaceSource, /pub fn ensure_marketplaces\(home: &Path\)/);
  assert.match(
    marketplaceSource,
    /ensure_openai_curated_remote_marketplace_available\(/,
  );
  assert.match(marketplaceSource, /initializedRemote/);
  assert.match(marketplaceSource, /managedConfigCompatible/);
  assert.match(coreMarketplaceSource, /include_bytes!\([^)]*openai-curated-remote\.zip/);
  assert.match(coreMarketplaceSource, /const CODEY_CURATED_MARKETPLACE: &str = "codey-curated"/);
  assert.match(coreMarketplaceSource, /install_openai_curated_remote_marketplace_zip\(/);
  assert.match(coreMarketplaceSource, /cleanup_managed_reserved_marketplace_configs\(/);
  assert.match(coreMarketplaceSource, /if !cfg!\(windows\) \|\| value\.starts_with/);
  assert.doesNotMatch(coreMarketplaceSource, /codeload\.github\.com\/openai\/plugins/);
  assert.ok(embeddedSnapshot.length > 1_000_000);
  assert.equal(embeddedSnapshot.subarray(0, 4).toString("binary"), "PK\u0003\u0004");
  assert.doesNotMatch(statusFunction, /ensure_marketplaces/);
  assert.match(statusFunction, /marketplaces_status/);
  assert.match(repairFunction, /ensure_marketplaces/);
  assert.match(repairFunction, /\.await/);
  assert.doesNotMatch(launcherSource, /plugin_marketplace::ensure_marketplaces/);
  assert.doesNotMatch(launcherSource, /plugin_marketplace::marketplaces_status/);

  assert.match(
    appSource,
    /invoke<PluginMarketplaceStatus>\(\s*"plugin_marketplace_status"\s*,?\s*\)/,
  );
  assert.match(
    appSource,
    /invoke<PluginMarketplaceStatus>\(\s*"repair_plugin_marketplace"\s*,?\s*\)/,
  );
  assert.match(appSource, /initializedRemote/);
  assert.match(sectionsSource, /仅检查当前状态，不会在打开配置页时自动修复/);
  assert.match(sectionsSource, /Codey 内置市场：快照缺失/);
  assert.match(sectionsSource, /Codey 内置快照已接管，无需联网下载/);
  assert.match(sectionsSource, /remoteMarketplaceCached/);
  assert.match(sectionsSource, /remoteRegistered/);
  assert.match(sectionsSource, /onRepairPluginMarketplace/);
  assert.match(sectionsSource, /手动修复/);
});
