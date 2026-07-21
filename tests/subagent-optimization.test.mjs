import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("subagent optimization is opt-in and exposed through the settings switch", async () => {
  const [appSource, sectionsSource, configSource, commandSource, launcherSource] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/AppSections.tsx", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/launcher.rs", root), "utf8"),
  ]);
  const uiSource = `${appSource}\n${sectionsSource}`;

  assert.match(configSource, /pub subagent_optimization: bool/);
  assert.match(configSource, /subagent_optimization: false/);
  assert.match(commandSource, /config\.subagent_optimization = config_input\.subagent_optimization/);
  assert.match(launcherSource, /config\.subagent_optimization/);
  assert.match(appSource, /const SUBAGENT_MODEL = "gpt-5\.6-luna"/);
  assert.match(uiSource, /checked=\{config\.subagentOptimization\}/);
  assert.match(uiSource, /onCheckedChange=\{\(checked\) => onSubagentOptimizationChange\(checked\)\}/);
  assert.match(uiSource, /aria-label="启用子代理协作优化"/);
  assert.match(uiSource, /<Badge variant="warning">需支持 GPT-5\.6-Luna<\/Badge>/);
  assert.match(appSource, /invoke\("fetch_current_provider_models"\)/);
  assert.match(appSource, /supportsModel\(result\.models, SUBAGENT_MODEL\)/);
  assert.match(appSource, /provider\.official \? "官方账号" : "第三方 API"/);
  assert.match(appSource, /不支持 \$\{SUBAGENT_MODEL\}，无法开启子代理协作优化/);
  assert.match(uiSource, /启用v2并行配置/);
  assert.doesNotMatch(uiSource, /下次启动启用 V2 并行配置，退出时自动恢复原文件/);
});

test("subagent optimization owns the requested V2 and default-agent settings", async () => {
  const source = await readFile(new URL("backend/src/codex_config.rs", root), "utf8");

  assert.match(source, /multi_agent\["enabled"\] = value\(true\)/);
  assert.match(source, /multi_agent\["hide_spawn_agent_metadata"\] = value\(true\)/);
  assert.match(source, /multi_agent\["tool_namespace"\] = value\("agents"\)/);
  assert.match(source, /multi_agent\["max_concurrent_threads_per_session"\] = value\(7\)/);
  assert.match(source, /multi_agent\["max_wait_timeout_ms"\] = value\(120_000\)/);
  assert.match(source, /doc\.as_table_mut\(\)\.remove\("agents"\)/);
  assert.match(source, /model = "gpt-5\.6-luna"/);
  assert.match(source, /model_reasoning_effort = "low"/);
  assert.match(source, /image_generation = false/);
  assert.match(source, /const SUBAGENT_GUIDANCE: &str/);
});
