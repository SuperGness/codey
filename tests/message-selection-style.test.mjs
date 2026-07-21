import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

test("adjacent selected turns render as one continuous outline", () => {
  assert.match(
    source,
    /\.\$\{selectedClass\}::before \{[^}]*border: 3px solid #7c8cff;/s,
  );
  assert.match(
    source,
    /data-codey-selected-previous="true"[^}]*border-top: 0;/s,
  );
  assert.match(
    source,
    /data-codey-selected-next="true"[^}]*border-bottom: 0;/s,
  );
  assert.doesNotMatch(source, /outline-offset:\s*12px/);
});

test("selection changes resynchronize adjacent-turn grouping", () => {
  assert.match(source, /const syncSelectionGroups = \(\) => \{/);
  assert.match(source, /lastSelectedRow = row;\s*syncSelectionGroups\(\);/s);
  assert.match(source, /row\.dataset\.codeySelectedPrevious = "true"/);
  assert.match(source, /row\.dataset\.codeySelectedNext = "true"/);
});

test("hard-deleted messages are removed while Codex rebuilds its active history", () => {
  assert.doesNotMatch(source, /codey-deleted-turns/);
  assert.doesNotMatch(source, /data-codey-message-deleted/);
  assert.match(source, /dispatcher\("unsubscribe-thread-for-host"/);
  assert.match(source, /dispatcher\("maybe-resume-conversation"/);
  assert.match(source, /hardDeletedMessageKeys/);
  assert.match(source, /rows\.forEach\(\(row\) => row\.remove\(\)\)/);
  assert.doesNotMatch(source, /dispatcher\("discard-conversation-from-cache"/);
  assert.match(source, /已移除 \$\{locallyRemoved\} 条未写入会话的消息/);
  assert.match(source, /已永久删除 \$\{deleted\} 轮对话/);
});
