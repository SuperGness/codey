import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const quitSource = readFileSync(new URL("../backend/src/cdp/app_quit.rs", import.meta.url), "utf8");
const quitScript = quitSource.match(/const APP_QUIT_SCRIPT: &str = r#"([\s\S]*?)"#;/)[1];

test("desktop quit invokes the native app lifecycle once without requesting a relaunch", async () => {
  const messages = [];
  let acknowledged = false;
  const electronBridge = {
    async sendMessageFromView(message) {
      assert.equal(this, electronBridge);
      messages.push(JSON.parse(JSON.stringify(message)));
      await Promise.resolve();
      acknowledged = true;
    },
  };
  const result = await vm.runInNewContext(quitScript, { window: { electronBridge } });
  assert.equal(result, true);
  assert.equal(acknowledged, true);
  assert.deepEqual(messages, [{ type: "quit-app" }]);
});

test("desktop quit reports an unavailable native bridge without closing windows", async () => {
  for (const electronBridge of [undefined, {}, { sendMessageFromView: null }]) {
    const result = await vm.runInNewContext(quitScript, {
      window: { electronBridge, close() { assert.fail("window.close must not replace app quit"); } },
    });
    assert.equal(result, false);
  }
});

test("desktop quit propagates a native IPC rejection without retrying", async () => {
  let calls = 0;
  const result = vm.runInNewContext(quitScript, {
    window: {
      electronBridge: {
        async sendMessageFromView() {
          calls += 1;
          throw new Error("native quit unavailable");
        },
      },
    },
  });
  await assert.rejects(result, /native quit unavailable/);
  assert.equal(calls, 1);
});
