import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const source = fs.readFileSync(
  new URL("../src/main.tsx", import.meta.url),
  "utf8",
);

test("GPT-6 preview matches the bundled model reasoning metadata", async () => {
  const { previewOfficialModels, previewUpstreamModels } = await loadTypeScriptModule(
    new URL("../src/previewModels.ts", import.meta.url),
  );
  const { models } = JSON.parse(fs.readFileSync(
    new URL("../vendor/CodeyRuntime/assets/model-catalog-metadata.json", import.meta.url),
    "utf8",
  ));
  const astra = models.find((model) => model.slug === "gpt-6-astra");
  assert.ok(astra);
  assert.deepEqual(previewOfficialModels.find((model) => model.slug === astra.slug), {
    slug: astra.slug,
    displayName: astra.display_name,
    supportedReasoningEfforts: astra.supported_reasoning_levels.map((level) => level.effort),
    defaultReasoningEffort: astra.default_reasoning_level,
  });
  assert.ok(previewUpstreamModels.includes(astra.slug));
});

test("development preview fixtures cannot be mistaken for live credentials", () => {
  assert.match(source, /example\.invalid/);
  assert.match(source, /apiKey: "preview-route-primary-key"/);
  assert.match(source, /apiKey: "preview-route-backup-key"/);
  assert.match(source, /apiKey: "preview-prompt-optimization-key"/);
  assert.match(
    source,
    /kind: "telegram"[\s\S]*?botToken: ""[\s\S]*?botTokenConfigured: true/,
  );
  assert.match(source, /preview-chat-id/);
  assert.doesNotMatch(source, /\bsk-(?:proj-)?[A-Za-z0-9._-]+/);
  assert.doesNotMatch(source, /api\.(?:openai|anthropic)\.com/);
  assert.doesNotMatch(
    source,
    /open\.feishu\.cn\/open-apis\/bot\/v2\/hook\/[A-Za-z0-9-]+/,
  );
  assert.doesNotMatch(
    source,
    /qyapi\.weixin\.qq\.com\/cgi-bin\/webhook\/send\?key=[A-Za-z0-9-]+/,
  );
});

test("development preview follows the current runtime-status contract", () => {
  assert.match(source, /visibility: "internal"/);
  assert.match(source, /visibility: "feature"/);
  assert.match(
    source,
    /id: "renderer-controls"[\s\S]*?visibility: "internal"/,
  );
  assert.match(source, /fastContextToolsActive: previewConfig\.fastContextTools/);
  assert.match(
    source,
    /subagentOptimizationActive: previewConfig\.subagentOptimization/,
  );
  assert.match(source, /notificationChannelsActive: activeNotificationChannelCount > 0/);
  assert.match(source, /activeNotificationChannelCount,/);
  assert.match(
    source,
    /traceLogWriteProtectionActive: previewConfig\.disableTraceLogWrites/,
  );
  assert.match(
    source,
    /crashpadDiskProtectionActive:[\s\S]*?previewClientPlatform === "macos"[\s\S]*?previewConfig\.protectCrashpadPending/,
  );
  assert.doesNotMatch(source, /command === "refresh_injection_status"/);
});

test("development preview exercises cross-route subagent model selection", () => {
  assert.match(
    source,
    /backup: \["claude-sonnet-4-5", "claude-opus-4-1"\]/,
  );
  assert.match(source, /model: "backup\/claude-sonnet-4-5"/);
});
