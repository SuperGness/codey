import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(
  new URL("../src/workflows/WorkflowConsole.tsx", import.meta.url),
  "utf8",
);
const appSource = readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8");
const runsSource = readFileSync(
  new URL("../src/workflows/useWorkflowRuns.ts", import.meta.url),
  "utf8",
);

test("workflow console only offers node retry for backend-retryable failures", () => {
  assert.match(
    source,
    /const retryable = node\.state === "failed" && controller\.capabilities\?\.actions\.retryNode;/,
  );
  assert.doesNotMatch(source, /node\.state === "canceled"\) && controller\.capabilities/);
});

test("embedded workflow console is session-scoped and hidden without a linked thread", () => {
  assert.match(
    appSource,
    /const workflowViewAvailable = !embedded \|\| Boolean\(workflowThreadId\);/,
  );
  assert.match(
    appSource,
    /<WorkflowConsole[\s\S]*initialRunId=\{workflowRunId\}[\s\S]*threadId=\{workflowThreadId\}/,
  );
  assert.match(
    runsSource,
    /api\.list\(\{ limit: RUN_PAGE_SIZE, threadId \}, \{ signal \}\)/,
  );
});
