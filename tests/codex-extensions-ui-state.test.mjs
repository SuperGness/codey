import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
const {
  draftChanged,
  editorError,
  matchesResource,
  selectResources,
  pageResources,
  canToggle,
  batchTargets,
  dependencyStatus,
} = await loadTypeScriptModule(
  new URL("../src/features/codex-extensions/state.ts", import.meta.url),
);
test("dirty draft includes identity changes and preserves unchanged close", () => {
  const draft = {
    kind: "mcp",
    id: "a",
    originalId: "a",
    content: "x",
    original: "x",
  };
  assert.equal(draftChanged(draft), false);
  assert.equal(draftChanged({ ...draft, id: "b" }), true);
  assert.equal(draftChanged({ ...draft, content: "y" }), true);
});
test("imports require absolute paths on both supported systems", () => {
  for (const content of [
    "/Users/demo/skill",
    "C:\\skills\\demo.zip",
    "\\\\server\\share\\skill",
  ])
    assert.equal(editorError({ kind: "install", content }), "");
  assert.match(
    editorError({ kind: "install", content: "../skill" }),
    /绝对路径/,
  );
});
test("search combines ownership-safe state filtering with case-insensitive source lookup", () => {
  const entry = {
    id: "a",
    name: "Browser",
    sourcePath: "/Users/demo/skills",
    description: "网页",
    enabled: false,
    readOnly: true,
  };
  assert.equal(matchesResource(entry, "browser", "disabled"), true);
  assert.equal(matchesResource(entry, "/users/demo", "readonly"), true);
  assert.equal(matchesResource(entry, "", "enabled"), false);
  assert.equal(
    matchesResource({ ...entry, enabledKnown: false }, "", "disabled"),
    false,
  );
  assert.equal(
    matchesResource(
      { ...entry, enabled: true, enabledKnown: false },
      "",
      "enabled",
    ),
    false,
  );
});
test("pagination caps rendered items and clamps invalid pages", () => {
  const entries = Array.from({ length: 1000 }, (_, id) => ({ id }));
  assert.equal(pageResources(entries, 1).entries.length, 20);
  assert.equal(pageResources(entries, 999).page, 50);
  assert.equal(pageResources(entries, 0).page, 1);
  assert.deepEqual(pageResources([], 5), { entries: [], page: 1, pages: 1 });
});
test("filter and sorting compose without changing source order", () => {
  const entries = [
    {
      name: "Zebra",
      id: "z",
      sourcePath: "a",
      ownership: "managed",
      enabled: false,
      updatedAt: "2020-01-01",
    },
    {
      name: "Apple",
      id: "a",
      sourcePath: "a",
      ownership: "external",
      enabled: true,
      updatedAt: "2025-01-01",
    },
  ];
  assert.deepEqual(
    selectResources(entries, "", "all", "all", "name").map((entry) => entry.id),
    ["a", "z"],
  );
  assert.deepEqual(
    selectResources(entries, "", "all", "managed", "updated").map(
      (entry) => entry.id,
    ),
    ["z"],
  );
  assert.equal(entries[0].id, "z");
});
test("unknown, readonly and invalid disabled resources cannot be enabled", () => {
  const entry = { enabled: false, readOnly: false };
  assert.equal(canToggle(entry), true);
  for (const extra of [
    { enabledKnown: false },
    { readOnly: true },
    { canToggle: false },
    { configurationStatus: "invalid" },
  ])
    assert.equal(canToggle({ ...entry, ...extra }), false);
  assert.equal(
    canToggle({ ...entry, enabled: true, configurationStatus: "invalid" }),
    true,
  );
});
test("editor rejects oversize identifiers and malformed Skill metadata", () => {
  assert.match(
    editorError({ kind: "mcp", id: "a".repeat(129), content: "command = 'x'" }),
    /128/,
  );
  assert.match(
    editorError({ kind: "skill", content: "# No metadata" }),
    /元数据/,
  );
  assert.equal(
    editorError({
      kind: "skill",
      content: "---\nname: demo\ndescription: test\n---\n",
    }),
    "",
  );
});

test("Skill 编辑允许 4 MiB，MCP 保留 1 MiB 并按 UTF-8 字节检查", () => {
  const header = "---\nname: demo\ndescription: test\n---\n";
  const content = header + "a".repeat(2 * 1024 * 1024);
  assert.equal(editorError({ kind: "skill", content }), "");
  assert.match(editorError({ kind: "mcp", id: "demo", content }), /1 MiB/);
  assert.equal(editorError({ kind: "skill", content: header + "a".repeat(4 * 1024 * 1024 - header.length) }), "");
  assert.match(editorError({ kind: "skill", content: header + "中".repeat(1_400_000) }), /4 MiB/);
});
test("batch changes skip unchanged resources and preserve the ability to disable invalid ones", () => {
  const entries = [
    { id: "enabled-invalid", enabled: true, configurationStatus: "invalid" },
    { id: "disabled-invalid", enabled: false, configurationStatus: "invalid" },
    { id: "ready", enabled: false, configurationStatus: "valid" },
    { id: "unknown", enabled: false, enabledKnown: false },
  ];
  const selected = entries.map((entry) => entry.id);
  assert.deepEqual(batchTargets(entries, selected, true), ["ready"]);
  assert.deepEqual(batchTargets(entries, selected, false), ["enabled-invalid"]);
});
test("declared dependencies distinguish scope, ambiguous names, unknown state and invalid configuration", () => {
  const dependency = { type: "mcp", name: "docs" };
  const entry = { id: "docs", name: "docs", scope: "user", enabled: true };
  const inventory = { scope: { kind: "user" }, mcps: [entry], skills: [] };
  assert.match(dependencyStatus(dependency, "project", inventory), /未找到/);
  assert.match(dependencyStatus(dependency, "user", inventory), /已启用/);
  assert.match(
    dependencyStatus(dependency, "user", {
      ...inventory,
      mcps: [{ ...entry, enabledKnown: false }],
    }),
    /待确认/,
  );
  assert.match(
    dependencyStatus(dependency, "user", {
      ...inventory,
      mcps: [{ ...entry, configurationStatus: "invalid" }],
    }),
    /无效/,
  );
  assert.match(
    dependencyStatus(dependency, "user", {
      ...inventory,
      mcps: [entry, { ...entry, id: "other" }],
    }),
    /多个/,
  );
});
