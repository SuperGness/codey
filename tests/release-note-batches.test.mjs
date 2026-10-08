import assert from "node:assert/strict";
import test from "node:test";
import { chunkNotePatches, chunkPatches, createNoteBatchInput, mergeNoteResults, resolveNoteEntries } from "../scripts/release-note-batches.mjs";
import { validateNotes } from "../scripts/release-automation.mjs";

function byteLength(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function assertReassembled(patches, batches) {
  const chunks = batches.flat();
  let offset = 0;
  for (const patch of patches) {
    let diff = "";
    let consumed = false;
    while (offset < chunks.length && chunks[offset].file === patch.file && (!consumed || diff.length < patch.diff.length)) {
      diff += chunks[offset++].diff;
      consumed = true;
    }
    assert.equal(diff, patch.diff);
  }
  assert.equal(offset, chunks.length);
}

test("splits large patches without changing file paths or diff contents", () => {
  const patches = [
    { file: "src/alpha.js", diff: "diff --git a/src/alpha.js b/src/alpha.js\r\n@@ -1,2 +1,5 @@\r\n-old\r\n+new\r\n+增加内容\r\n" },
    { file: "src/beta.js", diff: "@@ -4,1 +4,3 @@\n+beta-one\n+beta-two\n" },
  ];
  const batches = chunkPatches(patches, 90);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 90);
  assertReassembled(patches, batches);
});

test("supports Unicode, CRLF, more than one hundred files, and total diffs above 80 KB", () => {
  const patches = Array.from({ length: 125 }, (_, index) => ({ file: `src/文件-${index}.ts`, diff: `@@ -1,1 +1,41 @@\r\n${`+第 ${index} 行，包含中文内容和转义字符 \\\"\r\n`.repeat(40)}` }));
  const batches = chunkPatches(patches, 512);
  assert.ok(patches.length > 100);
  assert.ok(Buffer.byteLength(patches.map(patch => patch.diff).join("")) > 80_000);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 512);
  assertReassembled(patches, batches);
});

test("splits one large file at complete diff lines", () => {
  const patch = { file: "large.txt", diff: Array.from({ length: 5000 }, (_, index) => `+line-${index}\n`).join("") };
  const batches = chunkPatches([patch], 2048);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 2048);
  assertReassembled([patch], batches);
});

test("rejects a line that cannot fit in a single serialized batch", () => {
  const patch = { file: "secrets.txt", diff: `+${"超长".repeat(100)}\n` };
  assert.throws(() => chunkPatches([patch], 64), /单行差异与文件路径序列化后超过单批大小限制/);
});

test("empty diffs remain representable and empty input returns no batches", () => {
  assert.deepEqual(chunkPatches([]), []);
  const patches = [{ file: "empty.txt", diff: "" }];
  const batches = chunkPatches(patches, 128);
  assertReassembled(patches, batches);
  assert.equal(byteLength(batches[0]), byteLength([{ file: "empty.txt", diff: "" }]));
});

test("deduplicates exact note text while preserving its evidence", () => {
  const firstEvidence = { note: "修复启动流程", file: "src/start.js", excerpt: "+start()" };
  const duplicateEvidence = { note: "修复启动流程", file: "src/other.js", excerpt: "+same" };
  const secondEvidence = { note: "增加更新检查", file: "src/update.js", excerpt: "+check()" };
  assert.deepEqual(mergeNoteResults([
    { notes: "- 修复启动流程\n- 增加更新检查", evidence: [firstEvidence, secondEvidence] },
    { notes: "- 修复启动流程", evidence: [duplicateEvidence] },
  ]), { notes: "**体验优化**\n- 修复启动流程\n- 增加更新检查", evidence: [firstEvidence, secondEvidence] });
});

test("ignores empty batch results", () => {
  assert.deepEqual(mergeNoteResults([{ notes: "", evidence: [] }]), { notes: "", evidence: [] });
  for (const result of [null, { notes: "", evidence: [{}] }, { notes: "- 无证据结论", evidence: [] }]) assert.throws(() => mergeNoteResults([result]));
});

test("merging grouped batches preserves category order, deduplication and original evidence", () => {
  const fix = { category: "问题修复", note: "修复启动失败", file: "start.js", excerpt: "+const fixed = true;" };
  const feature = { category: "新增功能", note: "支持查看日志", file: "update.js", excerpt: "+const details = true;" };
  const legacy = { note: "简化更新提示", file: "update.js", excerpt: "+const compact = true;" };
  const result = mergeNoteResults([
    { notes: "**问题修复**\n- 修复启动失败", evidence: [fix] },
    { notes: "**新增功能**\n- 支持查看日志", evidence: [feature] },
    { notes: "- 简化更新提示\n- 修复启动失败", evidence: [legacy, fix] },
  ]);
  assert.equal(result.notes, "**新增功能**\n- 支持查看日志\n\n**体验优化**\n- 简化更新提示\n\n**问题修复**\n- 修复启动失败");
  assert.deepEqual(result.evidence, [feature, legacy, fix]);
  assert.throws(() => mergeNoteResults([{ notes: "**新增功能**\n- 修复启动失败", evidence: [fix] }]), /证据不一致/);
  assert.throws(() => mergeNoteResults([{ notes: "**问题修复**", evidence: [] }]), /分类必须包含内容/);
});

test("unknown categories are rejected while missing categories remain compatible", () => {
  const input = createNoteBatchInput([{ file: "app.js", diff: "+const changed = true;\n" }]);
  const ref = [...input.references.keys()][0];
  assert.throws(() => resolveNoteEntries({ entries: [{ category: "其他", note: "调整提示", ref }] }, input.references), /日志分类必须/);
  const result = resolveNoteEntries({ entries: [{ note: "调整提示", ref }] }, input.references);
  assert.equal(result.notes, "**体验优化**\n- 调整提示");
  assert.equal(result.evidence[0].category, "体验优化");
});

test("reference entries restore exact original evidence without AI copying paths or code", () => {
  const patches = [{ file: "src/文件.js", diff: 'diff --git a/src/文件.js b/src/文件.js\r\n--- a/src/文件.js\r\n+++ b/src/文件.js\r\n@@ -1 +1,2 @@\r\n context +not-a-change\r\n+const label = "转义\\\\路径";\r\n+}\r\n' }];
  const input = createNoteBatchInput(patches, 1);
  assert.equal(input.references.size, 2);
  assert.equal(input.patches[0].diff.replace(/^\[evidence:b\d+p\d+l\d+\] /gm, ""), patches[0].diff);
  const entries = [...input.references.keys()].map((ref, index) => ({ ref, note: `变更 ${index + 1}`, file: "fabricated.js", excerpt: "+fabricated evidence" }));
  const result = resolveNoteEntries({ entries }, input.references);
  assert.equal(validateNotes(result, patches).notes_status, "generated");
  for (const evidence of result.evidence) {
    assert.equal(evidence.file, patches[0].file);
    assert.ok(patches[0].diff.includes(evidence.excerpt));
    assert.doesNotMatch(evidence.excerpt, /fabricated/);
  }
  assert.deepEqual(resolveNoteEntries({ entries: [] }, input.references), { notes: "", evidence: [] });
});

test("unknown references, context lines, headers and references from another batch are rejected", () => {
  const patches = [{ file: "same.js", diff: "--- a/same.js\n+++ b/same.js\n context +not-a-change\n+const real = true;\n" }];
  const first = createNoteBatchInput(patches, 0);
  const second = createNoteBatchInput(patches, 1);
  assert.equal(first.references.size, 1);
  for (const ref of ["fabricated", [...second.references.keys()][0], "b0000000001p0l0", "b0000000001p0l2"]) {
    assert.throws(() => resolveNoteEntries({ entries: [{ note: "拒绝伪造证据", ref }] }, first.references), /第 1 条.*不属于当前批次/);
  }
  assert.throws(() => resolveNoteEntries({ entries: [{ note: "多行\n注入", ref: [...first.references.keys()][0] }] }, first.references), /文字无效/);
});

test("annotation overhead is included in the batch byte budget and all raw differences remain intact", () => {
  const patches = [{ file: "large.js", diff: Array.from({ length: 100 }, (_, index) => `+const 中文_${index} = "\\\\路径";\r\n`).join("") }];
  const batches = chunkNotePatches(patches, 512);
  assert.ok(batches.length > 1);
  const refs = new Set();
  for (const [index, batch] of batches.entries()) {
    const input = createNoteBatchInput(batch, index);
    assert.ok(byteLength(input.patches) <= 512);
    for (const ref of input.references.keys()) {
      assert.ok(!refs.has(ref));
      refs.add(ref);
    }
  }
  assert.equal(refs.size, 100);
  assertReassembled(patches, batches);
  assert.throws(() => chunkNotePatches([{ file: "long.js", diff: `+${"a".repeat(100)}\n` }], 128), /单批大小限制/);
});
