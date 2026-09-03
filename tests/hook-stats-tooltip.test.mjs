import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

test("oversized conversation detail tooltips stay inside their scrollable surface", () => {
  assert.match(source, /const conversationRichTooltipOpenClass = "codey-rich-tooltip-open"/);
  assert.match(
    source,
    /const conversationRichTooltipTriggerSelector = "button, \[role=\\"button\\"\], span\[tabindex=\\"0\\"\]"/,
  );
  assert.doesNotMatch(source, /body:has\(\$\{/);

  const rule = source.match(
    /body\.\$\{conversationRichTooltipOpenClass\} \[role="tooltip"\] \{([^}]*)\}/,
  )?.[1] || "";
  assert.match(rule, /overflow-x: hidden !important/);
  assert.match(rule, /overflow-y: auto !important/);
  assert.match(rule, /overscroll-behavior: contain/);
  assert.doesNotMatch(rule, /pointer-events/);
});

test("conversation rich tooltips reuse the session-tools observer instead of body:has", () => {
  const sessionObserverFilter = source.match(
    /attributeFilter:\s*\[([\s\S]*?)\],\s*childList:\s*true/,
  )?.[1] ?? "";
  assert.match(sessionObserverFilter, /"aria-describedby"/);
  assert.match(source, /if \(mutation\.attributeName === "aria-describedby"\) \{/);
  assert.match(source, /syncConversationRichTooltipOpen\(target\)/);
  assert.match(source, /syncConversationRichTooltipOpen\(\);/);
});

test("pointer handoff keeps conversation rich tooltips open while entering them", () => {
  assert.match(source, /const conversationRichTooltipHandoffMs = 150/);
  assert.match(source, /event\.stopPropagation\(\)/);
  assert.match(source, /new PointerEvent\("pointerout", \{/);
  assert.match(
    source,
    /document\.addEventListener\("pointerout", holdConversationRichTooltipOpen, true\)/,
  );
  assert.match(
    source,
    /document\.addEventListener\("pointerover", continueConversationRichTooltipHandoff, true\)/,
  );
});
