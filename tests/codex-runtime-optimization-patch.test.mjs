import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdtemp, mkdir, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";
import vm from "node:vm";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";
// Desktop's shared transport applies its own transform before serialization.
const appServerTransportFixture = `globalThis.Transport=class {
  constructor(options){this.options=options}
  sendMessage(e){let n=this.options.getConnection(),a=this.options.transformOutgoingMessage==null?e:this.options.transformOutgoingMessage(e);n.send(JSON.stringify(a))}
};`;

async function loadPatchInIsolatedContext(runtimeConfigOverrides, contextOverrides = {}, installMessagePatch = true) {
  const Module = process.getBuiltinModule("module");
  const originalLoad = Module._load;
  const originalJsExtension = Module._extensions[".js"];
  const workerThreads = process.getBuiltinModule("worker_threads");
  const NativeWorker = workerThreads.Worker;
  const childProcess = process.getBuiltinModule("child_process");
  const originalSpawn = childProcess.spawn;
  const spawnCalls = [];
  childProcess.spawn = (...args) => {
    spawnCalls.push(args);
    return { pid: 4242 };
  };
  const context = {
    clearTimeout,
    console,
    process: { ...process, env: {
      ...process.env,
      CODEY_DISABLE_MACOS_CHILD_PROCESS_SAMPLER: "false",
    } },
    Promise,
    setImmediate,
    setTimeout,
    ...contextOverrides,
  };
  context.globalThis = context;
  const restore = () => {
    childProcess.spawn = originalSpawn;
    Module._load = originalLoad;
    Module._extensions[".js"] = originalJsExtension;
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
  };
  try {
    const result = vm.runInNewContext(
      await loadStartupPatchTemplate({
        runtimeConfigOverrides,
        requireAppServerRuntimeOverrides: true,
      }),
      context,
    );
    if (installMessagePatch) {
      context.__CODEY_PATCH_CODEX_APP_SERVER_MESSAGES__(appServerTransportFixture);
    }
    return {
      context,
      result,
      restore,
      spawnCalls,
    };
  } catch (error) {
    restore();
    throw error;
  }
}

test("shared app-server chunk routes native thread requests after Desktop's transform", async () => {
  const directory = await mkdtemp(join(tmpdir(), "codey-message-patch-"));
  const filename = join(directory, ".vite", "build", "src-transport.js");
  await mkdir(join(directory, ".vite", "build"), { recursive: true });
  await writeFile(filename, appServerTransportFixture);
  const runtime = await loadPatchInIsolatedContext(['model_provider="codey_router"'], {}, false);
  try {
    process.getBuiltinModule("module")._extensions[".js"]({
      _compile(source) { vm.runInNewContext(source, runtime.context); },
    }, filename);
    assert.equal(runtime.context.__CODEY_CODEX_STARTUP_PATCH__.localRouterMessageSourcePatched, true);
    const messages = [];
    const options = {
      hostKind: "local",
      getConnection: () => ({ send: (message) => messages.push(JSON.parse(message)) }),
      transformOutgoingMessage: (message) => ({ ...message, params: {
        ...message.params, modelProvider: "first", config: { ...message.params.config, "model_provider": "first", "artifact.session": "keep" },
      } }),
    };
    const transport = new runtime.context.Transport(options);
    for (const method of ["thread/start", "thread/resume", "thread/fork"]) {
      const message = Object.freeze({ id: 17, method, params: Object.freeze({
        threadId: "old-thread", model: "route-second/gpt-6-astra", modelProvider: null,
        config: Object.freeze({ model_providers: { first: { base_url: "https://wrong.example" } },
          "model_providers.codey_router.base_url": "https://wrong.example", "model_provider.name": "first", service_tier: "fast" }),
      }) });
      transport.sendMessage(message);
      assert.deepEqual(messages.at(-1), { id: 17, method, params: {
        threadId: "old-thread", model: "route-second/gpt-6-astra", modelProvider: "codey_router",
        config: { service_tier: "fast", "artifact.session": "keep" },
      } });
      assert.equal(message.params.modelProvider, null);
      assert.ok(message.params.config.model_providers);
    }
    options.transformOutgoingMessage = null;
    const turn = { id: 18, method: "turn/start", params: { threadId: "old-thread", model: "route-second/gpt-6-astra" } };
    transport.sendMessage(turn);
    assert.deepEqual(messages.at(-1), turn);
    for (const hostKind of ["ssh", "remote-control", "durable", undefined]) {
      options.hostKind = hostKind;
      const remote = { id: 19, method: "thread/resume", params: { threadId: "remote", modelProvider: "remote-provider" } };
      transport.sendMessage(remote);
      assert.deepEqual(messages.at(-1), remote);
    }
    const patch = runtime.context.__CODEY_PATCH_CODEX_APP_SERVER_MESSAGES__;
    assert.throws(() => patch("changed transport"), /matched 0/);
    assert.throws(() => patch(appServerTransportFixture.repeat(2)), /matched 2/);
    if (process.env.CODEY_TEST_CODEX_TRANSPORT_SOURCE) {
      const source = await readFile(process.env.CODEY_TEST_CODEX_TRANSPORT_SOURCE, "utf8");
      new vm.Script(patch(source));
    }
  } finally {
    runtime.restore();
    await rm(directory, { recursive: true, force: true });
  }
  const native = await loadPatchInIsolatedContext([]);
  try {
    const message = { method: "thread/resume", params: { modelProvider: "first" } };
    assert.equal(native.context.__CODEY_ROUTE_LOCAL_APP_SERVER_MESSAGE__(message, "local"), message);
  } finally { native.restore(); }
});

test("build chunks use one native source read and retain CommonJS loading semantics", async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), "codey-build-loading-")));
  const build = join(directory, ".vite", "build");
  await mkdir(build, { recursive: true });
  const dependency = join(build, "dependency.cjs");
  const entry = join(build, "entry.cjs");
  const broken = join(build, "broken.cjs");
  await writeFile(dependency, "module.exports=41;");
  await writeFile(entry, "#!/usr/bin/env node\nmodule.exports=require('./dependency.cjs')+1;");
  await writeFile(broken, "module.exports=;");
  const Module = process.getBuiltinModule("module");
  const fs = process.getBuiltinModule("fs");
  const nativeRead = fs.readFileSync;
  const reads = new Map();
  const runtime = await loadPatchInIsolatedContext([], {}, false);
  fs.readFileSync = function(filename, ...args) {
    if (String(filename).startsWith(build)) {
      reads.set(String(filename), (reads.get(String(filename)) ?? 0) + 1);
    }
    return Reflect.apply(nativeRead, this, [filename, ...args]);
  };
  try {
    const require = Module.createRequire(join(directory, "probe.cjs"));
    assert.equal(require(entry), 42);
    assert.equal(require(entry), 42);
    assert.equal(reads.get(entry), 1);
    assert.equal(reads.get(dependency), 1);
    const loaded = require.cache[entry];
    assert.equal(Object.hasOwn(loaded, "_compile"), false);
    assert.equal(loaded.loaded, true);
    assert.equal(loaded.children[0].filename, dependency);
    delete require.cache[dependency];
    await writeFile(dependency, "module.exports=99;");
    assert.equal(require(dependency), 99);
    assert.equal(reads.get(dependency), 2);
    const failed = new Module(broken);
    assert.throws(() => Module._extensions[".js"](failed, broken), SyntaxError);
    assert.equal(Object.hasOwn(failed, "_compile"), false);
  } finally {
    fs.readFileSync = nativeRead;
    runtime.restore();
    delete Module._cache[entry];
    delete Module._cache[dependency];
    await rm(directory, { recursive: true, force: true });
  }
});

test("router mode refuses to spawn when the shared request patch is missing", async () => {
  const runtime = await loadPatchInIsolatedContext(['model_provider="codey_router"'], {}, false);
  try {
    const pending = runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__();
    assert.throws(() => process.getBuiltinModule("child_process").spawn("codex", ["app-server"]), /未能启用本地路由请求处理/);
    assert.equal(runtime.spawnCalls.length, 0);
    await assert.rejects(pending, /未能启用本地路由请求处理/);
  } finally { runtime.restore(); }
});

test("desktop patches follow split 26.903 chunks and preserve dollar-prefixed listeners", async () => {
  const directory = await mkdtemp(join(tmpdir(), "codey-split-patch-"));
  const build = join(directory, ".vite", "build");
  await mkdir(build, { recursive: true });
  const chunks = {
    "main-fixture.js": [
      "let u={},d={analyticsEnabled:u!=null&&u.analytics?.enabled!==!1};",
      "f.postMessage({type:`worker-analytics-enabled-update`,enabled:e.analytics?.enabled!==!1});",
      "let $e=()=>{Qe.reconcileExternalPluginState(`focus`)};",
      "l.app.on(`browser-window-focus`,$e),$I.add(()=>{l.app.off(`browser-window-focus`,$e)});",
      "class Sampler{start(){this.appStateHeartbeat=setInterval(()=>{this.requestAppStateSnapshot(`heartbeat`)},gX),this.appStateHeartbeat.unref()}}",
      "const request=`electron-app-state-snapshot-request`;",
    ].join(""),
    "window-all-closed-fixture.js": [
      "const failure=`datadog-log-sink-failure`;",
      "let d=new n.wt({analyticsEnabled:s.get().then(e=>e.analytics?.enabled!==!1)}),",
      "f=new n.Ct({source:`codex-desktop`,transport:d});",
    ].join(""),
    "src-fixture.js": [
      "async function title(){return await $9({feature:`thread_title`})}",
      "async function $9({feature:i}){try{let h=await V0({model:tj,threadSource:i});",
      "return WA({feature:i,model:tj}),h}catch(e){throw WA({feature:i,model:tj}),e}}function next(){}",
    ].join(""),
  };
  chunks["main-monolithic.js"] = Object.values(chunks).map((source) => `{${source}}`).join("");
  if (process.env.CODEY_TEST_CODEX_BUILD_DIR) {
    for (const name of await readdir(process.env.CODEY_TEST_CODEX_BUILD_DIR)) {
      if (/^(?:main-|src-|window-all-closed-).*\.js$/.test(name)) {
        chunks[name] = await readFile(join(process.env.CODEY_TEST_CODEX_BUILD_DIR, name), "utf8");
      }
    }
  }
  try {
    for (const entries of [Object.entries(chunks), Object.entries(chunks).reverse()]) {
      const runtime = await loadPatchInIsolatedContext([], {}, false);
      try {
        for (const [name, input] of entries) {
          const filename = join(build, name);
          await writeFile(filename, input);
          let output;
          process.getBuiltinModule("module")._extensions[".js"]({
            _compile(source) { output = source; new vm.Script(source); },
          }, filename);
          if (name.startsWith("main-")) {
            assert.match(output, /worker-analytics-enabled-update`,enabled:!1/);
            assert.match(output, /\$e\.cancel\?\.\(\)/);
          } else if (input.includes("thread_title")) {
            assert.equal(output.match(/globalThis\.__CODEY_THREAD_TITLE_MODEL__/g)?.length, 3);
          } else if (input.includes("datadog-log-sink-failure") && input.includes("codex-desktop")) {
            assert.match(output, /analyticsEnabled:!1/);
            assert.doesNotMatch(output, /analyticsEnabled:.*\.get\(\)\.then/);
          } else {
            assert.equal(output, input);
          }
        }
        assert.equal(runtime.context.__CODEY_DESKTOP_ANALYTICS_SOURCE_PATCHED__, true);
        assert.equal(runtime.context.__CODEY_THREAD_TITLE_MODEL_SOURCE_PATCHED__, true);
        assert.equal(runtime.context.__CODEY_CODEX_STARTUP_PATCH__.optionalMainBundlePatchFailures.length, 0);
        // A successful worker chunk must not erase a shared transport failure.
        const filename = join(build, "window-all-closed-broken.js");
        const broken = chunks["window-all-closed-fixture.js"].replace("s.get()", "s.read()");
        await writeFile(filename, broken);
        process.getBuiltinModule("module")._extensions[".js"]({
          _compile(source) { assert.equal(source, broken); },
        }, filename);
        process.getBuiltinModule("module")._extensions[".js"]({ _compile() {} }, join(build, "main-fixture.js"));
        assert.equal(runtime.context.__CODEY_CODEX_STARTUP_PATCH__.disableDesktopCesAnalytics, false);
        assert.equal(runtime.context.__CODEY_DESKTOP_ANALYTICS_SOURCE_PATCHED__, false);
      } finally { runtime.restore(); }
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("macOS child-process sampler patch skips only the process-tree call", async () => {
  const directory = await mkdtemp(join(tmpdir(), "codey-macos-sampler-patch-"));
  const build = join(directory, ".vite", "build");
  await mkdir(build, { recursive: true });
  const filename = join(build, "main-fixture.js");
  await writeFile(filename, [
    "const sampler={async collectSnapshotFields(e){return process.platform!==`win32`&&await this.addChildProcessFields(i),e}};",
    "module.exports=sampler;",
  ].join(""));
  const runtime = await loadPatchInIsolatedContext([], {
    process: { ...process, platform: "darwin", env: {
      ...process.env,
      CODEY_DISABLE_MACOS_CHILD_PROCESS_SAMPLER: "true",
    } },
  }, false);
  try {
    let compiled;
    process.getBuiltinModule("module")._extensions[".js"]({
      _compile(source) { compiled = source; },
    }, filename);
    assert.match(compiled, /return false,e/);
    assert.equal(runtime.context.__CODEY_MACOS_CHILD_PROCESS_SAMPLER_SOURCE_PATCHED__, true);
  } finally {
    runtime.restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test("real CLI routes new and resumed threads through the local entry", {
  skip: !process.env.CODEY_TEST_CODEX_CLI,
  timeout: 30_000,
}, async (t) => {
  const home = await mkdtemp(join(tmpdir(), "codey-router-resume-"));
  t.after(() => rm(home, { recursive: true, force: true }));
  const requests = [];
  const ports = {};
  for (const provider of ["first", "codey_router"]) {
    const server = createServer(async (req, res) => {
      let body = "";
      for await (const chunk of req) body += chunk;
      requests.push({ provider, path: req.url, model: JSON.parse(body).model });
      // A terminal HTTP error persists the turn without needing a model service.
      res.writeHead(400, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: { message: "isolated routing probe", type: "invalid_request_error" } }));
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    t.after(() => new Promise((resolve) => server.close(resolve)));
    ports[provider] = server.address().port;
  }
  await writeFile(join(home, "config.toml"), [
    'model_provider="first"', 'model="gpt-5.4"',
    ...Object.entries(ports).flatMap(([provider, port]) => [
      `[model_providers.${provider}]`, `name="${provider}"`,
      `base_url="http://127.0.0.1:${port}/v1"`, 'wire_api="responses"', "requires_openai_auth=false",
    ]),
  ].join("\n"));
  const runtime = await loadPatchInIsolatedContext(['model_provider="codey_router"']);
  const routeMessage = runtime.context.__CODEY_ROUTE_LOCAL_APP_SERVER_MESSAGE__;
  runtime.restore();
  const spawn = process.getBuiltinModule("child_process").spawn;
  const startClient = async (routerDefault, normalize = (message) => message, wrapper = false) => {
    const child = spawn(wrapper ? process.env.CODEY_TEST_CODEX_WRAPPER : process.env.CODEY_TEST_CODEX_CLI, ["app-server",
      ...(routerDefault ? ["-c", 'model_provider="codey_router"'] : []),
    ], { env: { HOME: home, CODEX_HOME: home, PATH: process.env.PATH, RUST_LOG: "off",
      ...(wrapper ? {
        CODEY_CODEX_CLI_WRAPPER_TARGET: process.env.CODEY_TEST_CODEX_CLI,
        CODEY_CODEX_CLI_WRAPPER_OVERRIDES: JSON.stringify(['model_provider="codey_router"']),
        CODEY_CODEX_CLI_WRAPPER_PORT: "0", CODEY_CODEX_CLI_WRAPPER_TOKEN: "isolated-test",
      } : {}),
    }, stdio: ["pipe", "pipe", "ignore"] });
    t.after(() => { if (child.exitCode == null) child.kill(); });
    const messages = new EventEmitter();
    const lines = createInterface({ input: child.stdout });
    lines.on("line", (line) => messages.emit("message", JSON.parse(line)));
    const waitFor = (predicate) => new Promise((resolve, reject) => {
      const cleanup = () => {
        clearTimeout(timeout);
        messages.off("message", onMessage);
        child.off("exit", onExit);
        child.off("error", onError);
      };
      const onMessage = (message) => { if (predicate(message)) { cleanup(); resolve(message); } };
      const onError = (error) => { cleanup(); reject(error); };
      const onExit = (code) => onError(new Error(`app-server exited: ${code}`));
      const timeout = setTimeout(() => onError(new Error("app-server response timed out")), 10_000);
      messages.on("message", onMessage);
      child.on("exit", onExit);
      child.on("error", onError);
    });
    let id = 0;
    const rpc = async (method, params) => {
      const requestId = ++id;
      const response = waitFor((message) => message.id === requestId);
      child.stdin.write(JSON.stringify(normalize({ id: requestId, method, params }, "local")) + "\n");
      const result = await response;
      assert.equal(result.error, undefined, JSON.stringify(result.error));
      return result.result;
    };
    await rpc("initialize", { clientInfo: { name: "codey_routing_regression", version: "1" }, capabilities: { experimentalApi: true } });
    child.stdin.write('{"method":"initialized","params":{}}\n');
    return { rpc,
      async turn(threadId) {
        const completed = waitFor((message) => message.method === "turn/completed");
        await rpc("turn/start", { threadId, model: "route-second/gpt-5.4", input: [{ type: "text", text: "probe", text_elements: [] }] });
        await completed;
      },
      async stop() {
        const closed = new Promise((resolve) => child.once("close", resolve));
        child.stdin.end();
        await closed;
      },
    };
  };
  const seed = await startClient(false);
  const original = await seed.rpc("thread/start", { modelProvider: "first", model: "gpt-5.4", cwd: home, approvalPolicy: "never", sandbox: "read-only" });
  const threadId = original.thread.id;
  await seed.turn(threadId);
  await seed.stop();
  requests.length = 0;
  // Reproduce Desktop's null-provider resume with the local CLI default present.
  const before = await startClient(true);
  const old = await before.rpc("thread/resume", { threadId, modelProvider: null });
  assert.equal(old.modelProvider, "first");
  await before.turn(threadId);
  await before.stop();
  assert.deepEqual(requests, [{ provider: "first", path: "/v1/responses", model: "route-second/gpt-5.4" }]);
  const newThreadCases = [
    { modelProvider: null },
    { modelProvider: "first" },
    { modelProvider: null, config: { model_provider: "first" } },
    { modelProvider: "codey_router", config: {
      "model_providers.codey_router.base_url": `http://127.0.0.1:${ports.first}/v1`,
    } },
  ];
  const createThread = (client, params) => client.rpc("thread/start", {
    model: "route-second/gpt-5.4", cwd: home, approvalPolicy: "never", sandbox: "read-only",
    ephemeral: true, ...params,
  });
  // New threads can override both the CLI's default provider and its endpoint.
  const unpatched = await startClient(true);
  for (const params of newThreadCases) {
    requests.length = 0;
    const fresh = await createThread(unpatched, params);
    await unpatched.turn(fresh.thread.id);
    assert.deepEqual(requests, [{ provider: params === newThreadCases[0] ? "codey_router" : "first",
      path: "/v1/responses", model: "route-second/gpt-5.4" }]);
  }
  await unpatched.stop();
  for (const wrapper of process.env.CODEY_TEST_CODEX_WRAPPER ? [false, true] : [false]) {
    requests.length = 0;
    const after = await startClient(true, wrapper ? (message) => message : routeMessage, wrapper);
    const resumed = await after.rpc("thread/resume", { threadId, modelProvider: null, config: {
      model_provider: "first", "model_providers.codey_router.base_url": `http://127.0.0.1:${ports.first}/v1`,
    } });
    assert.equal(resumed.modelProvider, "codey_router");
    await after.turn(threadId);
    assert.deepEqual(requests, [{ provider: "codey_router", path: "/v1/responses", model: "route-second/gpt-5.4" }], wrapper ? "CLI wrapper" : "main transport");
    for (const params of newThreadCases) {
      requests.length = 0;
      const fresh = await createThread(after, params);
      assert.equal(fresh.modelProvider, "codey_router");
      await after.turn(fresh.thread.id);
      assert.deepEqual(requests, [{ provider: "codey_router", path: "/v1/responses", model: "route-second/gpt-5.4" }], wrapper ? "new thread via CLI wrapper" : "new thread via main transport");
    }
    await after.stop();
  }
});

test("router mode forces a private CLI and applies transport overrides after parent tables", async () => {
  const overrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:43127/v1"',
  ];
  const processWithExternalTransport = () => ({ ...process, env: {
    CODEX_APP_SERVER_FORCE_CLI: "0",
    CODEX_APP_SERVER_USE_LOCAL_DAEMON: "1",
    CODEX_APP_SERVER_WS_URL: "ws://127.0.0.1:9999",
  } });
  const runtime = await loadPatchInIsolatedContext(overrides, { process: processWithExternalTransport() });
  try {
    assert.equal(runtime.context.process.env.CODEX_APP_SERVER_FORCE_CLI, "1");
    const parent = 'model_providers={codey_router={base_url="https://wrong.example/v1"}}';
    process.getBuiltinModule("child_process").spawn("codex", ["app-server", "-c", parent]);
    const args = Array.from(runtime.spawnCalls.at(-1)[1]);
    assert.deepEqual(args.slice(-overrides.length * 2), overrides.flatMap((value) => ["-c", value]));
    assert.ok(args.indexOf(parent) < args.indexOf(overrides[1]));
    for (const command of ["proxy", "daemon"]) {
      assert.throws(() => process.getBuiltinModule("child_process").spawn("codex", ["app-server", command]), /proxy\/daemon/);
    }
  } finally {
    runtime.restore();
  }
  const native = await loadPatchInIsolatedContext([], { process: processWithExternalTransport() });
  try {
    assert.equal(native.context.process.env.CODEX_APP_SERVER_FORCE_CLI, "0");
  } finally {
    native.restore();
  }
});

test("thread title routing prefers official Luna, route Luna, then the default model", async () => {
  const runtime = await loadPatchInIsolatedContext([]);
  try {
    const select = runtime.context.__CODEY_SELECT_THREAD_TITLE_MODEL__;
    const base = [
      'model_provider="codey_router"',
      'model="relay/gpt-5.6-sol"',
    ];
    assert.equal(
      select([
        ...base,
        "model_providers.codey_router.requires_openai_auth=true",
      ], []),
      "gpt-5.6-luna",
    );
    assert.equal(
      select([
        ...base,
        "model_providers.codey_router.requires_openai_auth=false",
      ], [{ slug: "relay/gpt-5.6-luna" }]),
      "relay/gpt-5.6-luna",
    );
    assert.equal(
      select([
        ...base,
        "model_providers.codey_router.requires_openai_auth=false",
      ], [{ slug: "relay/gpt-5.6-sol" }]),
      "relay/gpt-5.6-sol",
    );

    const fixture = [
      "async function hfe(){let d=await $9({appServerClient:r,",
      "feature:`thread_title`,prompt:u})}",
      "async function $9({appServerClient:e,feature:i}){try{",
      "let h=await V0({model:tj,threadSource:i});",
      "return WA({feature:i,model:tj}),h}catch(e){",
      "throw WA({feature:i,model:tj}),e}}",
      "function yfe(){}const unrelated={model:tj};",
    ].join("");
    const patched = runtime.context.__CODEY_PATCH_CODEX_MAIN_THREAD_TITLE_MODEL__(
      fixture,
    );
    assert.equal(
      patched.match(/globalThis\.__CODEY_THREAD_TITLE_MODEL__/g)?.length,
      3,
    );
    assert.match(patched, /const unrelated=\{model:tj\}/);
  } finally {
    runtime.restore();
  }
});

test("startup patch disables Codex analytics and trims diagnostic polling", async () => {
  const Module = process.getBuiltinModule("module");
  const childProcess = process.getBuiltinModule("child_process");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const originalLoad = Module._load;
  const originalJsExtension = Module._extensions[".js"];
  const originalSpawn = childProcess.spawn;
  const NativeWorker = workerThreads.Worker;
  const spawnCalls = [];
  const ipcHandlers = new Map();
  const fakeIpcMain = new EventEmitter();
  fakeIpcMain.handle = (channel, handler) => {
    ipcHandlers.set(channel, handler);
  };
  const fakeElectron = {
    BrowserWindow: class BrowserWindow {},
    ipcMain: fakeIpcMain,
  };
  Module._load = function testElectronLoader(request) {
    if (request === "electron") return fakeElectron;
    return Reflect.apply(originalLoad, this, arguments);
  };
  childProcess.spawn = (...args) => {
    spawnCalls.push(args);
    return { pid: 42 };
  };

  try {
    const runtimeConfigOverrides = [
      "features.hooks=true",
      'model_provider="codey_global"',
      'model_providers.codey_global.base_url="http://127.0.0.1:61818/v1"',
      'developer_instructions="Codey route"',
      'mcp_servers.codey_fastctx.command="C:\\\\Program Files\\\\Codey\\\\codey-fastctx.exe"',
      'agents.default.config_file="D:\\\\Codey\\\\runtime\\\\default.toml"',
      'hooks.state."C:\\\\Users\\\\Kim\\\\.codex\\\\hooks.json:pre_tool_use:1:0".trusted_hash="sha256:test"',
      `hooks.PreToolUse=[{ hooks = [{ type = "command", command = "'C:\\\\Program Files\\\\Codey\\\\codey.exe' --codey-subagent-gate-hook" }] }]`,
    ];
    const nativeRuntimeConfigOverrides = runtimeConfigOverrides;
    const expression = await loadStartupPatchTemplate({ runtimeConfigOverrides });
    assert.equal((0, eval)(expression), "codey-startup-patch-installed-v40");

    const patchedElectron = Module._load("electron");
    const passthroughGitHandler = () => "git-handler";
    const passthroughMessageHandler = () => "message-handler";
    patchedElectron.ipcMain.handle(
      "codex_desktop:worker:git:from-view",
      passthroughGitHandler,
    );
    patchedElectron.ipcMain.handle(
      "codex_desktop:message-from-view",
      passthroughMessageHandler,
    );
    assert.equal(
      ipcHandlers.get("codex_desktop:worker:git:from-view"),
      passthroughGitHandler,
    );
    assert.equal(
      ipcHandlers.get("codex_desktop:message-from-view"),
      passthroughMessageHandler,
    );

    const desktopMcpConfig =
      'mcp_servers.codex_app={ command = "/opt/codex-app-mcp", args = ["server.mjs"] }';
    const directArgs = [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "--analytics-default-enabled",
      "-c",
      desktopMcpConfig,
    ];
    childProcess.spawn("/Applications/ChatGPT.app/Contents/Resources/codex", directArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "-c",
      desktopMcpConfig,
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    const fastctxRuntimeConfig = nativeRuntimeConfigOverrides.find((config) =>
      config.startsWith("mcp_servers.codey_fastctx.command="),
    );
    assert.ok(fastctxRuntimeConfig);
    assert.ok(
      spawnCalls.at(-1)[1].indexOf(fastctxRuntimeConfig) >
        spawnCalls.at(-1)[1].indexOf(desktopMcpConfig),
    );
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );
    const subagentGateRuntimeId =
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_RUNTIME_ID;
    assert.match(subagentGateRuntimeId, /^[A-Za-z0-9-]+$/);
    const alreadyPatchedDirectArgs = spawnCalls.at(-1)[1];
    childProcess.spawn("codex", alreadyPatchedDirectArgs);
    assert.equal(spawnCalls.at(-1)[1], alreadyPatchedDirectArgs);
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );
    const secondSubagentGateRuntimeId =
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_RUNTIME_ID;
    assert.notEqual(
      secondSubagentGateRuntimeId,
      subagentGateRuntimeId,
    );

    const wrappedAppServerArgs = [
      "/opt/codey/codex.js",
      "-c",
      'model_provider="stale_provider"',
      "--config",
      'model_providers.codey_global.base_url="https://stale.example/v1"',
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn(process.execPath, wrappedAppServerArgs);
    const patchedWrappedAppServerArgs = spawnCalls.at(-1)[1];
    assert.deepEqual(patchedWrappedAppServerArgs, [
      "/opt/codey/codex.js",
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      patchedWrappedAppServerArgs.filter(
        (argument) => argument === 'model_provider="codey_global"',
      ).length,
      1,
    );
    assert.equal(
      patchedWrappedAppServerArgs.some((argument) =>
        String(argument).includes("stale_provider") ||
        String(argument).includes("stale.example")
      ),
      false,
    );
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );

    const configuredArgs = [
      "-c",
      "analytics.enabled=true",
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn("codex", configuredArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const argsWithoutLegacyAnalyticsFlag = ["app-server"];
    childProcess.spawn("codex", argsWithoutLegacyAnalyticsFlag);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const unrelatedArgs = ["--version"];
    childProcess.spawn("git", unrelatedArgs);
    assert.equal(spawnCalls.at(-1)[1], unrelatedArgs);

    const unrelatedShell = "echo 'app-server --analytics-default-enabled'";
    childProcess.spawn("bash", ["-lc", unrelatedShell]);
    assert.equal(spawnCalls.at(-1)[1].at(-1), unrelatedShell);

    const runtimeManagedAppServerArgs = ["app-server", "--analytics-default-enabled"];
    childProcess.spawn("node", runtimeManagedAppServerArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const spawnOptions = { cwd: "/tmp" };
    childProcess.spawn("git", spawnOptions);
    assert.equal(spawnCalls.at(-1).length, 2);
    assert.equal(spawnCalls.at(-1)[1], spawnOptions);
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerAnalyticsPatchCount,
      5,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
    assert.equal(
      await globalThis.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__(),
      "codey-app-server-runtime-overrides-verified",
    );

    const desktopAnalyticsFixture = [
      "let u={},g={get(){return Promise.resolve({})}},",
      "d={analyticsEnabled:u!=null&&u.analytics?.enabled!==!1};",
      "p.postMessage({type:`worker-analytics-enabled-update`,",
      "enabled:e.analytics?.enabled!==!1});",
      "T=new Transport({analyticsEnabled:g.get().then(",
      "e=>e.analytics?.enabled!==!1)}),",
      "E=new Reporter({source:`codex-desktop`,transport:T});",
    ].join("");
    const patchedDesktopAnalytics =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        desktopAnalyticsFixture,
      );
    assert.equal(
      patchedDesktopAnalytics.match(/analyticsEnabled:!1/g)?.length,
      2,
    );
    assert.match(
      patchedDesktopAnalytics,
      /worker-analytics-enabled-update`,enabled:!1/,
    );
    assert.doesNotMatch(
      patchedDesktopAnalytics,
      /analytics\?\.enabled!==!1/,
    );

    const doubleQuotedDesktopAnalyticsFixture =
      desktopAnalyticsFixture.replaceAll("`", '"');
    const patchedDoubleQuotedDesktopAnalytics =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        doubleQuotedDesktopAnalyticsFixture,
      );
    assert.equal(
      patchedDoubleQuotedDesktopAnalytics.match(/analyticsEnabled:!1/g)?.length,
      2,
    );
    assert.match(
      patchedDoubleQuotedDesktopAnalytics,
      /worker-analytics-enabled-update",enabled:!1/,
    );
    assert.doesNotMatch(
      patchedDoubleQuotedDesktopAnalytics,
      /analytics\?\.enabled!==!1/,
    );

    const desktopAnalyticsWithoutReporterFixture =
      desktopAnalyticsFixture.replace(
        "E=new Reporter({source:`codex-desktop`,transport:T});",
        "",
      );
    const patchedDesktopAnalyticsWithoutReporter =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        desktopAnalyticsWithoutReporterFixture,
      );
    assert.equal(
      patchedDesktopAnalyticsWithoutReporter.match(/analyticsEnabled:!1/g)
        ?.length,
      2,
    );
    assert.doesNotMatch(
      patchedDesktopAnalyticsWithoutReporter,
      /analytics\?\.enabled!==!1/,
    );

    const incompatibleDesktopAnalyticsFixture =
      "const analyticsEnabledFromNewBundleShape = true;";
    const degradedDesktopAnalytics =
      globalThis.__CODEY_APPLY_OPTIONAL_MAIN_BUNDLE_PATCH__(
        "desktopCesAnalytics",
        globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__,
        incompatibleDesktopAnalyticsFixture,
      );
    assert.equal(
      degradedDesktopAnalytics,
      incompatibleDesktopAnalyticsFixture,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.disableDesktopCesAnalytics,
      false,
    );
    assert.deepEqual(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.optionalMainBundlePatchFailures,
      [{
        name: "desktopCesAnalytics",
        message: "Codey desktop analytics matches 0/0/0",
      }],
    );

    globalThis.__CODEY_APPLY_OPTIONAL_MAIN_BUNDLE_PATCH__(
      "desktopCesAnalytics",
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__,
      desktopAnalyticsFixture,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.disableDesktopCesAnalytics,
      true,
    );
    assert.deepEqual(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.optionalMainBundlePatchFailures,
      [],
    );

    const fixture = [
      "let Oe={},",
      "ke=()=>{Oe.reconcileExternalPluginState(`focus`)};",
      "l.app.on(`browser-window-focus`,ke);",
      "P.add(()=>{l.app.off(`browser-window-focus`,ke)});",
    ].join("");
    const patchedFixture =
      globalThis.__CODEY_PATCH_CODEX_MAIN_FOCUS_RECONCILE__(fixture);
    assert.match(
      patchedFixture,
      /ke=globalThis\.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__/,
    );
    assert.match(patchedFixture, /ke\.cancel\?\.\(\)/);

    const reconciles = [];
    const throttled =
      globalThis.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__(
        (value) => reconciles.push(value),
        20,
      );
    throttled("leading");
    throttled("middle");
    throttled("trailing");
    assert.deepEqual(reconciles, ["leading"]);
    await new Promise((resolve) => setTimeout(resolve, 35));
    assert.deepEqual(reconciles, ["leading", "trailing"]);
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__
        .externalPluginFocusReconcileSuppressedCount,
      2,
    );

    const cancelledReconciles = [];
    const cancelled =
      globalThis.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__(
        (value) => cancelledReconciles.push(value),
        20,
      );
    cancelled("leading");
    cancelled("trailing");
    cancelled.cancel();
    await new Promise((resolve) => setTimeout(resolve, 35));
    assert.deepEqual(cancelledReconciles, ["leading"]);

    const heartbeatFixture = [
      "class Sampler{constructor(){",
      "this.appStateHeartbeat=setInterval(()=>{",
      "this.requestAppStateSnapshot(`heartbeat`)",
      "},gX),this.appStateHeartbeat.unref()",
      "}dispose(){clearInterval(this.appStateHeartbeat)}",
      "requestAppStateSnapshot(e){",
      "send({type:`electron-app-state-snapshot-request`,reason:e})",
      "}}",
    ].join("");
    const patchedHeartbeat =
      globalThis.__CODEY_PATCH_CODEX_MAIN_APP_STATE_HEARTBEAT__(
        heartbeatFixture,
      );
    assert.match(patchedHeartbeat, /this\.appStateHeartbeat=null/);
    assert.doesNotMatch(patchedHeartbeat, /appStateHeartbeat=setInterval/);
    assert.match(
      patchedHeartbeat,
      /requestAppStateSnapshot\(e\).*electron-app-state-snapshot-request/,
    );
  } finally {
    childProcess.spawn = originalSpawn;
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    Module._load = originalLoad;
    Module._extensions[".js"] = originalJsExtension;
  }
});

test("startup patch fails closed when app-server runtime override injection is never observed", async () => {
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
    'model_providers.codey_router.http_headers={ x-codey-router-token = "codey-router-secret-token-1234" }',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides, {
    setTimeout(callback) {
      queueMicrotask(callback);
      return { unref() {} };
    },
    clearTimeout() {},
  });

  try {
    assert.match(
      await loadStartupPatchTemplate({
        runtimeConfigOverrides,
        subagentGateActive: false,
        requireAppServerRuntimeOverrides: true,
      }),
      /appServerRuntimeOverrideTimeoutMs = 20_000/,
    );
    assert.equal(runtime.result, "codey-startup-patch-installed-v40");
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.observed,
      false,
    );
    await assert.rejects(
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__(),
      (error) => {
        assert.match(
          error.message,
          /当前 Codex 版本的 app-server 启动参数结构与 Codey 不兼容/,
        );
        assert.match(error.message, /model_providers\.codey_router\.http_headers/);
        assert.doesNotMatch(error.message, /secret-token-1234/);
        return true;
      },
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch keeps Codey MCP servers in the app-server config layer", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'mcp_servers.codey_fastctx.command="/opt/codey-fastctx"',
    'mcp_servers.codey_fastctx.args=["--codey-fastctx-mcp"]',
    'mcp_servers.codey_subagent_control.command="/opt/codey"',
  ];
  const desktopMcpConfig =
    'mcp_servers.codex_app={ command = "/opt/codex-app-mcp", args = ["server.mjs"] }';
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    childProcess.spawn("codex", [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "-c",
      desktopMcpConfig,
    ]);
    const rewritten = Array.from(runtime.spawnCalls.at(-1)[1]);
    const appServerIndex = rewritten.indexOf("app-server");
    const desktopMcpIndex = rewritten.indexOf(desktopMcpConfig);

    assert.ok(appServerIndex >= 0);
    assert.ok(desktopMcpIndex > appServerIndex);
    for (const config of runtimeConfigOverrides) {
      const configIndex = rewritten.indexOf(config);
      assert.ok(
        configIndex > appServerIndex,
        `${config} must follow app-server`,
      );
      assert.ok(
        configIndex > desktopMcpIndex,
        `${config} must follow Desktop's overrides in the same config layer`,
      );
    }
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch resolves app-server runtime override validation after the matching spawn", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    const pending =
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__();
    childProcess.spawn("codex", ["app-server"]);
    assert.equal(
      await pending,
      "codey-app-server-runtime-overrides-verified",
    );
    assert.deepEqual(Array.from(runtime.spawnCalls.at(-1)[1]), [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...runtimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch tolerates duplicate Codex analytics flags while injecting runtime overrides", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    const pending =
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__();
    childProcess.spawn("codex", [
      "app-server",
      "--analytics-default-enabled",
      "--analytics-default-enabled",
    ]);
    assert.equal(
      await pending,
      "codey-app-server-runtime-overrides-verified",
    );
    assert.deepEqual(Array.from(runtime.spawnCalls.at(-1)[1]), [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...runtimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});
