import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { analyzeNoteBatch, analyzeNotePatches, assertSameAssets, callback, collectNotePatches, generateNotes, identity, main, selectNoteCandidates, signature, updateReleaseNotes, validateBuild, validateNotes, validateRelease } from "../scripts/release-automation.mjs";
import { formatNoteResults } from "../scripts/release-note-batches.mjs";

const environment = {
  RELEASE_BUILD_ID: "build_test-123", RELEASE_ATTEMPT: "2", GITHUB_RUN_ID: "456", RELEASE_ACTION: "build",
  RELEASE_SOURCE_SHA: "a".repeat(40), RELEASE_VERSION: "1.2.3", RELEASE_BASE_SHA: "b".repeat(40), RELEASE_BASE_TAG: "v1.2.2",
  GITHUB_REPOSITORY: "owner/codey", CODEY_RELEASE_ADMIN_URL: "https://admin.example.com", RELEASE_ADMIN_CALLBACK_SECRET: "s".repeat(40),
};
const build = {
  id: environment.RELEASE_BUILD_ID, repository: environment.GITHUB_REPOSITORY, version: "1.2.3", tag: "v1.2.3",
  source_sha: environment.RELEASE_SOURCE_SHA, base_sha: environment.RELEASE_BASE_SHA, base_tag: "v1.2.2", action: "build", attempt: 2,
};

test("callback signs the exact body and includes immutable execution identity", async () => {
  let calls = 0;
  const result = await callback("events", { status: "succeeded", attempt: 99 }, environment, async (url, options) => {
    calls += 1;
    assert.equal(url, "https://admin.example.com/api/internal/builds/build_test-123/events");
    const body = JSON.parse(options.body);
    assert.deepEqual(body, { status: "succeeded", attempt: 2, run_id: 456, action: "build", source_sha: "a".repeat(40) });
    assert.equal(options.headers["x-release-signature"], createHmac("sha256", environment.RELEASE_ADMIN_CALLBACK_SECRET).update(`${options.headers["x-release-timestamp"]}.${options.body}`).digest("hex"));
    assert.equal(options.redirect, "error");
    return new Response('{"ok":true}');
  });
  assert.equal(calls, 1);
  assert.deepEqual(result, { ok: true });
  assert.equal(signature("123", "{}", "secret").length, 64);
});

test("invalid identity and insecure callback endpoints never reach the network", async () => {
  assert.throws(() => identity({ ...environment, RELEASE_SOURCE_SHA: "master" }), /身份参数/);
  assert.throws(() => identity({ ...environment, RELEASE_ATTEMPT: "1.5" }), /身份参数/);
  assert.throws(() => identity({ ...environment, RELEASE_ACTION: "publish" }), /身份参数/);
  const transport = () => assert.fail("must not reach network");
  await assert.rejects(callback("claim", {}, { ...environment, CODEY_RELEASE_ADMIN_URL: "http://example.com" }, transport), /HTTPS/);
  await assert.rejects(callback("claim", {}, { ...environment, RELEASE_ADMIN_CALLBACK_SECRET: "short" }, transport), /至少/);
});

test("callback rejects authorization and stale execution without automatic replay", async () => {
  let calls = 0;
  await assert.rejects(callback("claim", {}, environment, async () => {
    calls += 1;
    return new Response("stale attempt", { status: 409 });
  }), /409.*stale attempt/);
  assert.equal(calls, 1);
});

test("claim data must match repository, version, source and comparison range", () => {
  assert.equal(validateBuild(build, environment), build);
  for (const change of [{ repository: "attacker/repo" }, { source_sha: "c".repeat(40) }, { base_sha: "c".repeat(40) }, { version: "2.0.0" }, { tag: "v9.0.0" }, { action: "delete" }, { attempt: 3 }]) {
    assert.throws(() => validateBuild({ ...build, ...change }, environment), /不一致/);
  }
  assert.throws(() => validateBuild({ ...build, artifact_run_id: 123 }, environment), /产物来源/);
  assert.equal(validateBuild({ ...build, artifact_run_id: 123 }, { ...environment, RELEASE_ARTIFACT_RUN_ID: '123' }).artifact_run_id, 123);
  for (const version of ["01.2.3", "1.2.3-01", "1.2.3-rc..1", "1.2.3+build", "9007199254740992.2.3"]) assert.throws(() => validateBuild({ ...build, version, tag: `v${version}` }, { ...environment, RELEASE_VERSION: version }), /SemVer/);
});

test("release ownership rejects collisions and unexpected IDs", () => {
  const release = { id: 10, tag_name: "v1.2.3", body: "notes\n<!-- codey-build:build_test-123 -->" };
  assert.equal(validateRelease(release, build), release);
  assert.throws(() => validateRelease({ ...release, body: "other release" }, build), /占用/);
  assert.throws(() => validateRelease(release, { ...build, release_id: 11 }), /占用/);
});

test("retry cannot overwrite changed or missing installation assets", () => {
  const assets = [{ file_name: "installer.exe", sha256: "a".repeat(64), size: 42 }];
  assertSameAssets(assets, structuredClone(assets));
  assert.throws(() => assertSameAssets(assets, [{ ...assets[0], sha256: "b".repeat(64) }]), /产物/);
  assert.throws(() => assertSameAssets(assets, []), /产物/);
});

test("AI notes require file and literal code evidence from actual differences", () => {
  const diff = "+const retries = 3;\n-const retries = 1;";
  const patches = [{ file: 'retry.js', diff }];
  const valid = { notes: "- 调整失败重试次数", evidence: [{ note: "调整失败重试次数", file: "retry.js", excerpt: "+const retries = 3;" }] };
  assert.equal(validateNotes(valid, patches).notes_status, "generated");
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], file: "missing.js" }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], excerpt: "unrelated evidence" }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], note: '额外结论' }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, notes: valid.notes + '\n- 无证据的性能提升' }, patches), /每条日志/);
  assert.throws(() => validateNotes({ notes: "无证据的性能提升", evidence: [] }, patches), /证据/);
  assert.equal(validateNotes(valid, [{ file: "retry.js", diff: "+const first = true;" }, ...patches]).notes_status, "generated");
  assert.throws(() => validateNotes(valid, [{ file: "retry.js", diff: " context +const retries = 3;" }]), /无法核实/);
});

test("generated notes accept entries beyond the suggested count and length", () => {
  const evidence = { note: "用户可见变化", file: "app.js", excerpt: "+const changed = true;" };
  const patches = [{ file: evidence.file, diff: evidence.excerpt }];
  const many = Array.from({ length: 12 }, (_, index) => ({ ...evidence, note: `变化${index}：${"详细说明".repeat(30)}` }));
  const notes = many.map(item => `- ${item.note}`).join("\n");
  const result = validateNotes({ notes, evidence: many }, patches);
  assert.equal(result.notes_status, "generated");
  assert.equal(result.notes, notes);
  assert.deepEqual(result.evidence, many);
});

function largeNotePatches() {
  return Array.from({ length: 125 }, (_, index) => ({ file: `src/file-${index}.js`, diff: `@@ -1 +1,101 @@\n${Array.from({ length: 100 }, (_, line) => `+const value_${index}_${line} = ${line};\n`).join("")}` }));
}

function batchNote(batch, note) {
  const patch = batch.find(item => item.diff.split("\n").some(line => /^\+const/.test(line)));
  return { notes: `- ${note}`, evidence: [{ note, file: patch.file, excerpt: patch.diff.split("\n").find(line => /^\+const/.test(line)) }] };
}

test("all large-diff batches are analyzed before validated notes are merged", async () => {
  const patches = largeNotePatches();
  const seen = [];
  const result = await analyzeNotePatches(patches, build, async (batch, context, index, total) => {
    assert.equal(context.source_sha, build.source_sha);
    assert.equal(context.base_sha, build.base_sha);
    assert.ok(total > 1);
    seen.push(...batch);
    return batchNote(batch, `批次 ${index + 1} 的代码变更`);
  }, async candidates => {
    const evidence = candidates;
    return { notes: evidence.map(item => `- ${item.note}`).join("\n"), evidence };
  });
  assert.equal(result.notes_status, "generated");
  assert.ok(result.evidence.length > 1);
  assert.ok(result.evidence.length > 8);
  for (const patch of patches) assert.equal(seen.filter(item => item.file === patch.file).map(item => item.diff).join(""), patch.diff);
});

test("multi-batch results use one injected global selector with all candidates", async () => {
  const patches = ["alpha", "beta"].map(name => ({ file: `${name}.js`, diff: `+const ${name} = '${"x".repeat(30_000)}';\n` }));
  let candidates;
  const result = await analyzeNotePatches(patches, build, async batch => {
    const evidence = batch[0];
    const excerpt = evidence.diff.trimEnd();
    return { notes: `- 更新 ${evidence.file}`, evidence: [{ note: `更新 ${evidence.file}`, file: evidence.file, excerpt }] };
  }, async selected => {
    candidates = selected;
    const candidate = selected.at(-1);
    return { notes: `- ${candidate.note}`, evidence: [candidate] };
  });
  assert.equal(candidates.length, 2);
  assert.equal(result.evidence[0].file, "beta.js");
  assert.equal(result.notes, "**体验优化**\n- 更新 beta.js");
});

test("global AI selection retries invalid candidate references without accepting new text", async () => {
  const candidates = [{ note: "增加启动提示", file: "app.js", excerpt: "+const startupHint = true;" }];
  let calls = 0;
  const result = await selectNoteCandidates(candidates, build, async (command, args) => {
    calls += 1;
    assert.equal(command, "copilot");
    for (const flag of ["--available-tools", "--deny-tool", "--disable-builtin-mcps", "--no-custom-instructions", "--no-auto-update", "--no-ask-user"]) assert.ok(args.includes(flag));
    if (calls === 1) return { stdout: JSON.stringify({ selected: ["c99"] }) };
    assert.match(args[1], /上次精选结果未通过校验/);
    return { stdout: JSON.stringify({ selected: ["c0"] }) };
  });
  assert.equal(calls, 2);
  assert.deepEqual(result, { ...formatNoteResults(candidates), notes_status: "generated" });
});

test("global AI selection preserves valid candidates beyond the suggested size without retrying", async () => {
  const candidates = Array.from({ length: 9 }, (_, index) => ({
    note: `变化${index}：${"必要说明".repeat(20)}`, file: "app.js", excerpt: "+const startupHint = true;",
  }));
  let calls = 0;
  const result = await selectNoteCandidates(candidates, build, async () => {
    calls += 1;
    return { stdout: JSON.stringify({ selected: candidates.map((_, index) => `c${index}`) }) };
  });
  assert.equal(calls, 1);
  assert.deepEqual(result.evidence, candidates);
  assert.equal(result.notes, `**体验优化**\n${candidates.map(candidate => `- ${candidate.note}`).join("\n")}`);
});

test("batch evidence cannot cite changes from another batch", async () => {
  const patches = largeNotePatches();
  await assert.rejects(analyzeNotePatches(patches, build, async () => batchNote([patches.at(-1)], "引用其他批次")), /第 1\/.*无法核实/);
});

test("all patches are checked for secrets and binaries before the first AI request", async () => {
  for (const unsafe of [
    { file: ".env.production", diff: "+TOKEN=value\n" },
    { file: "src/secret.js", diff: `+const token = 'ghp_${"a".repeat(40)}';\n` },
    { file: "src/private.txt", diff: "+-----BEGIN PRIVATE KEY-----\n" },
    { file: "image.png", diff: "Binary files a/image.png and b/image.png differ\n" },
  ]) {
    await assert.rejects(analyzeNotePatches([...largeNotePatches(), unsafe], build, () => assert.fail("AI must not receive any batch before preflight")), /凭据|密钥|二进制/);
  }
});

test("quoted private key header fixtures pass without allowing actual or partial keys", async () => {
  const marker = ["-----BEGIN", "PRIVATE KEY-----"].join(" ");
  const fixture = { file: "tests/example.test.mjs", diff: `+{ diff: "+${marker}\\n" },\n` };
  let calls = 0;
  await analyzeNotePatches([fixture], build, async batch => {
    calls += 1;
    return { notes: "- 增加私钥检测测试", evidence: [{ note: "增加私钥检测测试", file: batch[0].file, excerpt: batch[0].diff.trimEnd() }] };
  });
  assert.equal(calls, 1);
  for (const prefix of ["+", "-", " "]) {
    for (const content of [
      `${marker}\n${prefix}${"A".repeat(48)}\n`,
      `const key = "${marker}\\n${"A".repeat(48)}";\n`,
      `const token = 'ghp_${"a".repeat(40)}';\n`,
    ]) {
      await assert.rejects(analyzeNotePatches([{ file: "src/credentials.js", diff: prefix + content }], build, () => assert.fail("sensitive diffs must not reach AI")), error => {
        assert.match(error.message, /src\/credentials\.js/);
        assert.doesNotMatch(error.message, /A{48}|a{40}/);
        return true;
      });
    }
  }
});

test("empty batch summaries are allowed but an entirely empty release requires manual notes", async () => {
  let calls = 0;
  const result = await analyzeNotePatches(largeNotePatches(), build, async batch => ++calls === 1 ? batchNote(batch, "确认代码变更") : { notes: "", evidence: [] }, async evidence => ({ notes: evidence.map(item => `- ${item.note}`).join("\n"), evidence }));
  assert.equal(result.evidence.length, 1);
  await assert.rejects(analyzeNotePatches([{ file: "internal.js", diff: "+const internal = true;\n" }], build, async () => ({ notes: "", evidence: [] })), /缺少有效日志/);
});

test("Copilot returns only notes and references while the script preserves exact CRLF and escaped evidence", async () => {
  const patches = [{ file: "src/路径.js", diff: '+const label = "escaped\\\\path";\r\n' }];
  let calls = 0;
  const result = await analyzeNoteBatch(patches, build, 0, 1, async (command, args, options) => {
    calls += 1;
    assert.equal(command, "copilot");
    for (const flag of ["--available-tools", "--deny-tool", "--disable-builtin-mcps", "--no-custom-instructions", "--no-auto-update", "--no-ask-user"]) assert.ok(args.includes(flag));
    assert.equal(options.timeout, 300_000);
    const ref = args[1].match(/\[evidence:(b\d+p\d+l\d+)\]/)[1];
    return { stdout: JSON.stringify({ entries: [{ note: "调整路径处理", ref }] }) };
  });
  assert.equal(calls, 1);
  assert.equal(result.notes, "**体验优化**\n- 调整路径处理");
  assert.equal(result.evidence[0].file, patches[0].file);
  assert.equal(result.evidence[0].excerpt, patches[0].diff.slice(0, -1));
});

test("invalid AI references receive one correction with sanitized diagnostics", async () => {
  const patches = [{ file: "retry.js", diff: "+const retries = 3;\n" }];
  let calls = 0;
  const result = await analyzeNoteBatch(patches, build, 0, 1, async (command, args) => {
    calls += 1;
    if (calls === 1) return { stdout: JSON.stringify({ entries: [{ note: "调整重试", ref: "sensitive-invented-reference" }] }) };
    assert.match(args[1], /上次输出未通过校验.*第 1 条.*不属于当前批次/);
    assert.doesNotMatch(args[1], /sensitive-invented-reference/);
    return { stdout: JSON.stringify({ entries: [{ note: "调整重试", ref: args[1].match(/\[evidence:(b\d+p\d+l\d+)\]/)[1] }] }) };
  });
  assert.equal(calls, 2);
  assert.equal(result.notes_status, "generated");
});

for (const [name, notes] of [
  ["longer entries", ["说明".repeat(60)]],
  ["more entries", Array.from({ length: 9 }, (_, index) => `变更${index}`)],
]) {
  test(`AI accepts ${name} without a length correction retry`, async () => {
    let calls = 0;
    const result = await analyzeNoteBatch([{ file: "retry.js", diff: "+const retries = 3;\n" }], build, 0, 1, async (_command, args) => {
      calls += 1;
      const ref = args[1].match(/\[evidence:(b\d+p\d+l\d+)\]/)[1];
      return { stdout: JSON.stringify({ entries: notes.map(note => ({ note, ref })) }) };
    });
    assert.equal(calls, 1);
    assert.equal(result.notes, `**体验优化**\n${notes.map(note => `- ${note}`).join("\n")}`);
  });
}

test("categorized AI entries produce concise section format with evidence in matching order", async () => {
  const patches = [{ file: "app.js", diff: "+const changed = true;\n" }];
  let calls = 0;
  const result = await analyzeNoteBatch(patches, build, 0, 1, async (_command, args) => {
    calls += 1;
    assert.match(args[1], /新增功能、体验优化、问题修复/);
    assert.match(args[1], /省去背景、过程和实现细节/);
    assert.match(args[1], /这些是篇幅建议/);
    const ref = args[1].match(/\[evidence:(b\d+p\d+l\d+)\]/)[1];
    return { stdout: JSON.stringify({ entries: [
      { category: "问题修复", note: "修复启动失败", ref },
      { category: "新增功能", note: "支持查看更新日志", ref },
      { category: "体验优化", note: "简化更新提示", ref },
    ] }) };
  });
  assert.equal(calls, 1);
  assert.equal(result.notes, "**新增功能**\n- 支持查看更新日志\n\n**体验优化**\n- 简化更新提示\n\n**问题修复**\n- 修复启动失败");
  assert.deepEqual(result.evidence.map(item => item.category), ["新增功能", "体验优化", "问题修复"]);
  assert.equal(result.notes_status, "generated");
  assert.throws(() => validateNotes({ ...result, evidence: result.evidence.map(item => ({ ...item, category: "问题修复" })) }, patches), /分类或证据格式不一致/);
});

test("global selection retains categories and omits empty sections after choosing candidates", async () => {
  const candidates = [
    { category: "新增功能", note: "支持快捷更新", file: "app.js", excerpt: "+const changed = true;" },
    { category: "问题修复", note: "修复启动失败", file: "app.js", excerpt: "+const changed = true;" },
    { category: "体验优化", note: "简化更新提示", file: "app.js", excerpt: "+const changed = true;" },
  ];
  const result = await selectNoteCandidates(candidates, build, async (_command, args) => {
    assert.match(args[1], /"category":"新增功能"/);
    return { stdout: JSON.stringify({ selected: ["c1", "c0"] }) };
  });
  assert.equal(result.notes, "**新增功能**\n- 支持快捷更新\n\n**问题修复**\n- 修复启动失败");
  assert.deepEqual(result.evidence, [candidates[0], candidates[1]]);
});

test("invalid JSON never appears in the correction prompt and repeated invalid references still fail", async () => {
  const patches = [{ file: "retry.js", diff: "+const retries = 3;\n" }];
  let calls = 0;
  await assert.rejects(analyzeNoteBatch(patches, build, 0, 1, async (command, args) => {
    calls += 1;
    if (calls === 1) return { stdout: "private-output-should-not-be-logged {" };
    assert.match(args[1], /不是有效 JSON/);
    assert.doesNotMatch(args[1], /private-output-should-not-be-logged/);
    return { stdout: JSON.stringify({ entries: [{ note: "错误引用", ref: "missing" }] }) };
  }), /第 1 条.*不属于当前批次/);
  assert.equal(calls, 2);
});

test("Copilot command failures are not replayed as output corrections", async () => {
  let calls = 0;
  await assert.rejects(analyzeNoteBatch([{ file: "retry.js", diff: "+const retries = 3;\n" }], build, 0, 1, async () => {
    calls += 1;
    throw Object.assign(new Error("network failure"), { cmd: "copilot --prompt" });
  }), /network failure/);
  assert.equal(calls, 1);
});

for (const [lineEndingName, lineEnding] of [["LF", "\n"], ["CRLF", "\r\n"]]) {
  test(`managed workflows isolate AI permissions and wait for every build gate before failure reporting (${lineEndingName})`, async () => {
    const workflow = (await readFile(new URL("../.github/workflows/build-desktop.yml", import.meta.url), "utf8")).replace(/\r?\n/g, lineEnding);
    const notes = workflow.slice(workflow.indexOf("\n  notes:"), workflow.indexOf("\n  macos:"));
    assert.match(notes, /contents: read\s+copilot-requests: write/);
    assert.doesNotMatch(notes, /RELEASE_ADMIN_CALLBACK_SECRET|CLOUDFLARE_API_TOKEN|contents: write/);
    assert.match(notes, /persist-credentials: false/);
    const managedPublish = workflow.slice(workflow.indexOf("\n  managed-publish:"), workflow.indexOf("\n  report-failure:"));
    for (const gate of ["prepare", "notes", "macos", "macos-check", "windows", "windows-check"]) assert.match(managedPublish, new RegExp(`- ${gate}\\r?\\n`));
    const failure = workflow.slice(workflow.indexOf("\n  report-failure:"));
    assert.match(failure, /needs.prepare.outputs.claimed == 'true'/);
    assert.match(failure, /needs.managed-publish.result != 'success'/);
    for (const gate of ["prepare", "notes", "macos", "macos-check", "windows", "windows-check", "managed-publish"]) assert.match(failure, new RegExp(`- ${gate}\\r?\\n`));
  });
}

test("managed platform builds inject claimed versions before Rust caches without changing legacy or artifact-reuse paths", async () => {
  const workflow = await readFile(new URL("../.github/workflows/build-desktop.yml", import.meta.url), "utf8");
  for (const [job, nextJob] of [["macos", "macos-check"], ["windows", "windows-check"]]) {
    const steps = workflow.slice(workflow.indexOf(`\n  ${job}:`), workflow.indexOf(`\n  ${nextJob}:`));
    assert.match(steps, /if: inputs.artifact_run_id == ''/);
    assert.match(steps, /ref: \$\{\{ inputs.source_sha \|\| github.ref \}\}/);
    assert.match(steps, /name: Download claimed platform build\s+if: inputs.build_id != ''\s+uses: actions\/download-artifact@v4\s+with:\s+name: release-build-context/);
    assert.match(steps, /name: Inject validated platform release version\s+if: inputs.build_id != ''\s+run: node scripts\/release-automation.mjs prepare-version/);
    assert.ok(steps.indexOf("prepare-version") < steps.indexOf("name: Cache Rust build"));
    assert.doesNotMatch(steps, /release\.mjs|generate-lockfile|git push|git commit/);
  }
  const notes = workflow.slice(workflow.indexOf("\n  notes:"), workflow.indexOf("\n  macos:"));
  assert.doesNotMatch(notes, /prepare-version/);
  const legacy = workflow.slice(workflow.indexOf("\n  publish:"), workflow.indexOf("\n  managed-publish:"));
  assert.match(legacy, /startsWith\(github.ref, 'refs\/tags\/v'\) && inputs.build_id == ''/);
  assert.doesNotMatch(legacy, /prepare-version/);
});

test("delete maintenance uses the same lock and has no R2 credentials", async () => {
  const workflow = await readFile(new URL("../.github/workflows/release-maintenance.yml", import.meta.url), "utf8");
  assert.match(workflow, /group: codey-release-automation\s+cancel-in-progress: false/);
  assert.match(workflow, /run-name: "Codey .*#\$\{\{ inputs.attempt \}\}"/);
  const deletion = workflow.slice(workflow.indexOf("\n  delete:"), workflow.indexOf("\n  report-failure:"));
  assert.doesNotMatch(deletion, /CLOUDFLARE|copilot/);
  assert.match(deletion, /release-automation.mjs delete/);
});

test("notes retry uses fixed workflow tools while retaining the original source and baseline", async () => {
  for (const name of ["build-desktop.yml", "release-maintenance.yml"]) {
    const workflow = await readFile(new URL(`../.github/workflows/${name}`, import.meta.url), "utf8");
    const notes = workflow.slice(workflow.indexOf("\n  notes:"), workflow.indexOf(name === "build-desktop.yml" ? "\n  macos:" : "\n  sync:"));
    assert.match(notes, /ref: \$\{\{ inputs.source_sha \}\}\s+fetch-depth: 0\s+persist-credentials: false/);
    assert.match(notes, /ref: \$\{\{ github.sha \}\}\s+path: .release-tools\s+persist-credentials: false/);
    assert.match(notes, /run: node \.release-tools\/scripts\/release-automation.mjs notes/);
    assert.doesNotMatch(notes, /working-directory|RELEASE_ADMIN_CALLBACK_SECRET|CLOUDFLARE_API_TOKEN|contents: write/);
    assert.match(workflow, /RELEASE_BASE_SHA: \$\{\{ inputs.base_sha \}\}/);
    assert.match(workflow, /RELEASE_SOURCE_SHA: \$\{\{ inputs.source_sha \}\}/);
    if (name === "release-maintenance.yml") {
      const sync = workflow.slice(workflow.indexOf("\n  sync:"), workflow.indexOf("\n  delete:"));
      assert.match(sync, /ref: \$\{\{ github.sha \}\}/);
    }
  }
});

async function sandboxBuild(action, handler) {
  const directory = await mkdtemp(join(tmpdir(), "codey-release-automation-"));
  const oldCwd = process.cwd();
  const oldEnv = { ...process.env };
  const oldFetch = globalThis.fetch;
  try {
    Object.assign(process.env, environment, { RELEASE_ACTION: action, GH_TOKEN: "test-token" });
    process.chdir(directory);
    await writeFile(".release-build.json", JSON.stringify({ ...build, action, release_id: 10 }));
    await handler(directory);
  } finally {
    globalThis.fetch = oldFetch;
    process.chdir(oldCwd);
    for (const key of Object.keys(process.env)) if (!(key in oldEnv)) delete process.env[key];
    Object.assign(process.env, oldEnv);
    await rm(directory, { recursive: true, force: true });
  }
}

test("updating draft notes preserves the tag, commit and ownership and rejects a changed response", async () => {
  await sandboxBuild("build", async () => {
    const release = { id: 10, tag_name: build.tag, target_commitish: build.source_sha, draft: true, body: `<!-- codey-build:${build.id} -->` };
    let changed = false;
    globalThis.fetch = async (url, options) => {
      assert.equal(url, "https://api.github.com/repos/owner/codey/releases/10");
      assert.equal(options.method, "PATCH");
      const body = JSON.parse(options.body);
      assert.deepEqual(Object.keys(body).sort(), ["body", "tag_name"]);
      assert.ok(!("target_commitish" in body));
      return Response.json({ ...release, ...body, tag_name: changed ? "untagged-test" : body.tag_name || "untagged-test" });
    };
    const updated = await updateReleaseNotes(release, build, "- 修复发布流程");
    assert.equal(updated.tag_name, build.tag);
    assert.equal(updated.target_commitish, build.source_sha);
    assert.equal(updated.draft, true);
    assert.match(updated.body, /修复发布流程/);
    changed = true;
    await assert.rejects(updateReleaseNotes(release, build, "- 修复发布流程"), /占用/);
  });
});

test("GitHub release errors retain status, request ID and the server denial reason", async () => {
  await sandboxBuild("notes", async () => {
    const release = { id: 10, tag_name: build.tag, draft: true, body: `<!-- codey-build:${build.id} -->` };
    let calls = 0;
    globalThis.fetch = async (url, options) => {
      calls += 1;
      assert.equal(options.method, "PATCH");
      return Response.json({ message: "Resource not accessible by integration", documentation_url: "https://docs.github.com/rest/releases/releases" }, { status: 403, headers: { "x-github-request-id": "E000:16483:3B520E:40969C:6AC5877E" } });
    };
    await assert.rejects(updateReleaseNotes(release, build, "- 修复发布流程"), error => {
      assert.match(error.message, /403.*E000:16483:3B520E:40969C:6AC5877E/);
      assert.match(error.message, /Resource not accessible by integration/);
      return true;
    });
    assert.equal(calls, 1);
  });
});

test("GitHub error diagnostics redact credentials, control characters and non-message fields", async () => {
  await sandboxBuild("notes", async () => {
    const release = { id: 10, tag_name: build.tag, draft: true, body: `<!-- codey-build:${build.id} -->` };
    process.env.GH_TOKEN = "private-test-credential";
    const token = `ghp_${"a".repeat(40)}`;
    globalThis.fetch = async () => Response.json({
      message: `\u001b[31mDenied\u0000\n${process.env.GH_TOKEN} ${token} Bearer arbitrary-private-value ${environment.RELEASE_ADMIN_CALLBACK_SECRET} ${"x".repeat(1000)} message-tail`,
      request: { body: "private-source-body", authorization: "private-authorization" },
    }, { status: 403 });
    await assert.rejects(updateReleaseNotes(release, build, "- 修复发布流程"), error => {
      assert.match(error.message, /Denied.*\[redacted\]/);
      assert.doesNotMatch(error.message, /private-test-credential|ghp_|arbitrary-private-value|private-source-body|private-authorization|message-tail|\u0000|\u001b|\n/);
      assert.ok(!error.message.includes(environment.RELEASE_ADMIN_CALLBACK_SECRET));
      assert.ok(error.message.length < 600);
      return true;
    });
  });
});

test("non-JSON GitHub error bodies are not logged and never hide the original HTTP failure", async () => {
  await sandboxBuild("notes", async () => {
    const release = { id: 10, tag_name: build.tag, draft: true, body: `<!-- codey-build:${build.id} -->` };
    globalThis.fetch = async () => new Response("<html>private-upstream-body</html>", { status: 502 });
    await assert.rejects(updateReleaseNotes(release, build, "- 修复发布流程"), error => {
      assert.match(error.message, /GitHub PATCH releases\/10 失败（502/);
      assert.doesNotMatch(error.message, /private-upstream-body|JSON|html/);
      return true;
    });
  });
});

test("claim accepts a platform-selected version different from valid consistent source manifests", async () => {
  for (const action of ["build", "notes", "delete"]) await sandboxBuild(action, async () => {
    await writeFile("package.json", '{"version":"1.2.2"}');
    await writeFile("Cargo.toml", '[workspace.package]\nversion = "1.2.2"\n');
    globalThis.fetch = async () => Response.json({ ...build, action });
    await main("claim");
    assert.equal(JSON.parse(await readFile(".release-build.json", "utf8")).version, "1.2.3");
    assert.equal(JSON.parse(await readFile("package.json", "utf8")).version, "1.2.2");
  });
});

test("claim rejects inconsistent or invalid source versions without writing a claimed context", async () => {
  await sandboxBuild("build", async () => {
    await rm(".release-build.json");
    await writeFile("package.json", '{"version":"1.2.2"}');
    await writeFile("Cargo.toml", '[workspace.package]\nversion = "1.2.1"\n');
    globalThis.fetch = async () => Response.json(build);
    await assert.rejects(main("claim"), /必须一致/);
    await assert.rejects(readFile(".release-build.json"), { code: "ENOENT" });
    await writeFile("package.json", '{"version":"01.2.2"}');
    await assert.rejects(main("claim"), /SemVer/);
    await assert.rejects(readFile(".release-build.json"), { code: "ENOENT" });
  });
});

test("prepare-version rejects maintenance and artifact reuse before changing manifests", async () => {
  for (const action of ["notes", "delete"]) await sandboxBuild(action, async () => {
    await assert.rejects(main("prepare-version"), /完整打包任务/);
  });
  await sandboxBuild("build", async () => {
    process.env.RELEASE_ARTIFACT_RUN_ID = "123";
    await writeFile(".release-build.json", JSON.stringify({ ...build, artifact_run_id: 123 }));
    await assert.rejects(main("prepare-version"), /完整打包任务/);
  });
});

test("delete cleans the owned Release before tag and asks backend to clean R2 last", async () => {
  await sandboxBuild("delete", async () => {
    let hasTag = true;
    let hasRelease = true;
    const mutations = [];
    globalThis.fetch = async (url, options = {}) => {
      const path = new URL(url).pathname;
      const method = options.method || "GET";
      if (path.endsWith("/events")) {
        const body = JSON.parse(options.body);
        assert.equal(body.action, "delete");
        assert.equal(body.source_sha, build.source_sha);
        assert.equal(options.headers["x-release-signature"], signature(options.headers["x-release-timestamp"], options.body, environment.RELEASE_ADMIN_CALLBACK_SECRET));
        mutations.push(body.status);
        return new Response('{"ok":true}');
      }
      if (method === "DELETE") {
        mutations.push(path);
        if (path.endsWith("/releases/10")) hasRelease = false;
        else if (path.endsWith("/git/refs/tags/v1.2.3")) hasTag = false;
        else assert.fail(`unexpected deletion ${path}`);
        return new Response(null, { status: 204 });
      }
      if (path.endsWith("/git/ref/tags/v1.2.3")) return hasTag ? Response.json({ object: { type: "tag", sha: "tag-sha" } }) : new Response(null, { status: 404 });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: build.source_sha }, message: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/releases/tags/v1.2.3")) return new Response(null, { status: 404 });
      if (path === "/repos/owner/codey/releases") return Response.json(hasRelease ? [{ id: 10, draft: true, tag_name: build.tag, body: `<!-- codey-build:${build.id} -->` }] : []);
      if (path === "/repos/owner/codey") return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await main("delete");
    assert.deepEqual(mutations, ["/repos/owner/codey/releases/10", "/repos/owner/codey/git/refs/tags/v1.2.3", "github_deleted", "clean_r2"]);
    mutations.length = 0;
    await main("delete");
    assert.deepEqual(mutations, ["github_deleted", "clean_r2"]);
  });
});

test("delete refuses a tag pointing at a different commit before any mutation", async () => {
  await sandboxBuild("delete", async () => {
    globalThis.fetch = async (url, options = {}) => {
      assert.notEqual(options.method, "DELETE");
      const path = new URL(url).pathname;
      if (path.endsWith("/git/ref/tags/v1.2.3")) return Response.json({ object: { type: "tag", sha: "tag-sha" } });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: "c".repeat(40) }, message: `<!-- codey-build:${build.id} -->` });
      if (path === "/repos/owner/codey") return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await assert.rejects(main("delete"), /归属或提交不一致/);
  });
});

test("delete leaves remaining resources visible when GitHub deletion fails", async () => {
  await sandboxBuild("delete", async () => {
    const calls = [];
    globalThis.fetch = async (url, options = {}) => {
      calls.push([url, options.method]);
      const path = new URL(url).pathname;
      assert.ok(!path.endsWith("/events"));
      if (options.method === "DELETE") return new Response("permission denied", { status: 403 });
      if (path.endsWith("/git/ref/tags/v1.2.3")) return Response.json({ object: { type: "tag", sha: "tag-sha" } });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: build.source_sha }, message: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/releases/tags/v1.2.3")) return Response.json({ id: 10, tag_name: build.tag, body: `<!-- codey-build:${build.id} -->` });
      if (path === "/repos/owner/codey") return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await assert.rejects(main("delete"), /403/);
    assert.equal(calls.filter(([, method]) => method === "DELETE").length, 1);
    assert.ok(!calls.some(([url]) => url.includes("/git/refs/")));
  });
});

test("first release without baseline preserves a manual-notes result without using AI", async () => {
  await sandboxBuild("build", async () => {
    process.env.RELEASE_BASE_SHA = "";
    process.env.RELEASE_BASE_TAG = "";
    await writeFile(".release-build.json", JSON.stringify({ ...build, base_sha: "", base_tag: "" }));
    globalThis.fetch = () => assert.fail("no remote call for missing baseline");
    await main("notes");
    const result = JSON.parse(await readFile("release-notes.json", "utf8"));
    assert.equal(result.notes_status, "manual_required");
    assert.match(result.reason, /首次发布/);
    assert.equal(result.notes, "");
  });
});

test("a failed batch discards every partial note and masks command prompts", async () => {
  await sandboxBuild("notes", async () => {
    let calls = 0;
    await generateNotes({ collect: async () => largeNotePatches(), analyze: async batch => {
      if (++calls === 2) throw Object.assign(new Error("prompt with source and credentials"), { cmd: "copilot --prompt sensitive" });
      return batchNote(batch, "不应保存的部分日志");
    } });
    const result = JSON.parse(await readFile("release-notes.json", "utf8"));
    assert.equal(calls, 2);
    assert.equal(result.notes_status, "manual_required");
    assert.equal(result.notes, "");
    assert.deepEqual(result.evidence, []);
    assert.match(result.reason, /第 2\/.*AI 调用失败/);
    assert.doesNotMatch(result.reason, /sensitive|credentials|prompt with/);
  });
});

test("reference correction exhaustion still discards previously successful batch notes", async () => {
  await sandboxBuild("notes", async () => {
    let calls = 0;
    await generateNotes({ collect: async () => largeNotePatches(), analyze: (batch, context, index, total) => analyzeNoteBatch(batch, context, index, total, async (command, args) => {
      calls += 1;
      const ref = index === 0 ? args[1].match(/\[evidence:(b\d+p\d+l\d+)\]/)[1] : "invented-reference";
      return { stdout: JSON.stringify({ entries: [{ note: "不应保留的部分日志", ref }] }) };
    }) });
    const result = JSON.parse(await readFile("release-notes.json", "utf8"));
    assert.equal(calls, 3);
    assert.equal(result.notes_status, "manual_required");
    assert.equal(result.notes, "");
    assert.deepEqual(result.evidence, []);
    assert.match(result.reason, /第 2\/.*第 1 条.*不属于当前批次/);
    assert.doesNotMatch(result.reason, /invented-reference/);
  });
});

test("Git diff collection preserves a file above 2 MB and treats special paths literally", async () => {
  await sandboxBuild("notes", async () => {
    const execute = promisify(execFile);
    const git = args => execute("git", args);
    await git(["init", "--quiet"]);
    const file = process.platform === "win32" ? "literal[测试].txt" : ":literal[测试].txt";
    await writeFile(file, "old\n");
    await git(["add", "."]);
    const commit = () => git(["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "--quiet", "-m", "test"]);
    await commit();
    const base = (await git(["rev-parse", "HEAD"])).stdout.trim();
    const contents = `${"const largeLine = '中文内容'; ".repeat(12)}\n`.repeat(10000);
    assert.ok(Buffer.byteLength(contents) > 2 * 1024 * 1024);
    await writeFile(file, contents);
    await git(["add", "."]);
    await commit();
    const source = (await git(["rev-parse", "HEAD"])).stdout.trim();
    const patches = await collectNotePatches({ base_sha: base, source_sha: source });
    assert.equal(patches.length, 1);
    assert.equal(patches[0].file, file);
    assert.ok(Buffer.byteLength(patches[0].diff) > 2 * 1024 * 1024);
    assert.equal(patches[0].diff.split("\n").filter(line => line.startsWith("+const largeLine")).length, 10000);
  });
});

test("managed publication checks the canonical repository URL before reading release notes", async () => {
  await sandboxBuild("build", async () => {
    Object.assign(process.env, {
      CLOUDFLARE_R2_BUCKET: "codey-updates", CLOUDFLARE_R2_PUBLIC_BASE_URL: "https://r2.example.com",
      CLOUDFLARE_ACCOUNT_ID: "test-account", CLOUDFLARE_API_TOKEN: "test-token",
    });
    const requests = [];
    globalThis.fetch = async (url, options) => {
      requests.push(url);
      assert.equal(options.method, "GET");
      assert.equal(options.headers.authorization, "Bearer test-token");
      return url === "https://api.github.com/repos/owner/codey" ? Response.json({ id: 1 }) : new Response(null, { status: 404 });
    };
    await assert.rejects(main("publish"), error => error.code === "ENOENT" && typeof error.path === "string" && resolve(error.path) === resolve("release-notes.json"));
    assert.deepEqual(requests, ["https://api.github.com/repos/owner/codey"]);
  });
});

test("managed publication fails immediately if R2 configuration is incomplete", async () => {
  await sandboxBuild("build", async () => {
    delete process.env.CLOUDFLARE_R2_BUCKET;
    globalThis.fetch = () => assert.fail("missing R2 must fail before remote writes");
    await assert.rejects(main("publish"), /缺少 CLOUDFLARE_R2_BUCKET/);
  });
});
