import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
const { createCodexExtensionsPreview } = await loadTypeScriptModule(
  new URL("../src/dev/codexExtensionsMock.ts", import.meta.url),
);
function preview(scenario = "") {
  globalThis.window = { location: { search: `?extensions=${scenario}` } };
  return createCodexExtensionsPreview("macos");
}
test("JSON creation enables MCP automatically and editing preserves aliases", async () => {
  const request = preview();
  const initial = await request({ action: "list" });
  const created = await request({
    action: "save_mcp",
    id: "json-demo",
    createOnly: true,
    confirmed: true,
    configJson: {
      type: "http",
      url: "https://example.com/mcp",
      headers: { Test: "value" },
      enabled: false,
    },
    revision: initial.revision,
  });
  assert.equal(
    created.inventory.mcps.find((entry) => entry.id === "json-demo").enabled,
    true,
  );
  assert.equal(created.applyStatus, "applied");
  const saved = await request({ action: "get_mcp", id: "json-demo" });
  assert.deepEqual(saved.configJson.http_headers, { Test: "value" });
  assert.equal("configToml" in saved, false);
  await request({
    action: "save_mcp",
    id: "json-demo",
    configJson: { ...saved.configJson, url: "https://example.com/edited" },
    revision: created.inventory.revision,
  });
  assert.equal(
    (await request({ action: "get_mcp", id: "json-demo" })).configJson.url,
    "https://example.com/edited",
  );
});
test("JSON save rejects competing formats and repeated identities without mutation", async () => {
  const request = preview();
  const initial = await request({ action: "list" });
  const base = {
    action: "save_mcp",
    id: "docs",
    createOnly: true,
    revision: initial.revision,
  };
  await assert.rejects(
    request({
      ...base,
      configJson: { command: "node" },
      configToml: 'command = "node"',
    }),
    /仅支持 configJson/,
  );
  await assert.rejects(
    request({ ...base, configJson: { command: "node" } }),
    /已存在/,
  );
  assert.equal((await request({ action: "list" })).revision, initial.revision);
});
test("unknown IDs produce recoverable errors", async () => {
  const request = preview();
  for (const action of [
    "get_mcp",
    "read_skill",
    "export_skill",
    "test_mcp",
    "validate_skill",
  ])
    await assert.rejects(
      request({ action, id: "missing", confirmed: true }),
      /不存在/,
    );
});
test("Skill create, export and edit preserve disabled default and content", async () => {
  const request = preview(),
    initial = await request({ action: "list" });
  const content = "---\nname: demo\ndescription: Example\n---\n# Demo";
  const created = await request({
    action: "create_skill",
    content,
    revision: initial.revision,
  });
  assert.equal(
    created.inventory.skills.find((entry) => entry.id === "demo").enabled,
    false,
  );
  const archive = await request({ action: "export_skill", id: "demo" });
  const bytes = Buffer.from(archive.dataBase64, "base64");
  assert.equal(bytes.readUInt32LE(), 0x04034b50);
  assert.ok(bytes.includes(Buffer.from(content)));
  const edited = await request({
    action: "save_skill",
    id: "demo",
    content: content + "\nChanged",
    revision: created.inventory.revision,
  });
  assert.equal("snapshotId" in edited, false);
  assert.equal(
    (await request({ action: "read_skill", id: "demo" })).content,
    content + "\nChanged",
  );
});
test("batch validates every target before changing anything", async () => {
  const request = preview(),
    initial = await request({ action: "list" });
  await assert.rejects(
    request({
      action: "set_mcps_enabled",
      ids: ["local", "missing"],
      enabled: true,
      revision: initial.revision,
      confirmed: true,
    }),
    /未执行任何修改/,
  );
  assert.equal(
    (await request({ action: "list" })).mcps.find(
      (entry) => entry.id === "local",
    ).enabled,
    false,
  );
  const result = await request({
    action: "set_mcps_enabled",
    ids: ["docs", "local"],
    enabled: false,
    revision: initial.revision,
    confirmed: true,
  });
  assert.ok(result.inventory.mcps.every((entry) => !entry.enabled));
  assert.equal("snapshotId" in result, false);
});
test("removed history actions are rejected without changing resources", async () => {
  const request = preview();
  const initial = await request({ action: "list" });
  for (const action of ["list_snapshots", "rollback"])
    await assert.rejects(
      request({ action, revision: initial.revision, confirmed: true }),
      /不支持/,
    );
  assert.deepEqual(await request({ action: "list" }), initial);
});
test("destructive actions require confirmation and stale versions cannot overwrite", async () => {
  const request = preview(),
    initial = await request({ action: "list" });
  await assert.rejects(
    request({ action: "remove_mcp", id: "docs", revision: initial.revision }),
    /确认/,
  );
  await request({
    action: "remove_mcp",
    id: "docs",
    revision: initial.revision,
    confirmed: true,
  });
  await assert.rejects(
    request({
      action: "save_mcp",
      id: "local",
      configJson: { command: "test" },
      revision: initial.revision,
    }),
    /配置已变化/,
  );
});
test("preview scenarios cover empty, large, permission, offline and conflict", async () => {
  assert.equal((await preview("empty")({ action: "list" })).mcps.length, 0);
  assert.equal(
    (await preview("large")({ action: "list" })).skills.length,
    1000,
  );
  assert.ok(
    (await preview("permission")({ action: "list" })).mcps.every(
      (entry) => entry.canToggle === false,
    ),
  );
  await assert.rejects(preview("offline")({ action: "list" }), /服务不可用/);
  const request = preview("conflict"),
    inventory = await request({ action: "list" });
  await assert.rejects(
    request({
      action: "set_mcp_enabled",
      id: "local",
      enabled: true,
      revision: inventory.revision,
    }),
    /其他程序修改/,
  );
});
test("external skills can be removed while builtin and cached ones stay read-only", async () => {
  const request = preview(),
    initial = await request({ action: "list" });
  const external = initial.skills.find(
    (entry) => entry.ownership === "external",
  );
  const builtin = initial.skills.find((entry) => entry.ownership === "builtin");
  assert.equal(external.canRemove, true);
  assert.equal(builtin.canRemove, false);
  const removed = await request({
    action: "uninstall_skill",
    id: external.id,
    revision: initial.revision,
    confirmed: true,
  });
  assert.equal(
    removed.inventory.skills.some((entry) => entry.id === external.id),
    false,
  );
});

test("Skill 预览不写入清单，安装要求来源版本匹配且默认禁用", async () => {
  const request = preview();
  const initial = await request({ action: "list" });
  const sourcePath = "/Users/preview/Downloads/demo.zip";
  const source = await request({ action: "preview_skill_install", sourcePath });
  assert.equal(source.sourceKind, "zip");
  assert.equal(source.fileCount, source.files.length);
  assert.deepEqual(await request({ action: "list" }), initial);
  await assert.rejects(request({ action: "install_skill", sourcePath, sourceRevision: "outdated", revision: initial.revision }), /重新预览/);
  assert.deepEqual(await request({ action: "list" }), initial);
  const result = await request({ action: "install_skill", sourcePath, sourceRevision: source.sourceRevision, revision: initial.revision });
  assert.equal(result.inventory.skills.find((entry) => entry.name === source.name).enabled, false);
});
