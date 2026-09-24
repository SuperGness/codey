import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const python = ["python3", "python"].find(command => {
  const result = spawnSync(command, ["--version"], { encoding: "utf8" });
  return result.status === 0 && /Python 3\./.test(result.stdout);
});
const options = { skip: python ? false : "插件打包测试需要 Python 3" };
const script = fileURLToPath(new URL("../scripts/package-plugin.py", import.meta.url));
const digest = value => createHash("sha256").update(value).digest("hex");

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), "codey-plugin-package-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const library = join(dir, "demo.so"), config = join(dir, "settings.json"), output = join(dir, "demo.codey-plugin");
  // 打包测试只处理字节，不加载动态库。
  writeFileSync(library, "test library bytes");
  writeFileSync(config, '{\n  "value": "示例"\n}\n');
  return { dir, library, config, output, run: (extra = []) => spawnSync(python, [script,
    "--library", library, "--output", output,
    "--id", "test.demo", "--name", "打包测试", "--version", "1.0.0",
    "--platform", "linux", "--arch", "x86_64", ...extra,
  ], { encoding: "utf8" }) };
}

function archive(path) {
  const result = spawnSync(python, ["-c", `import base64,json,sys,zipfile
with zipfile.ZipFile(sys.argv[1]) as package:
    assert package.testzip() is None
    print(json.dumps({name:base64.b64encode(package.read(name)).decode() for name in package.namelist()}))`, path], { encoding: "utf8", maxBuffer: 2 * 1024 * 1024 });
  assert.equal(result.status, 0, result.stderr);
  return Object.fromEntries(Object.entries(JSON.parse(result.stdout)).map(([name, data]) => [name, Buffer.from(data, "base64")]));
}

test("plugin package preserves library, config text and manifest without overwriting output", options, t => {
  const f = fixture(t);
  writeFileSync(f.config, '{\n  "_comments": {"value":"示例值", "_comments":"{\\"_comments\\":null} // 代码示例", "":""},\n  "value" : "示例",\n  "rules": [{"_comments":{"state":"说明"}, "nested":[[{"_comments":{}}]]}],\n  "_comment": null, "__comments": false, "_commentsExtra": [1], "text": "_comments"\n}\n');
  assert.equal(f.run(["--capability", "request.lifecycle.v1", "--header", "X-Demo", "--config", f.config]).status, 0);
  const files = archive(f.output), manifest = JSON.parse(files["manifest.json"]);
  assert.deepEqual(Object.keys(files).sort(), ["config.json", "lib/demo.so", "manifest.json"]);
  assert.deepEqual(files["lib/demo.so"], readFileSync(f.library));
  assert.deepEqual(files["config.json"], readFileSync(f.config));
  assert.deepEqual(manifest, {
    id: "test.demo", name: "打包测试", version: "1.0.0", abiVersion: 1,
    platform: "linux", arch: "x86_64", entry: "lib/demo.so",
    librarySha256: digest(readFileSync(f.library)), capabilities: ["request.lifecycle.v1"],
    headerNames: ["X-Demo"], responseHeaderNames: [],
  });
  const saved = readFileSync(f.output);
  assert.notEqual(f.run().status, 0);
  assert.deepEqual(readFileSync(f.output), saved);
});

test("plugin package accepts a UTF-8 configuration object at the size limit", options, t => {
  const f = fixture(t);
  const config = Buffer.concat([Buffer.from('{"name":"配置"}'), Buffer.alloc(1024 * 1024 - Buffer.byteLength('{"name":"配置"}'), 32)]);
  writeFileSync(f.config, config);
  const result = f.run(["--config", f.config]);
  assert.equal(result.status, 0, result.stderr);
  const files = archive(f.output), manifest = JSON.parse(files["manifest.json"]);
  assert.deepEqual(files["config.json"], config);
  assert.equal(manifest.configUi, undefined);
  assert.equal(manifest.configSchema, undefined);
  assert.deepEqual(manifest.capabilities, []);
});

test("plugin package defaults to an empty configuration object", options, t => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  assert.equal(archive(f.output)["config.json"].toString(), "{}\n");
});

test("plugin package can declare a provider route without lifecycle permissions", options, t => {
  const f = fixture(t);
  const result = f.run(["--capability", "provider.route.v1"]);
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(archive(f.output)["manifest.json"]);
  assert.deepEqual(manifest.capabilities, ["provider.route.v1"]);
  assert.equal(manifest.responseHeaderNames, undefined);
});

test("plugin package declares lifecycle controls and permissions", options, t => {
  const f = fixture(t);
  const result = f.run(["--capability", "request.lifecycle.v1", "--capability", "request.lifecycle.auth",
    "--header", "x-example", "--response-header", "content-type", "--lifecycle-failure-policy", "abort",
    "--lifecycle-max-wait-ms", "45000"]);
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(archive(f.output)["manifest.json"]);
  assert.deepEqual(manifest.capabilities, ["request.lifecycle.v1", "request.lifecycle.auth"]);
  assert.deepEqual(manifest.headerNames, ["x-example"]);
  assert.deepEqual(manifest.responseHeaderNames, ["content-type"]);
  assert.equal(manifest.lifecycleFailurePolicy, "abort");
  assert.equal(manifest.lifecycleMaxWaitMs, 45000);
});

test("plugin package rejects inconsistent lifecycle permissions and limits", options, t => {
  const f = fixture(t);
  for (const args of [
    ["--capability", "request.beforeSend"],
    ["--header", "x-example"],
    ["--capability", "request.lifecycle.auth"],
    ["--response-header", "x-example"],
    ["--lifecycle-failure-policy", "continue"],
    ["--lifecycle-max-wait-ms", "1000"],
    ["--capability", "request.lifecycle.v1", "--lifecycle-max-wait-ms", "0"],
    ["--capability", "request.lifecycle.v1", "--lifecycle-max-wait-ms", "600001"],
    ["--capability", "request.lifecycle.v1", "--capability", "request.lifecycle.v1"],
    ["--capability", "request.lifecycle.v1", "--response-header", "X-Demo", "--response-header", "x-demo"],
  ]) {
    const result = f.run(args);
    assert.notEqual(result.status, 0, args.join(" "));
    assert.equal(existsSync(f.output), false);
  }
});

for (const [name, content] of [["oversized", Buffer.alloc(1024 * 1024 + 1)], ["invalid UTF-8", Buffer.from([0xff])], ["invalid JSON", "invalid"], ["array", "[]"], ["null", "null"], ["NaN", '{"value":NaN}'], ["infinity", '{"value":Infinity}'], ["JSONC", '{ // comment\n "x": 1 }']]) {
  test(`plugin package rejects ${name} configuration before writing output`, options, t => {
    const f = fixture(t);
    writeFileSync(f.config, content);
    assert.notEqual(f.run(["--config", f.config]).status, 0);
    assert.equal(existsSync(f.output), false);
  });
}

for (const invalid of [null, [], "private-value", 1, false]) {
  test(`plugin package rejects invalid comments object ${JSON.stringify(invalid)}`, options, t => {
    const f = fixture(t);
    writeFileSync(f.config, JSON.stringify({ rules: [{ _comments: invalid }] }));
    const result = f.run(["--config", f.config]);
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes('$["rules"][0]["_comments"]'), result.stderr);
    assert.match(result.stderr, /必须是对象/);
    assert.ok(!result.stderr.includes("private-value"));
    assert.equal(existsSync(f.output), false);
  });
}

for (const invalid of [null, [], { secret: "private-value" }, 1, false]) {
  test(`plugin package rejects invalid annotation value ${JSON.stringify(invalid)}`, options, t => {
    const f = fixture(t);
    writeFileSync(f.config, JSON.stringify({ rules: [{ _comments: { state: invalid } }] }));
    const result = f.run(["--config", f.config]);
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes('$["rules"][0]["_comments"]["state"]'), result.stderr);
    assert.match(result.stderr, /必须是字符串/);
    assert.ok(!result.stderr.includes("private-value"));
    assert.equal(existsSync(f.output), false);
  });
}

test("plugin package reports deeply nested parse errors without a traceback", options, t => {
  const f = fixture(t);
  // 缺少闭合符，确保各 Python 版本都会拒绝，不依赖 JSON 解析器的递归限制。
  writeFileSync(f.config, '{"rules":' + '['.repeat(15000) + '{}');
  const result = f.run(["--config", f.config]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /配置模板必须是有效的 UTF-8 JSON/);
  assert.doesNotMatch(result.stderr, /Traceback/);
  assert.equal(existsSync(f.output), false);
});

test("plugin package rejects configuration symlinks", options, t => {
  const f = fixture(t), link = join(f.dir, "link.json");
  try { symlinkSync(f.config, link, "file"); }
  catch (error) {
    if (process.platform === "win32" && ["EPERM", "EACCES"].includes(error.code)) return t.skip("当前 Windows 账号无创建符号链接权限");
    throw error;
  }
  assert.notEqual(f.run(["--config", link]).status, 0);
  assert.equal(existsSync(f.output), false);
});

test("plugin package rejects old configuration flags and invalid output extension before writing", options, t => {
  const f = fixture(t), invalidOutput = join(f.dir, "demo.zip");
  assert.notEqual(f.run(["--output", invalidOutput]).status, 0);
  assert.equal(existsSync(invalidOutput), false);
  for (const flag of ["--schema", "--config-ui"]) assert.notEqual(f.run([flag, f.config]).status, 0);
  assert.equal(existsSync(f.output), false);
});
