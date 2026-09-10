import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const cardSource = readFileSync(
  new URL("../src/PromptOptimizationCard.tsx", import.meta.url),
  "utf8",
);
const backendSource = readFileSync(
  new URL("../backend/src/prompt_optimization.rs", import.meta.url),
  "utf8",
);
const commandSource = readFileSync(
  new URL("../backend/src/commands/prompt_optimization.rs", import.meta.url),
  "utf8",
);
const appSource = readFileSync(
  new URL("../src/App.tsx", import.meta.url),
  "utf8",
);

test("prompt optimization switches between Codey routing and manual upstream configuration", () => {
  assert.match(cardSource, /使用 Codey 路由/);
  assert.match(cardSource, /手动配置/);
  assert.match(cardSource, /OpenAI Responses/);
  assert.match(cardSource, /OpenAI Chat Completions/);
  assert.match(cardSource, /Anthropic Messages/);
  assert.match(cardSource, /<ModelCombobox/);
});

const manualComboboxSource = readFileSync(
  new URL("../src/components/ManualModelCombobox.tsx", import.meta.url),
  "utf8",
);

test("prompt optimization renders the searchable manual model combobox without remounting", () => {
  assert.doesNotMatch(cardSource, /modelSelectKey/);
  assert.match(
    cardSource,
    /<ManualModelCombobox[\s\S]*?options=\{cloudModels\}/,
  );
  assert.match(
    manualComboboxSource,
    /<AutoComplete/,
  );
  assert.match(
    manualComboboxSource,
    /onChange=\{onChange\}/,
  );
  assert.match(
    manualComboboxSource,
    /使用 \$\{trimmed\}/,
  );
  assert.doesNotMatch(cardSource, /prompt-optimization-model-create-option/);
});

test("prompt optimization supports all manual upstream request formats", () => {
  assert.match(backendSource, /fn responses_payload\(/);
  assert.match(backendSource, /fn openai_chat_payload\(/);
  assert.match(backendSource, /fn anthropic_payload\(/);
  assert.match(backendSource, /extract_anthropic_optimized_text/);
  assert.match(backendSource, /extract_responses_optimized_text\(response\)/);
  assert.match(backendSource, /extract_responses_stream_optimized_text/);
  assert.match(commandSource, /optimization\.uses_codey_route\(\)/);
  assert.match(commandSource, /ROUTER_AUTH_HEADER/);
  assert.match(commandSource, /response_store: uses_official_account\.then_some\(false\)/);
  assert.match(commandSource, /response_stream: uses_official_account\.then_some\(true\)/);
  assert.match(commandSource, /response_omit_max_output_tokens: uses_official_account/);
  assert.match(backendSource, /remove\("max_output_tokens"\)/);
});

test("prompt optimization connection tests use the shared toast instead of inline results", () => {
  assert.match(appSource, /<PromptOptimizationCard[\s\S]*?onNotice=\{setNotice\}/);
  assert.match(cardSource, /onNotice: \(notice: Notice\) => void/);
  assert.match(cardSource, /showTestNotice\(\s*"success"/);
  assert.match(cardSource, /showTestNotice\(\s*"error"/);
  assert.doesNotMatch(cardSource, /<NoticeToast/);
  assert.doesNotMatch(cardSource, /\btestResult\b/);
});
