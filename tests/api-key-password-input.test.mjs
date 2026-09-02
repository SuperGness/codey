import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("all configuration API keys use PasswordInput with local values", async () => {
  const [routes, promptOptimization, api, backend] = await Promise.all([
    readFile(new URL("src/ModelSection.tsx", root), "utf8"),
    readFile(new URL("src/PromptOptimizationCard.tsx", root), "utf8"),
    readFile(new URL("src/api.ts", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
  ]);

  assert.match(
    routes,
    /<PasswordInput[\s\S]*?id="route-key-input"[\s\S]*?onVisibilityChange=/,
  );
  assert.doesNotMatch(
    routes,
    /<Input[\s\S]*?id="route-key-input"[\s\S]*?type="password"/,
  );
  assert.doesNotMatch(routes, /reveal_route_api_key/);
  assert.match(
    promptOptimization,
    /<PasswordInput[\s\S]*?id=\{apiKeyInputId\}[\s\S]*?onVisibilityChange=/,
  );
  assert.doesNotMatch(promptOptimization, /reveal_prompt_optimization_api_key/);
  assert.match(promptOptimization, /已保存（输入新 Key 可替换）/);
  assert.doesNotMatch(promptOptimization, /Key 已保存；点击眼睛可查看/);
  assert.doesNotMatch(api, /reveal_(?:route|prompt_optimization)_api_key/);
  assert.doesNotMatch(backend, /profile\.api_key\.clear\(\)/);
  assert.doesNotMatch(backend, /prompt_optimization\.api_key\.clear\(\)/);
});
