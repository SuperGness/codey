import test from "node:test";
import assert from "node:assert/strict";
import { grayTargetCount, isIdempotencyKey } from "../src/policy.ts";

test("灰度白名单目标使用全部白名单设备并正确截断比例和数量", () => {
  assert.equal(grayTargetCount(7, null, null, true), 7);
  assert.equal(grayTargetCount(7, 50, null, false), 4);
  assert.equal(grayTargetCount(7, null, 3, false), 3);
  assert.throws(() => grayTargetCount(7, 50, 3, false), RangeError);
});

test("发布幂等键限制长度和字符集", () => {
  assert.equal(isIdempotencyKey("publish-2026-09-25"), true);
  assert.equal(isIdempotencyKey("short"), false);
  assert.equal(isIdempotencyKey("bad key 123"), false);
});
