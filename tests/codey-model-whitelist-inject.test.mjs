import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const MODEL_CONFIG_ID = "107580212";

async function loadPatch(
  catalogResponse,
  clients,
  { bridgeReady = true, queryClient = null, documentBody = null, storage = null } = {},
) {
  const [bridgeSource, source] = await Promise.all([
    readFile(new URL("../public/codey-bridge.js", import.meta.url), "utf8"),
    readFile(new URL("../public/model-whitelist-inject.js", import.meta.url), "utf8"),
  ]);
  let nextTimer = 0;
  const timers = new Map();
  const windowListeners = new Map();
  const documentListeners = new Map();
  const mutationObserverInstalls = [];
  const dispatchedEvents = [];
  let wildcardScanCount = 0;
  const body = documentBody || {};
  if (queryClient) {
    body.__reactFiber$codeyTest = {
      memoizedProps: {
        queryClient,
      },
    };
  }
  const head = documentBody ? new FakeElementCore("head") : null;
  const documentElement = documentBody ? new FakeElementCore("html") : {};
  const allDocumentRoots = () => [head, body, documentElement].filter(Boolean);
  const document = {
    body,
    documentElement,
    head,
    createElement(tagName) {
      return documentBody ? new FakeElementCore(tagName) : null;
    },
    getElementById() {
      return allDocumentRoots()
        .map((root) => root.querySelector?.("[id]"))
        .find(Boolean) || null;
    },
    querySelectorAll(selector) {
      if (selector === "*") wildcardScanCount += 1;
      return documentBody ? body.querySelectorAll(selector) : [];
    },
    addEventListener(name, listener) {
      const listeners = documentListeners.get(name) || new Set();
      listeners.add(listener);
      documentListeners.set(name, listeners);
    },
    removeEventListener(name, listener) {
      documentListeners.get(name)?.delete(listener);
    },
  };
  const bridge = async (path) => {
    assert.equal(path, "/codex-model-catalog");
    return typeof catalogResponse === "function"
      ? catalogResponse()
      : catalogResponse;
  };
  const window = {
    CustomEvent: class CustomEvent {
      constructor(type, init = {}) {
        this.type = type;
        this.detail = init.detail;
      }
    },
    __STATSIG__: {
      firstInstance: clients[0],
      instances: Object.fromEntries(clients.slice(1).map((client, index) => [index, client])),
    },
    addEventListener(name, listener) {
      const listeners = windowListeners.get(name) || new Set();
      listeners.add(listener);
      windowListeners.set(name, listeners);
    },
    removeEventListener(name, listener) {
      windowListeners.get(name)?.delete(listener);
    },
    setTimeout(callback) {
      nextTimer += 1;
      timers.set(nextTimer, callback);
      return nextTimer;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    dispatchEvent(event) {
      dispatchedEvents.push(event);
      for (const listener of windowListeners.get(event?.type) || []) {
        listener(event);
      }
      return true;
    },
    MutationObserver: class MutationObserver {
      constructor(callback) {
        this.callback = callback;
        this.disconnected = false;
      }

      observe(target, options) {
        mutationObserverInstalls.push({
          callback: this.callback,
          observer: this,
          options,
          target,
        });
      }

      disconnect() {
        this.disconnected = true;
      }
    },
  };
  if (storage) window.localStorage = storage;
  if (bridgeReady) window.__codexSessionDeleteBridge = bridge;
  Function("window", "document", "globalThis", "console", bridgeSource)(
    window,
    document,
    window,
    { warn() {} },
  );
  Function("window", "document", "globalThis", "console", source)(
    window,
    document,
    window,
    { warn() {} },
  );
  const patch = window.__codeyModelWhitelistPatch;
  if (bridgeReady) await patch.refresh();
  return {
    patch,
    connectBridge() {
      window.__codexSessionDeleteBridge = bridge;
    },
    dispatchWindowEvent(name, event) {
      window.dispatchEvent({ ...event, type: name });
    },
    dispatchDocumentEvent(name, event = {}) {
      for (const listener of documentListeners.get(name) || []) {
        listener({ ...event, type: name });
      }
    },
    dispatchedEvents() {
      return [...dispatchedEvents];
    },
    wildcardScanCount() {
      return wildcardScanCount;
    },
    mutationObserverInstalls() {
      return mutationObserverInstalls;
    },
    mutationObserverOptions() {
      return mutationObserverInstalls.map((install) => install.options);
    },
    dispatchObserverMutations(target, mutations) {
      for (const install of mutationObserverInstalls) {
        if (install.observer.disconnected || install.target !== target) continue;
        install.callback(mutations);
      }
    },
    async runNextTimer() {
      const next = timers.entries().next().value;
      assert.ok(next, "a retry timer should be pending");
      const [id, callback] = next;
      timers.delete(id);
      callback();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

function modelConfig(models, defaultModel) {
  return {
    value: {
      available_models: models,
      default_model: defaultModel,
      untouched: true,
    },
  };
}

function memoryStorage() {
  const values = new Map();
  let writes = 0;
  return {
    getItem(key) {
      return values.has(key) ? values.get(key) : null;
    },
    setItem(key, value) {
      writes += 1;
      values.set(key, String(value));
    },
    writeCount() {
      return writes;
    },
  };
}

function statsigClient(initialModels = ["gpt-5.6-sol", "gpt-5.3-codex"]) {
  const memo = modelConfig(initialModels, "gpt-5.4");
  const external = modelConfig(initialModels, "gpt-5.4");
  const internal = modelConfig(initialModels, "gpt-5.4");
  const events = [];
  return {
    memo,
    external,
    internal,
    events,
    _memoCache: {
      [`c|${MODEL_CONFIG_ID}`]: memo,
    },
    _store: {
      _valuesForExternalUse: {
        dynamic_configs: {
          [MODEL_CONFIG_ID]: external,
        },
      },
      _values: {
        _values: {
          dynamic_configs: {
            [MODEL_CONFIG_ID]: internal,
          },
        },
      },
    },
    getDynamicConfig(name) {
      return name === MODEL_CONFIG_ID
        ? modelConfig(initialModels, "gpt-5.4")
        : { value: { available_models: ["unrelated-model"] } };
    },
    $emt(event) {
      events.push(event);
    },
  };
}

function modelDescriptor(model, isDefault = false) {
  return {
    model,
    id: model,
    displayName: model,
    hidden: false,
    isDefault,
    defaultReasoningEffort: "medium",
    supportedReasoningEfforts: [{
      reasoningEffort: "medium",
      description: "medium effort",
    }],
  };
}

function activeModelQueryClient(initialModels) {
  const queryKey = ["models", "list", "local", "apikey", 100];
  const entries = new Map([[
    JSON.stringify(queryKey),
    {
      queryKey,
      data: {
        data: initialModels.map((model, index) => modelDescriptor(model, index === 0)),
        nextCursor: null,
      },
    },
  ]]);
  let invalidations = 0;
  return {
    get invalidations() {
      return invalidations;
    },
    getQueriesData({ queryKey: prefix }) {
      return [...entries.values()]
        .filter((entry) => prefix.every((value, index) => entry.queryKey[index] === value))
        .map((entry) => [entry.queryKey, entry.data]);
    },
    setQueryData(queryKeyValue, value) {
      const entry = entries.get(JSON.stringify(queryKeyValue));
      assert.ok(entry, "the active model query should exist");
      entry.data = typeof value === "function" ? value(entry.data) : value;
    },
    async invalidateQueries({ queryKey: prefix }) {
      assert.deepEqual(prefix, ["models", "list"]);
      invalidations += 1;
    },
    models() {
      return entries.get(JSON.stringify(queryKey)).data.data.map((model) => model.model);
    },
    model(modelName) {
      return entries
        .get(JSON.stringify(queryKey))
        .data
        .data
        .find((model) => model.model === modelName);
    },
  };
}

test("runtime whitelist keeps Spark and removes unsupported channel models", async () => {
  const firstClient = statsigClient();
  const secondClient = statsigClient(["gpt-5.6-terra"]);
  const expected = [
    "gpt-5.6-sol",
    "gpt-5.4",
    "gpt-5.3-codex-spark",
    "provider-fast-coder",
  ];
  const { patch } = await loadPatch({
    status: "ok",
    models: expected,
    default_model: "gpt-5.3-codex-spark",
  }, [firstClient, secondClient]);

  assert.deepEqual(patch.snapshot(), {
    loaded: true,
    models: expected,
    defaultModel: "gpt-5.3-codex-spark",
  });
  for (const client of [firstClient, secondClient]) {
    assert.deepEqual(client.memo.value.available_models, expected);
    assert.deepEqual(client.external.value.available_models, expected);
    assert.deepEqual(client.internal.value.available_models, expected);
    assert.equal(client.external.value.default_model, "gpt-5.3-codex-spark");

    const futureConfig = client.getDynamicConfig(MODEL_CONFIG_ID);
    assert.deepEqual(futureConfig.value.available_models, expected);
    assert.equal(futureConfig.value.default_model, "gpt-5.3-codex-spark");
    assert.equal(futureConfig.value.untouched, true);
    assert.deepEqual(
      client.getDynamicConfig("another-config"),
      { value: { available_models: ["unrelated-model"] } },
    );
  }
  assert.equal(expected.includes("gpt-5.3-codex"), false);
  assert.equal(expected.includes("gpt-5.6-terra"), false);
  patch.dispose();
});

test("an explicit refresh hot updates the native model list and default", async () => {
  const client = statsigClient();
  const catalogResponse = {
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  };
  const { patch } = await loadPatch(catalogResponse, [client]);

  catalogResponse.models = ["gpt-5.6-sol", "provider-hot-added"];
  catalogResponse.default_model = "provider-hot-added";
  await patch.refresh();

  assert.deepEqual(patch.snapshot(), {
    loaded: true,
    models: ["gpt-5.6-sol", "provider-hot-added"],
    defaultModel: "provider-hot-added",
  });
  assert.deepEqual(client.external.value.available_models, [
    "gpt-5.6-sol",
    "provider-hot-added",
  ]);
  assert.equal(client.external.value.default_model, "provider-hot-added");
  patch.dispose();
});

test("a backend-pushed catalog updates immediately without a nested bridge request", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  }, [client], { queryClient });
  const { patch } = runtime;
  const eventsBeforePush = client.events.length;

  assert.equal(patch.version, "40");
  assert.equal(await patch.setCatalog({
    status: "ok",
    models: ["gpt-5.6-sol", "provider-hot-pushed"],
    default_model: "provider-hot-pushed",
  }), true);
  assert.deepEqual(patch.snapshot(), {
    loaded: true,
    models: ["gpt-5.6-sol", "provider-hot-pushed"],
    defaultModel: "provider-hot-pushed",
  });
  assert.deepEqual(client.external.value.available_models, [
    "gpt-5.6-sol",
    "provider-hot-pushed",
  ]);
  assert.equal(client.external.value.default_model, "provider-hot-pushed");
  assert.ok(client.events.length > eventsBeforePush);
  assert.equal(client.events.at(-1).name, "values_updated");
  assert.deepEqual(queryClient.models(), [
    "gpt-5.6-sol",
    "provider-hot-pushed",
  ]);
  assert.ok(queryClient.invalidations > 0);
  assert.deepEqual(patch.delivery(), {
    revision: 2,
    statsigClients: 1,
    notifiedClients: 1,
    queryClients: 1,
    queryEntries: 1,
    reactContainers: 0,
    responsePatchInstalled: true,
  });

  runtime.dispatchWindowEvent("codex-message-from-view", {
    detail: {
      type: "mcp-request",
      request: {
        id: 41,
        method: "model/list",
        params: {},
      },
    },
  });
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: 41,
        result: {
          data: [modelDescriptor("provider-stale")],
          nextCursor: null,
        },
      },
    },
  };
  runtime.dispatchWindowEvent("message", response);
  assert.deepEqual(
    response.data.message.result.data.map((model) => model.model),
    ["gpt-5.6-sol", "provider-hot-pushed"],
  );
  patch.dispose();
});

test("an app-server model refresh after a turn keeps newly added route models", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "new-route/GLM-5.3-Flash"],
    default_model: "gpt-5.6-sol",
  }, [statsigClient()]);

  const preflight = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: { method: "model/list", params: { limit: 100 } },
  });
  assert.equal(preflight.request.id, undefined);

  runtime.patch.trackOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "model-list-after-turn",
      method: preflight.request.method,
      params: preflight.request.params,
    },
  });
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "model-list-after-turn",
        result: {
          data: [modelDescriptor("gpt-5.6-sol")],
          nextCursor: null,
        },
      },
    },
  };
  runtime.dispatchWindowEvent("message", response);

  assert.deepEqual(
    response.data.message.result.data.map((model) => model.model),
    ["gpt-5.6-sol", "new-route/GLM-5.3-Flash"],
  );
  runtime.patch.dispose();
});

test("explicit unknown thread and turn models are never replaced by a default", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "gpt-5.6-terra"],
    default_model: "gpt-5.6-sol",
  }, [statsigClient()]);

  for (const method of ["thread/start", "thread/resume", "turn/start"]) {
    const event = {
      detail: {
        type: "mcp-request",
        request: {
          id: method,
          method,
          params: {
            threadId: "stale-thread",
            model: "claude-opus-4-8",
          },
        },
      },
    };
    runtime.dispatchWindowEvent("codex-message-from-view", event);
    assert.equal(event.detail.request.params.model, "claude-opus-4-8");
  }
  runtime.patch.dispose();
});

test("route aliases display clearly and dispatch to the selected provider", async () => {
  const queryClient = activeModelQueryClient(["stale-model"]);
  const routeCatalog = {
    status: "ok",
    models: ["route-a/shared-model", "route-b/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [
      {
        model: "route-a/shared-model",
        display_name: "主线路 / shared-model",
        route_name: "主线路",
        provider_id: "codey_router",
        source_model: "route-a/shared-model",
        route_provider_id: "route-a",
        upstream_model: "shared-model",
        model_display_name: "shared-model",
      },
      {
        model: "route-b/shared-model",
        display_name: "备用线路 / shared-model",
        route_name: "备用线路",
        provider_id: "codey_router",
        source_model: "route-b/shared-model",
        route_provider_id: "route-b",
        upstream_model: "shared-model",
        model_display_name: "shared-model",
      },
    ],
  };
  const runtime = await loadPatch(routeCatalog, [statsigClient()], { queryClient });

  assert.equal(
    queryClient.model("route-a/shared-model").displayName,
    "主线路 / shared-model",
  );
  assert.equal(queryClient.model("route-a/shared-model").routeName, "主线路");
  assert.equal(queryClient.model("route-a/shared-model").codeyModelName, "shared-model");
  assert.equal(
    queryClient.model("route-b/shared-model").displayName,
    "备用线路 / shared-model",
  );

  const direct = {
    detail: {
      type: "mcp-request",
      request: {
        id: "route-direct",
        method: "turn/start",
        params: {
          model: "route-b/shared-model",
          responsesapiClientMetadata: { trace: "preserved", codey_route: "stale" },
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", direct);
  assert.deepEqual(direct.detail.request.params, {
    model: "route-b/shared-model",
    responsesapiClientMetadata: { trace: "preserved", codey_route: "route-b" },
  });

  const wrapped = {
    detail: {
      type: "mcp-request",
      request: {
        id: "route-wrapped",
        method: "send-cli-request-for-host",
        params: {
          method: "thread/start",
          params: { model: "route-a/shared-model" },
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", wrapped);
  assert.deepEqual(wrapped.detail.request.params.params, {
    model: "route-a/shared-model",
    modelProvider: "codey_router",
  });

  const resumed = {
    detail: {
      type: "mcp-request",
      request: {
        id: "route-resumed",
        method: "thread/resume",
        params: { model: "route-b/shared-model", model_provider: "codey_router" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", resumed);
  assert.deepEqual(resumed.detail.request.params, {
    model: "route-b/shared-model",
    modelProvider: "codey_router",
  });

  await runtime.patch.setCatalog({
    ...routeCatalog,
    model_metadata: routeCatalog.model_metadata.map((metadata) =>
      metadata.route_provider_id === "route-b"
        ? { ...metadata, display_name: "灾备线路 / shared-model" }
        : metadata,
    ),
  });
  assert.equal(
    queryClient.model("route-b/shared-model").displayName,
    "灾备线路 / shared-model",
  );

  await runtime.patch.setCatalog({
    status: "ok",
    models: ["route-a/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [routeCatalog.model_metadata[0]],
  });
  const deletedRouteRequest = {
    detail: {
      type: "mcp-request",
      request: {
        id: "deleted-route",
        method: "turn/start",
        params: {
          model: "route-b/shared-model",
          model_provider: "route-b",
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", deletedRouteRequest);
  assert.deepEqual(deletedRouteRequest.detail.request.params, {
    model: "route-b/shared-model",
    responsesapiClientMetadata: { codey_route: "route-b" },
  });
  assert.equal(
    runtime.patch.isBlockedOutgoingMessage(deletedRouteRequest.detail),
    false,
    "a stale route alias remains exact so the local gateway can reject it without guessing",
  );
  runtime.patch.dispose();
});

test("a raw model keeps its persisted thread route when multiple routes share the id", async () => {
  const storage = memoryStorage();
  const catalog = {
    status: "ok",
    models: ["route-a/shared-model", "route-b/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [
      {
        model: "route-a/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-a",
      },
      {
        model: "route-b/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-b",
      },
    ],
  };
  const firstRuntime = await loadPatch(catalog, [statsigClient()], { storage });
  const unboundRawTurn = firstRuntime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "ambiguous-unbound-turn",
      method: "turn/start",
      params: { threadId: "unknown-thread", model: "shared-model" },
    },
  });
  assert.deepEqual(unboundRawTurn.request.params, {
    threadId: "unknown-thread",
    model: "shared-model",
  }, "an ambiguous raw id must reach the gateway without a guessed route");
  const started = firstRuntime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "persisted-route-start",
      method: "thread/start",
      params: { model: "route-b/shared-model" },
    },
  });
  assert.deepEqual(started.request.params, {
    model: "route-b/shared-model",
    modelProvider: "codey_router",
  });
  firstRuntime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "persisted-route-start",
        result: { thread: { id: "persisted-thread", model: "shared-model" } },
      },
    },
  });
  firstRuntime.patch.dispose();

  const restoredRuntime = await loadPatch(catalog, [statsigClient()], { storage });
  const resumedTurn = restoredRuntime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "persisted-route-turn",
      method: "turn/start",
      params: { threadId: "persisted-thread", model: "shared-model" },
    },
  });
  assert.deepEqual(resumedTurn.request.params, {
    threadId: "persisted-thread",
    model: "route-b/shared-model",
    responsesapiClientMetadata: { codey_route: "route-b" },
  });
  restoredRuntime.patch.dispose();
});

test("thread responses expose the selector alias for unique raw route models", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["relay/gpt-5.5"],
    default_model: "relay/gpt-5.5",
    model_metadata: [{
      model: "relay/gpt-5.5",
      provider_id: "codey_router",
      source_model: "gpt-5.5",
      route_provider_id: "relay",
    }],
  }, [statsigClient()]);
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "read-thread-with-raw-model",
        result: {
          thread: {
            id: "raw-route-thread",
            model: "gpt-5.5",
            modelProvider: "codey_router",
          },
        },
      },
    },
  };

  runtime.dispatchWindowEvent("message", response);

  assert.equal(
    response.data.message.result.thread.model,
    "relay/gpt-5.5",
  );
  runtime.patch.dispose();
});

test("legacy custom-provider thread responses display the matching route alias", async () => {
  const alias = "aihub/gpt-5.6-sol";
  const runtime = await loadPatch({
    status: "ok",
    models: [alias],
    default_model: alias,
    model_metadata: [{
      model: alias,
      route_name: "AIHub",
      provider_id: "custom",
      source_model: "gpt-5.6-sol",
      route_provider_id: "aihub",
      upstream_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()]);
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "legacy-custom-thread-list",
        result: {
          data: [{
            id: "legacy-custom-thread",
            model: "gpt-5.6-sol",
            modelProvider: "custom",
          }],
        },
      },
    },
  };

  runtime.dispatchWindowEvent("message", response);

  assert.equal(response.data.message.result.data[0].model, alias);
  runtime.patch.dispose();
});

test("legacy custom-provider thread responses do not guess between shared routes", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["route-a/shared-model", "route-b/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [
      {
        model: "route-a/shared-model",
        provider_id: "custom",
        source_model: "shared-model",
        route_provider_id: "route-a",
      },
      {
        model: "route-b/shared-model",
        provider_id: "custom",
        source_model: "shared-model",
        route_provider_id: "route-b",
      },
    ],
  }, [statsigClient()]);
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "ambiguous-legacy-custom-thread",
        result: {
          thread: {
            id: "ambiguous-custom-thread",
            model: "shared-model",
            modelProvider: "custom",
          },
        },
      },
    },
  };

  runtime.dispatchWindowEvent("message", response);

  assert.equal(response.data.message.result.thread.model, "shared-model");
  runtime.patch.dispose();
});

test("thread display aliases replace immutable result payloads", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["relay/gpt-5.5"],
    default_model: "relay/gpt-5.5",
    model_metadata: [{
      model: "relay/gpt-5.5",
      provider_id: "codey_router",
      source_model: "gpt-5.5",
      route_provider_id: "relay",
    }],
  }, [statsigClient()]);
  const originalResult = Object.freeze({
    thread: Object.freeze({
      id: "immutable-thread",
      model: "gpt-5.5",
      modelProvider: "codey_router",
    }),
  });
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "immutable-thread-read",
        result: originalResult,
      },
    },
  };

  runtime.dispatchWindowEvent("message", response);

  assert.notEqual(response.data.message.result, originalResult);
  assert.equal(response.data.message.result.thread.model, "relay/gpt-5.5");
  runtime.patch.dispose();
});

test("thread responses use stored routes to disambiguate shared raw models", async () => {
  const storage = memoryStorage();
  storage.setItem("codey.thread-route-bindings.v1", JSON.stringify([
    ["persisted-thread", {
      routeProviderId: "route-b",
      sourceModel: "shared-model",
    }],
  ]));
  const runtime = await loadPatch({
    status: "ok",
    models: ["route-a/shared-model", "route-b/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [
      {
        model: "route-a/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-a",
      },
      {
        model: "route-b/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-b",
      },
    ],
  }, [statsigClient()], { storage });
  const response = {
    data: {
      type: "mcp-response",
      message: {
        id: "list-threads-with-raw-models",
        result: {
          data: [
            {
              id: "persisted-thread",
              model: "shared-model",
              modelProvider: "codey_router",
            },
            {
              id: "unbound-thread",
              model: "shared-model",
              modelProvider: "codey_router",
            },
          ],
        },
      },
    },
  };

  runtime.dispatchWindowEvent("message", response);

  assert.equal(response.data.message.result.data[0].model, "route-b/shared-model");
  assert.equal(response.data.message.result.data[1].model, "shared-model");
  runtime.patch.dispose();
});

test("unchanged thread routes do not rewrite the full persisted binding table", async () => {
  const storage = memoryStorage();
  const runtime = await loadPatch({
    status: "ok",
    models: ["route-a/shared-model", "route-b/shared-model"],
    default_model: "route-a/shared-model",
    model_metadata: [
      {
        model: "route-a/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-a",
      },
      {
        model: "route-b/shared-model",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "route-b",
      },
    ],
  }, [statsigClient()], { storage });

  const rewriteTurn = (model) => runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: `persist-${model}`,
      method: "turn/start",
      params: { threadId: "stable-route-thread", model },
    },
  });

  rewriteTurn("route-a/shared-model");
  const writesAfterFirstBinding = storage.writeCount();
  assert.ok(writesAfterFirstBinding > 0);
  for (let index = 0; index < 20; index += 1) {
    rewriteTurn("route-a/shared-model");
  }
  assert.equal(storage.writeCount(), writesAfterFirstBinding);

  rewriteTurn("route-b/shared-model");
  assert.equal(storage.writeCount(), writesAfterFirstBinding + 1);
  runtime.patch.dispose();
});

test("a persisted thread route is discarded when that route no longer exposes the model", async () => {
  const storage = memoryStorage();
  storage.setItem("codey.thread-route-bindings.v1", JSON.stringify([
    ["stale-route-thread", {
      routeProviderId: "route-b",
      sourceModel: "gpt-5.6-sol",
    }],
  ]));
  const runtime = await loadPatch({
    status: "ok",
    models: ["openai/gpt-5.6-sol", "route-b/claude-opus-5"],
    default_model: "route-b/claude-opus-5",
    model_metadata: [
      {
        model: "openai/gpt-5.6-sol",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: "route-b/claude-opus-5",
        provider_id: "codey_router",
        source_model: "claude-opus-5",
        route_provider_id: "route-b",
      },
    ],
  }, [statsigClient()], { storage });

  const nextTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-after-route-model-removal",
      method: "turn/start",
      params: {
        threadId: "stale-route-thread",
        model: "gpt-5.6-sol",
      },
    },
  });
  assert.deepEqual(nextTurn.request.params, {
    threadId: "stale-route-thread",
    model: "openai/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  assert.deepEqual(
    JSON.parse(storage.getItem("codey.thread-route-bindings.v1")),
    [["stale-route-thread", {
      routeProviderId: "openai",
      sourceModel: "gpt-5.6-sol",
    }]],
  );
  runtime.patch.dispose();
});

test("thread settings model changes replace the old route before the next turn", async () => {
  const catalog = {
    status: "ok",
    models: ["gpt-5.6-sol", "route-b/claude-opus-5"],
    default_model: "route-b/claude-opus-5",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
        route_name: "OpenAI 官方直登",
      },
      {
        model: "route-b/claude-opus-5",
        provider_id: "codey_router",
        source_model: "claude-opus-5",
        route_provider_id: "route-b",
        route_name: "新线路 2",
      },
    ],
  };
  const runtime = await loadPatch(catalog, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "settings-route-thread", modelProvider: "codey_router" }],
        },
      },
    },
  });

  const relaySelection = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "select-relay-model",
      method: "thread/settings/update",
      params: {
        threadId: "settings-route-thread",
        model: "route-b/claude-opus-5",
      },
    },
  });
  assert.deepEqual(relaySelection.request.params, {
    threadId: "settings-route-thread",
    model: "route-b/claude-opus-5",
  });

  const officialSelection = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "select-official-model",
      method: "thread/settings/update",
      params: {
        threadId: "settings-route-thread",
        model: "gpt-5.6-sol",
      },
    },
  });
  assert.deepEqual(officialSelection.request.params, {
    threadId: "settings-route-thread",
    model: "gpt-5.6-sol",
  });

  const nextTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-after-official-selection",
      method: "turn/start",
      params: {
        threadId: "settings-route-thread",
        input: [{ type: "text", text: "hello" }],
      },
    },
  });
  assert.deepEqual(nextTurn.request.params, {
    threadId: "settings-route-thread",
    input: [{ type: "text", text: "hello" }],
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("an explicit official settings choice beats an old same-id relay route", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "relay/gpt-5.6-sol"],
    default_model: "relay/gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: "relay/gpt-5.6-sol",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "relay",
      },
    ],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "same-id-thread-list",
        result: {
          data: [{ id: "same-id-thread", modelProvider: "codey_router" }],
        },
      },
    },
  });

  runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "same-id-select-relay",
      method: "thread/settings/update",
      params: { threadId: "same-id-thread", model: "relay/gpt-5.6-sol" },
    },
  });
  const officialSelection = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "same-id-select-official",
      method: "thread/settings/update",
      params: { threadId: "same-id-thread", model: "gpt-5.6-sol" },
    },
  });
  assert.deepEqual(officialSelection.request.params, {
    threadId: "same-id-thread",
    model: "gpt-5.6-sol",
  });

  const nextTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "same-id-next-turn",
      method: "turn/start",
      params: { threadId: "same-id-thread", model: "gpt-5.6-sol" },
    },
  });
  assert.deepEqual(nextTurn.request.params, {
    threadId: "same-id-thread",
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });

  const cleared = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "same-id-clear-model",
      method: "thread/settings/update",
      params: { threadId: "same-id-thread", model: null },
    },
  });
  assert.deepEqual(cleared.request.params, {
    threadId: "same-id-thread",
    model: null,
  });
  const turnAfterClear = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "same-id-turn-after-clear",
      method: "turn/start",
      params: { threadId: "same-id-thread" },
    },
  });
  assert.deepEqual(turnAfterClear.request.params, {
    threadId: "same-id-thread",
    model: "relay/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  runtime.patch.dispose();
});

test("a slash-containing model is preserved before the catalog finishes loading", async () => {
  const runtime = await loadPatch({ status: "failed" }, [statsigClient()]);
  const message = {
    type: "mcp-request",
    request: {
      id: "catalog-bootstrap-route",
      method: "thread/start",
      params: {
        model: "route-bootstrap/gpt-5.5",
        model_provider: "openai",
      },
    },
  };

  const rewritten = runtime.patch.rewriteOutgoingMessage(message);
  assert.deepEqual(rewritten.request.params, {
    model: "route-bootstrap/gpt-5.5",
    modelProvider: "openai",
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(rewritten), false);
  runtime.patch.dispose();
});

test("stale turn route metadata is removed before the catalog finishes loading", async () => {
  const storage = memoryStorage();
  storage.setItem("codey.thread-route-bindings.v1", JSON.stringify([
    ["early-catalog-thread", {
      routeProviderId: "route-b",
      sourceModel: "claude-opus-5",
    }],
  ]));
  const runtime = await loadPatch({ status: "failed" }, [statsigClient()], { storage });
  const rewritten = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "early-catalog-model-switch",
      method: "turn/start",
      params: {
        threadId: "early-catalog-thread",
        model: "gpt-5.6-sol",
        responsesapiClientMetadata: {
          codey_route: "route-b",
          workspace_kind: "project",
        },
      },
    },
  });
  assert.deepEqual(rewritten.request.params, {
    threadId: "early-catalog-thread",
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { workspace_kind: "project" },
  });

  const matchingRoute = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "early-catalog-same-model",
      method: "turn/start",
      params: {
        threadId: "early-catalog-thread",
        model: "claude-opus-5",
        responsesapiClientMetadata: { codey_route: "route-b" },
      },
    },
  });
  assert.deepEqual(matchingRoute.request.params, {
    threadId: "early-catalog-thread",
    model: "claude-opus-5",
    responsesapiClientMetadata: { codey_route: "route-b" },
  });
  runtime.patch.dispose();
});

test("a legacy OpenAI task resumes on the HTTP router even before catalog load", async () => {
  const runtime = await loadPatch({ status: "failed" }, [statsigClient()]);
  const rewritten = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "early-openai-resume",
      method: "thread/resume",
      params: {
        threadId: "legacy-official-thread",
        model_provider: "openai",
      },
    },
  });

  assert.deepEqual(rewritten.request.params, {
    threadId: "legacy-official-thread",
    modelProvider: "codey_router",
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(rewritten), false);
  runtime.patch.dispose();
});

test("an external-provider task resumes on the HTTP router before catalog load", async () => {
  const runtime = await loadPatch({ status: "failed" }, [statsigClient()]);
  const rewritten = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "early-external-resume",
      method: "thread/resume",
      params: {
        threadId: "external-thread-before-catalog",
        model: "vendor/model-with-a-slash",
        model_provider: "external_live",
      },
    },
  });

  assert.deepEqual(rewritten.request.params, {
    threadId: "external-thread-before-catalog",
    model: "vendor/model-with-a-slash",
    modelProvider: "codey_router",
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(rewritten), false);
  runtime.patch.dispose();
});

test("a legacy official task resumes through the local router carrier", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "relay/shared-model"],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        display_name: "官方线路 / gpt-5.6-sol",
        route_name: "官方线路",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: "relay/shared-model",
        display_name: "中转线路 / shared-model",
        route_name: "中转线路",
        provider_id: "codey_router",
        source_model: "relay/shared-model",
        route_provider_id: "relay",
        upstream_model: "shared-model",
      },
    ],
  }, [statsigClient()]);

  const resumed = {
    detail: {
      type: "mcp-request",
      request: {
        id: "resume-official-through-router",
        method: "thread/resume",
        params: {
          threadId: "official-thread",
          model: "gpt-5.6-sol",
          model_provider: "openai",
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", resumed);
  assert.deepEqual(resumed.detail.request.params, {
    threadId: "official-thread",
    model: "gpt-5.6-sol",
    modelProvider: "codey_router",
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(resumed.detail), false);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "resume-official-through-router",
        result: {
          thread: { id: "official-thread", modelProvider: "openai" },
          modelProvider: "codey_router",
        },
      },
    },
  });

  // The rollout still reports `openai` after a successful runtime migration.
  // A later list refresh must not downgrade the live carrier back to `openai`.
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "official-thread-list-after-resume",
        result: {
          data: [{ id: "official-thread", modelProvider: "openai" }],
        },
      },
    },
  });

  const selected = {
    detail: {
      type: "mcp-request",
      request: {
        id: "select-local-router-model",
        method: "turn/start",
        params: {
          threadId: "official-thread",
          model: "relay/shared-model",
          model_provider: "openai",
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", selected);
  assert.deepEqual(selected.detail.request.params, {
    threadId: "official-thread",
    model: "relay/shared-model",
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(selected.detail), false);
  runtime.patch.dispose();
});

test("an id-less app-server resume records its router migration after request creation", async () => {
  const alias = "relay/shared-model";
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", alias],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: alias,
        route_name: "中转线路",
        provider_id: "codey_router",
        source_model: "shared-model",
        route_provider_id: "relay",
      },
    ],
  }, [statsigClient()]);

  const preflight = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      method: "thread/resume",
      params: {
        threadId: "id-less-resume-thread",
        model: "gpt-5.6-sol",
        model_provider: "openai",
      },
    },
  });
  assert.deepEqual(preflight.request.params, {
    threadId: "id-less-resume-thread",
    model: "gpt-5.6-sol",
    modelProvider: "codey_router",
  });

  runtime.patch.trackOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "created-after-preflight",
      method: preflight.request.method,
      params: preflight.request.params,
    },
  });
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "created-after-preflight",
        result: {
          thread: {
            id: "id-less-resume-thread",
            modelProvider: "openai",
          },
        },
      },
    },
  });

  const switched = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-after-id-less-resume",
      method: "turn/start",
      params: {
        threadId: "id-less-resume-thread",
        model: alias,
      },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(switched), false);
  assert.deepEqual(switched.request.params, {
    threadId: "id-less-resume-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  runtime.patch.dispose();
});

test("a legacy custom-carrier thread resumes onto the router and continues on a third-party route", async () => {
  const alias = "aihub/gpt-5.6-sol";
  const runtime = await loadPatch({
    status: "ok",
    models: [alias],
    default_model: alias,
    model_metadata: [{
      model: alias,
      route_name: "AIHub",
      provider_id: "custom",
      source_model: "gpt-5.6-sol",
      route_provider_id: "aihub",
      upstream_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "legacy-custom-thread-list",
        result: {
          data: [{ id: "legacy-custom-thread", modelProvider: "custom" }],
        },
      },
    },
  });

  const resumed = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "resume-legacy-custom-thread",
      method: "thread/resume",
      params: {
        threadId: "legacy-custom-thread",
        model: alias,
        model_provider: "custom",
      },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(resumed), false);
  assert.deepEqual(resumed.request.params, {
    threadId: "legacy-custom-thread",
    model: alias,
    modelProvider: "codey_router",
  });
  runtime.patch.trackOutgoingMessage(resumed);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "resume-legacy-custom-thread",
        result: {
          thread: {
            id: "legacy-custom-thread",
            model: "gpt-5.6-sol",
            modelProvider: "custom",
          },
        },
      },
    },
  });
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "legacy-custom-thread-list-after-resume",
        result: {
          data: [{ id: "legacy-custom-thread", modelProvider: "custom" }],
        },
      },
    },
  });

  const continued = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "continue-legacy-custom-thread",
      method: "turn/start",
      params: { threadId: "legacy-custom-thread" },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(continued), false);
  assert.deepEqual(continued.request.params, {
    threadId: "legacy-custom-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "aihub" },
  });
  runtime.patch.dispose();
});

test("an external-provider thread resumes onto the router and switches to an official route", async () => {
  const alias = "openai/gpt-5.6-sol";
  const runtime = await loadPatch({
    status: "ok",
    models: [alias],
    default_model: alias,
    model_metadata: [{
      model: alias,
      route_name: "OpenAI 官方直登",
      provider_id: "codey_router",
      source_model: "gpt-5.6-sol",
      route_provider_id: "openai",
      upstream_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "external-official-thread-list",
        result: {
          data: [{ id: "external-official-thread", modelProvider: "external_live" }],
        },
      },
    },
  });

  const resumed = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "resume-external-official-thread",
      method: "thread/resume",
      params: {
        threadId: "external-official-thread",
        model: "legacy-vendor-model",
        model_provider: "external_live",
      },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(resumed), false);
  assert.deepEqual(resumed.request.params, {
    threadId: "external-official-thread",
    model: "legacy-vendor-model",
    modelProvider: "codey_router",
  });
  runtime.patch.trackOutgoingMessage(resumed);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "resume-external-official-thread",
        result: {
          thread: {
            id: "external-official-thread",
            model: "legacy-vendor-model",
            modelProvider: "external_live",
          },
        },
      },
    },
  });
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "external-official-thread-list-after-resume",
        result: {
          data: [{ id: "external-official-thread", modelProvider: "external_live" }],
        },
      },
    },
  });

  const switched = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "switch-external-thread-to-official",
      method: "turn/start",
      params: { threadId: "external-official-thread", model: alias },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(switched), false);
  assert.deepEqual(switched.request.params, {
    threadId: "external-official-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("an unmigrated OpenAI thread can keep using an official route directly", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["openai/gpt-5.6-sol", "relay/gpt-5.5"],
    default_model: "relay/gpt-5.5",
    model_metadata: [
      {
        model: "openai/gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: "relay/gpt-5.5",
        route_name: "中转线路",
        provider_id: "codey_router",
        source_model: "gpt-5.5",
        route_provider_id: "relay",
      },
    ],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "unmigrated-official-thread", modelProvider: "openai" }],
        },
      },
    },
  });

  const selected = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "select-official-on-unmigrated-thread",
      method: "thread/settings/update",
      params: {
        threadId: "unmigrated-official-thread",
        model: "openai/gpt-5.6-sol",
      },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(selected), false);
  assert.deepEqual(selected.request.params, {
    threadId: "unmigrated-official-thread",
    model: "gpt-5.6-sol",
  });

  const nextTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-on-unmigrated-official-thread",
      method: "turn/start",
      params: { threadId: "unmigrated-official-thread" },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(nextTurn), false);
  assert.deepEqual(nextTurn.request.params, {
    threadId: "unmigrated-official-thread",
    model: "gpt-5.6-sol",
  });
  runtime.patch.dispose();
});

test("a legacy local-official catalog route stays direct on an unmigrated OpenAI task", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["local-official/gpt-5.6-terra"],
    default_model: "local-official/gpt-5.6-terra",
    model_metadata: [{
      model: "local-official/gpt-5.6-terra",
      route_name: "OpenAI 官方直登",
      provider_id: "codey_router",
      source_model: "gpt-5.6-terra",
      route_provider_id: "local-official",
    }],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "legacy-local-official-thread-list",
        result: {
          data: [{ id: "legacy-local-official-thread", modelProvider: "openai" }],
        },
      },
    },
  });

  const turn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "legacy-local-official-default-turn",
      method: "turn/start",
      params: { threadId: "legacy-local-official-thread" },
    },
  });

  assert.equal(runtime.patch.isBlockedOutgoingMessage(turn), false);
  assert.deepEqual(turn.request.params, {
    threadId: "legacy-local-official-thread",
    model: "gpt-5.6-terra",
  });
  runtime.patch.dispose();
});

test("official-account route metadata keeps a custom official route direct on an OpenAI task", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["chatgpt-account/gpt-5.6-sol"],
    default_model: "chatgpt-account/gpt-5.6-sol",
    model_metadata: [{
      model: "chatgpt-account/gpt-5.6-sol",
      route_name: "OpenAI 官方直登",
      provider_id: "codey_router",
      source_model: "gpt-5.6-sol",
      route_provider_id: "chatgpt-account",
      official_account: true,
    }],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "custom-official-thread-list",
        result: {
          data: [{ id: "custom-official-thread", modelProvider: "openai" }],
        },
      },
    },
  });

  const selected = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "custom-official-settings",
      method: "thread/settings/update",
      params: {
        threadId: "custom-official-thread",
        model: "chatgpt-account/gpt-5.6-sol",
      },
    },
  });

  assert.equal(runtime.patch.isBlockedOutgoingMessage(selected), false);
  assert.deepEqual(selected.request.params, {
    threadId: "custom-official-thread",
    model: "gpt-5.6-sol",
  });
  runtime.patch.dispose();
});

test("an official thread must resume onto the router before selecting a third-party model", async () => {
  const alias = "relay/gpt-5.5";
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", alias],
    default_model: alias,
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: alias,
        route_name: "中转线路",
        provider_id: "codey_router",
        source_model: alias,
      },
    ],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "official-thread", modelProvider: "openai" }],
        },
      },
    },
  });
  const message = {
    type: "mcp-request",
    request: {
      id: "third-party-on-official",
      method: "turn/start",
      params: { threadId: "official-thread", model: alias },
    },
  };

  const rewritten = runtime.patch.rewriteOutgoingMessage(message);
  assert.equal(runtime.patch.isBlockedOutgoingMessage(rewritten), true);
  assert.deepEqual(rewritten.request.params, {
    threadId: "official-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  runtime.patch.dispose();
});

test("an unresumed external-provider task cannot bypass runtime migration", async () => {
  const alias = "relay/gpt-5.5";
  const runtime = await loadPatch({
    status: "ok",
    models: [alias],
    default_model: alias,
    model_metadata: [{
      model: alias,
      route_name: "中转线路",
      provider_id: "openai",
      source_model: alias,
      route_provider_id: "relay",
      upstream_model: "gpt-5.5",
    }],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "external-thread", modelProvider: "external_live" }],
        },
      },
    },
  });

  const rewritten = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "external-to-gateway",
      method: "turn/start",
      params: { threadId: "external-thread", model: alias },
    },
  });

  assert.deepEqual(rewritten.request.params, {
    threadId: "external-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(rewritten), true);
  runtime.patch.dispose();
});

test("a local-router thread can switch among third-party and official gateway routes", async () => {
  const routeA = "route-a/shared-model";
  const routeB = "route-b/shared-model";
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", routeA, routeB],
    default_model: routeA,
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: routeA,
        route_name: "线路 A",
        provider_id: "codey_router",
        source_model: routeA,
      },
      {
        model: routeB,
        route_name: "线路 B",
        provider_id: "codey_router",
        source_model: routeB,
      },
    ],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "router-thread", modelProvider: "codey_router" }],
        },
      },
    },
  });

  const thirdPartySwitch = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "switch-route",
      method: "turn/start",
      params: { threadId: "router-thread", model: routeB },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(thirdPartySwitch), false);
  assert.deepEqual(thirdPartySwitch.request.params, {
    threadId: "router-thread",
    model: routeB,
    responsesapiClientMetadata: { codey_route: "route-b" },
  });

  const officialSwitch = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "switch-official",
      method: "turn/start",
      params: { threadId: "router-thread", model: "gpt-5.6-sol" },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(officialSwitch), false);
  assert.deepEqual(officialSwitch.request.params, {
    threadId: "router-thread",
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("a prewarmed gateway thread switches models without an invalid turn provider override", async () => {
  const alias = "route-a/gpt-5.5";
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", alias],
    default_model: alias,
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: alias,
        route_name: "第三方线路",
        provider_id: "codey_router",
        source_model: alias,
      },
    ],
  }, [statsigClient()]);
  const prewarm = runtime.patch.rewriteOutgoingMessage({
    type: "thread-prewarm-start",
    request: {
      id: "prewarm-router-draft",
      method: "thread/start",
      params: {
        model: alias,
        modelProvider: "openai",
      },
    },
  });
  assert.deepEqual(prewarm.request.params, {
    model: alias,
    modelProvider: "codey_router",
  });
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "prewarm-router-draft",
        result: {
          thread: { id: "draft-thread", modelProvider: "codey_router" },
        },
      },
    },
  });

  const firstTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "first-official-turn",
      method: "turn/start",
      params: { threadId: "draft-thread", model: "gpt-5.6-sol" },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(firstTurn), false);
  assert.deepEqual(firstTurn.request.params, {
    threadId: "draft-thread",
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        method: "turn/started",
        params: {
          threadId: "draft-thread",
          turn: { id: "turn-1" },
        },
      },
    },
  });

  const laterThirdPartyTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "later-third-party-turn",
      method: "turn/start",
      params: { threadId: "draft-thread", model: alias },
    },
  });
  assert.equal(runtime.patch.isBlockedOutgoingMessage(laterThirdPartyTurn), false);
  assert.deepEqual(laterThirdPartyTurn.request.params, {
    threadId: "draft-thread",
    model: alias,
    responsesapiClientMetadata: { codey_route: "route-a" },
  });
  runtime.patch.dispose();
});

test("the transport preflight binds a new third-party thread to the local router", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "route-mt6lv4lx-i2bfax/gpt-5.5"],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: "route-mt6lv4lx-i2bfax/gpt-5.5",
        provider_id: "codey_router",
        source_model: "route-mt6lv4lx-i2bfax/gpt-5.5",
        route_provider_id: "route-mt6lv4lx-i2bfax",
        upstream_model: "gpt-5.5",
      },
    ],
  }, [statsigClient()]);
  const message = {
    type: "mcp-request",
    hostId: "local",
    request: {
      id: 91,
      method: "thread/start",
      params: {
        model: "route-mt6lv4lx-i2bfax/gpt-5.5",
        model_provider: "openai",
      },
    },
  };

  const rewritten = runtime.patch.rewriteOutgoingMessage(message);
  assert.notEqual(rewritten, message);
  assert.equal(rewritten.hostId, "local");
  assert.deepEqual(rewritten.request.params, {
    model: "route-mt6lv4lx-i2bfax/gpt-5.5",
    modelProvider: "codey_router",
  });
  assert.deepEqual(message.request.params, {
    model: "route-mt6lv4lx-i2bfax/gpt-5.5",
    model_provider: "openai",
  }, "the bridge preflight must be able to return a clone for frozen renderer envelopes");
  runtime.patch.dispose();
});

test("thread prewarm binds third-party models to the local router before the app server starts", async () => {
  const alias = "route-mt6lv4lx-i2bfax/gpt-5.5";
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", alias],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: alias,
        provider_id: "codey_router",
        source_model: alias,
        route_provider_id: "route-mt6lv4lx-i2bfax",
        upstream_model: "gpt-5.5",
      },
    ],
  }, [statsigClient()]);
  const message = {
    type: "thread-prewarm-start",
    hostId: "local",
    request: {
      id: 93,
      method: "thread/start",
      params: {
        model: alias,
        modelProvider: "openai",
      },
    },
    priority: "critical",
    source: "thread_open",
  };

  const rewritten = runtime.patch.rewriteOutgoingMessage(message);
  assert.notEqual(rewritten, message);
  assert.equal(rewritten.type, "thread-prewarm-start");
  assert.equal(rewritten.hostId, "local");
  assert.deepEqual(rewritten.request.params, {
    model: alias,
    modelProvider: "codey_router",
  });
  assert.deepEqual(message.request.params, {
    model: alias,
    modelProvider: "openai",
  });
  runtime.patch.dispose();
});

test("a hot default-model change replaces only the stale prewarm default", async () => {
  const oldDefault = "route-a/gpt-5.5";
  const newDefault = "route-a/claude-opus-5";
  const modelMetadata = [
    {
      model: oldDefault,
      display_name: "线路 A / gpt-5.5",
      route_name: "线路 A",
      provider_id: "codey_router",
      route_provider_id: "route-a",
      source_model: "gpt-5.5",
      upstream_model: "gpt-5.5",
    },
    {
      model: newDefault,
      display_name: "线路 A / claude-opus-5",
      route_name: "线路 A",
      provider_id: "codey_router",
      route_provider_id: "route-a",
      source_model: "claude-opus-5",
      upstream_model: "claude-opus-5",
    },
  ];
  const runtime = await loadPatch({
    status: "ok",
    models: [oldDefault, newDefault],
    default_model: oldDefault,
    model_metadata: modelMetadata,
  }, [statsigClient()]);
  await runtime.patch.setCatalog({
    status: "ok",
    models: [oldDefault, newDefault],
    default_model: newDefault,
    model_metadata: modelMetadata,
  });

  const prewarm = runtime.patch.rewriteOutgoingMessage({
    type: "thread-prewarm-start",
    request: {
      id: "stale-default-prewarm",
      method: "thread/start",
      params: { model: oldDefault, modelProvider: "codey_router" },
    },
  });
  assert.deepEqual(prewarm.request.params, {
    model: newDefault,
    modelProvider: "codey_router",
  });

  const explicitExistingThreadChange = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "explicit-old-model-on-existing-thread",
      method: "thread/settings/update",
      params: { threadId: "existing-thread", model: oldDefault },
    },
  });
  assert.deepEqual(explicitExistingThreadChange.request.params, {
    threadId: "existing-thread",
    model: oldDefault,
  });
  runtime.patch.dispose();
});

test("the first model-menu click overrides a lagging prewarm payload", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const menu = body.appendChild(new FakeElementCore("div", {
    attributes: { role: "menu" },
  }));
  const oldItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  oldItem.textContent = "线路 A / gpt-5.5";
  const selectedItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  selectedItem.textContent = "线路 A / claude-opus-5";
  const oldModel = "route-a/gpt-5.5";
  const selectedModel = "route-a/claude-opus-5";
  const runtime = await loadPatch({
    status: "ok",
    models: [oldModel, selectedModel],
    default_model: oldModel,
    model_metadata: [
      {
        model: oldModel,
        display_name: "线路 A / gpt-5.5",
        route_name: "线路 A",
        provider_id: "codey_router",
        route_provider_id: "route-a",
        source_model: "gpt-5.5",
      },
      {
        model: selectedModel,
        display_name: "线路 A / claude-opus-5",
        route_name: "线路 A",
        provider_id: "codey_router",
        route_provider_id: "route-a",
        source_model: "claude-opus-5",
      },
    ],
  }, [statsigClient()], { documentBody: body });
  runtime.patch.enhanceModelMenus();
  runtime.dispatchDocumentEvent("pointerdown", { target: selectedItem });

  const prewarm = runtime.patch.rewriteOutgoingMessage({
    type: "thread-prewarm-start",
    request: {
      id: "first-click-lagging-prewarm",
      method: "thread/start",
      params: { model: oldModel, modelProvider: "codey_router" },
    },
  });
  assert.deepEqual(prewarm.request.params, {
    model: selectedModel,
    modelProvider: "codey_router",
  });
  runtime.patch.dispose();
});

test("wrapped host thread starts preserve their envelope at the transport preflight", async () => {
  const alias = "route-mt6lv4lx-i2bfax/gpt-5.5";
  const runtime = await loadPatch({
    status: "ok",
    models: [alias],
    default_model: alias,
    model_metadata: [{
      model: alias,
      provider_id: "codey_router",
      source_model: alias,
    }],
  }, [statsigClient()]);
  const message = {
    type: "mcp-request",
    request: {
      id: 92,
      method: "send-cli-request-for-host",
      params: {
        hostId: "local",
        method: "thread/start",
        params: { model: alias },
      },
    },
  };

  const rewritten = runtime.patch.rewriteOutgoingMessage(message);
  assert.equal(rewritten.request.method, "send-cli-request-for-host");
  assert.deepEqual(rewritten.request.params, {
    hostId: "local",
    method: "thread/start",
    params: {
      model: alias,
      modelProvider: "codey_router",
    },
  });
  runtime.patch.dispose();
});

test("model picker menu groups models under route headings without changing model ids", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const menu = body.appendChild(new FakeElementCore("div", {
    attributes: { role: "menu" },
  }));
  const officialItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  officialItem.textContent = "[官] gpt-5.6-sol";
  const relayItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  relayItem.textContent = "[中转] gpt-5.6-sol";

  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "relay/gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        display_name: "[官] gpt-5.6-sol",
        route_name: "官方线路",
        route_prefix: "官",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: "relay/gpt-5.6-sol",
        display_name: "[中转] gpt-5.6-sol",
        route_name: "中转线路",
        route_prefix: "中转",
        provider_id: "relay",
        source_model: "gpt-5.6-sol",
      },
    ],
  }, [statsigClient()], { documentBody: body });

  runtime.patch.enhanceModelMenus();

  assert.equal(menu.children[0].textContent, "官方线路");
  assert.equal(menu.children[1], officialItem);
  assert.equal(officialItem.textContent, "gpt-5.6-sol");
  assert.equal(officialItem.dataset.codeyRouteModel, "gpt-5.6-sol");
  assert.equal(officialItem.getAttribute("aria-label"), "官方线路 / gpt-5.6-sol");
  assert.equal(menu.children[2].textContent, "中转线路");
  assert.equal(menu.children[3], relayItem);
  assert.equal(relayItem.textContent, "gpt-5.6-sol");
  assert.equal(relayItem.dataset.codeyRouteModel, "relay/gpt-5.6-sol");
  assert.equal(relayItem.getAttribute("aria-label"), "中转线路 / gpt-5.6-sol");

  const originalHeadings = [menu.children[0], menu.children[2]];
  runtime.patch.enhanceModelMenus();
  assert.equal(menu.children.length, 4);
  assert.equal(menu.children[0], originalHeadings[0]);
  assert.equal(menu.children[2], originalHeadings[1]);

  const request = {
    detail: {
      type: "mcp-request",
      request: {
        id: "grouped-menu-selected-relay",
        method: "turn/start",
        params: { model: "relay/gpt-5.6-sol" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", request);
  assert.deepEqual(request.detail.request.params, {
    model: "relay/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  runtime.patch.dispose();
});

test("an open model picker hides a route row removed by a hot catalog update", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const menu = body.appendChild(new FakeElementCore("div", {
    attributes: { role: "menu" },
  }));
  const officialItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  officialItem.textContent = "[官] gpt-5.6-sol";
  const deletedRouteItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  deletedRouteItem.textContent = "[1] DeepSeek-V4-Flash-0731";

  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "route-1/DeepSeek-V4-Flash-0731"],
    default_model: "gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        display_name: "[官] gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: "route-1/DeepSeek-V4-Flash-0731",
        display_name: "[1] DeepSeek-V4-Flash-0731",
        route_name: "待删除线路",
        provider_id: "codey_router",
        route_provider_id: "route-1",
        source_model: "DeepSeek-V4-Flash-0731",
      },
    ],
  }, [statsigClient()], { documentBody: body });
  runtime.patch.enhanceModelMenus();
  assert.equal(deletedRouteItem.hasAttribute("hidden"), false);

  // Simulate the native menu retaining its already-mounted row while the
  // backend pushes the post-deletion catalog.
  deletedRouteItem.textContent = "[1] DeepSeek-V4-Flash-0731";
  delete deletedRouteItem.dataset.codeyRouteModel;
  delete deletedRouteItem.dataset.codeyRouteName;
  await runtime.patch.setCatalog({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
    model_metadata: [{
      model: "gpt-5.6-sol",
      display_name: "[官] gpt-5.6-sol",
      route_name: "OpenAI 官方直登",
      provider_id: "openai",
      source_model: "gpt-5.6-sol",
    }],
  });
  runtime.patch.enhanceModelMenus();

  assert.equal(officialItem.hasAttribute("hidden"), false);
  assert.equal(deletedRouteItem.hasAttribute("hidden"), true);
  assert.equal(deletedRouteItem.dataset.codeySupersededModel, "route-1/DeepSeek-V4-Flash-0731");
  assert.deepEqual(
    menu.children.filter((child) => child.dataset.codeyRouteHeading)
      .map((heading) => heading.textContent),
    ["OpenAI 官方直登"],
  );

  // A virtualized native row can be reused for a current model later. The
  // stale marker must not leave that recycled row hidden.
  deletedRouteItem.textContent = "[官] gpt-5.6-sol";
  runtime.patch.enhanceModelMenus();
  assert.equal(deletedRouteItem.hasAttribute("hidden"), false);
  assert.equal(deletedRouteItem.dataset.codeyRouteModel, "gpt-5.6-sol");
  runtime.patch.dispose();
});

test("model picker observes row text changes after a route rename", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const menu = body.appendChild(new FakeElementCore("div", {
    attributes: { role: "menu" },
  }));
  const runtime = await loadPatch({
    status: "ok",
    models: ["relay/gpt-5.6-sol"],
    default_model: "relay/gpt-5.6-sol",
    model_metadata: [{
      model: "relay/gpt-5.6-sol",
      display_name: "[新线] gpt-5.6-sol",
      route_name: "新线路",
      route_prefix: "新线",
      provider_id: "relay",
      source_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()], { documentBody: body });

  const installs = runtime.mutationObserverInstalls();
  assert.equal(installs.length, 2);
  assert.equal(installs[0].target, body);
  assert.deepEqual(installs[0].options, {
    childList: true,
    subtree: true,
  });
  assert.equal(installs[1].target, menu);
  assert.deepEqual(installs[1].options, {
    childList: true,
    characterData: true,
    subtree: true,
  });
  runtime.patch.dispose();
});

test("model picker attaches text observers when a menu mounts later", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const runtime = await loadPatch({
    status: "ok",
    models: ["relay/gpt-5.6-sol"],
    default_model: "relay/gpt-5.6-sol",
    model_metadata: [{
      model: "relay/gpt-5.6-sol",
      display_name: "[新线] gpt-5.6-sol",
      route_name: "新线路",
      route_prefix: "新线",
      provider_id: "relay",
      source_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()], { documentBody: body });

  assert.equal(runtime.mutationObserverInstalls().length, 1);
  const menu = new FakeElementCore("div", {
    attributes: { role: "menu" },
  });
  body.appendChild(menu);
  runtime.dispatchObserverMutations(body, [{
    addedNodes: [menu],
    removedNodes: [],
    target: body,
    type: "childList",
  }]);
  const installs = runtime.mutationObserverInstalls();
  assert.equal(installs.length, 2);
  assert.equal(installs[1].target, menu);
  assert.equal(installs[1].options.characterData, true);
  runtime.patch.dispose();
});

test("model picker ignores streaming mutations outside the picker", async () => {
  const body = new FakeElementCore("body", { connected: true });
  const turn = body.appendChild(new FakeElementCore("div", {
    attributes: { "data-turn-key": "t1" },
  }));
  const runtime = await loadPatch({
    status: "ok",
    models: ["relay/gpt-5.6-sol"],
    default_model: "relay/gpt-5.6-sol",
    model_metadata: [{
      model: "relay/gpt-5.6-sol",
      display_name: "[新线] gpt-5.6-sol",
      route_name: "新线路",
      route_prefix: "新线",
      provider_id: "relay",
      source_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()], { documentBody: body });

  assert.equal(runtime.mutationObserverInstalls().length, 1);
  runtime.dispatchObserverMutations(body, [{
    addedNodes: [new FakeElementCore("span")],
    removedNodes: [],
    target: turn,
    type: "childList",
  }]);
  assert.equal(runtime.mutationObserverInstalls().length, 1);
  runtime.patch.dispose();
});

test("the clicked model route overrides stale thread and turn metadata", async () => {
  const storage = memoryStorage();
  storage.setItem("codey.thread-route-bindings.v1", JSON.stringify([
    ["menu-route-thread", {
      routeProviderId: "route-b",
      sourceModel: "gpt-5.6-sol",
    }],
  ]));
  const body = new FakeElementCore("body", { connected: true });
  const menu = body.appendChild(new FakeElementCore("div", {
    attributes: { role: "menu" },
  }));
  const officialItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  officialItem.textContent = "OpenAI 官方直登 / gpt-5.6-sol";
  const relayItem = menu.appendChild(new FakeElementCore("div", {
    attributes: { role: "menuitemradio" },
  }));
  relayItem.textContent = "新线路 2 / gpt-5.6-sol";

  const runtime = await loadPatch({
    status: "ok",
    models: ["openai/gpt-5.6-sol", "route-b/gpt-5.6-sol"],
    default_model: "route-b/gpt-5.6-sol",
    model_metadata: [
      {
        model: "openai/gpt-5.6-sol",
        display_name: "OpenAI 官方直登 / gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
      },
      {
        model: "route-b/gpt-5.6-sol",
        display_name: "新线路 2 / gpt-5.6-sol",
        route_name: "新线路 2",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "route-b",
      },
    ],
  }, [statsigClient()], { documentBody: body, storage });
  runtime.patch.enhanceModelMenus();
  runtime.dispatchDocumentEvent("pointerdown", { target: officialItem });

  const selectedTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-after-explicit-menu-selection",
      method: "turn/start",
      params: {
        threadId: "menu-route-thread",
        responsesapiClientMetadata: { codey_route: "route-b" },
      },
    },
  });
  assert.deepEqual(selectedTurn.request.params, {
    threadId: "menu-route-thread",
    model: "openai/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });

  const laterTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "turn-after-menu-intent-was-consumed",
      method: "turn/start",
      params: { threadId: "menu-route-thread" },
    },
  });
  assert.deepEqual(laterTurn.request.params, {
    threadId: "menu-route-thread",
    model: "openai/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("official account route models keep raw ids and dispatch to the OpenAI provider", async () => {
  const queryClient = activeModelQueryClient(["stale-model"]);
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
    model_metadata: [{
      model: "gpt-5.6-sol",
      display_name: "[官] gpt-5.6-sol",
      route_name: "OpenAI 官方直登",
      route_prefix: "官",
      provider_id: "openai",
      source_model: "gpt-5.6-sol",
    }],
  }, [statsigClient()], { queryClient });

  assert.equal(
    queryClient.model("gpt-5.6-sol").displayName,
    "[官] gpt-5.6-sol",
  );
  assert.equal(queryClient.model("gpt-5.6-sol").routeName, "OpenAI 官方直登");

  const request = {
    detail: {
      type: "mcp-request",
      request: {
        id: "official-raw-model",
        method: "turn/start",
        params: { model: "gpt-5.6-sol" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", request);

  assert.deepEqual(request.detail.request.params, {
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("official account models can run through the local router provider", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "relay/gpt-5.5"],
    default_model: "relay/gpt-5.5",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        display_name: "OpenAI 官方直登 / gpt-5.6-sol",
        route_name: "OpenAI 官方直登",
        provider_id: "codey_router",
        source_model: "gpt-5.6-sol",
        route_provider_id: "openai",
        upstream_model: "gpt-5.6-sol",
      },
      {
        model: "relay/gpt-5.5",
        display_name: "第三方线路 / gpt-5.5",
        route_name: "第三方线路",
        provider_id: "codey_router",
        source_model: "relay/gpt-5.5",
        route_provider_id: "relay",
        upstream_model: "gpt-5.5",
      },
    ],
  }, [statsigClient()]);
  runtime.dispatchWindowEvent("message", {
    data: {
      type: "mcp-response",
      message: {
        id: "thread-list",
        result: {
          data: [{ id: "router-thread", modelProvider: "codey_router" }],
        },
      },
    },
  });

  const officialTurn = runtime.patch.rewriteOutgoingMessage({
    type: "mcp-request",
    request: {
      id: "router-official-turn",
      method: "turn/start",
      params: { threadId: "router-thread", model: "gpt-5.6-sol" },
    },
  });

  assert.equal(runtime.patch.isBlockedOutgoingMessage(officialTurn), false);
  assert.deepEqual(officialTurn.request.params, {
    threadId: "router-thread",
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("official OpenAI route aliases dispatch raw model ids through the OpenAI provider from a relay default", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "relay/gpt-5.6-sol"],
    default_model: "relay/gpt-5.6-sol",
    model_metadata: [
      {
        model: "gpt-5.6-sol",
        display_name: "官方线路 / gpt-5.6-sol",
        provider_id: "openai",
        source_model: "gpt-5.6-sol",
      },
      {
        model: "relay/gpt-5.6-sol",
        display_name: "中转线路 / gpt-5.6-sol",
        provider_id: "relay",
        source_model: "gpt-5.6-sol",
      },
    ],
  }, [statsigClient()]);

  const official = {
    detail: {
      type: "mcp-request",
      request: {
        id: "official-route",
        method: "turn/start",
        params: {
          model: "openai/gpt-5.6-sol",
          model_provider: "openai",
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", official);
  assert.deepEqual(official.detail.request.params, {
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });

  const currentOfficial = {
    detail: {
      type: "mcp-request",
      request: {
        id: "official-current",
        method: "turn/start",
        params: {
          model: "gpt-5.6-sol",
          model_provider: "openai",
        },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", currentOfficial);
  assert.deepEqual(currentOfficial.detail.request.params, {
    model: "gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "openai" },
  });

  const relay = {
    detail: {
      type: "mcp-request",
      request: {
        id: "relay-route",
        method: "turn/start",
        params: { model: "relay/gpt-5.6-sol" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", relay);
  assert.deepEqual(relay.detail.request.params, {
    model: "relay/gpt-5.6-sol",
    responsesapiClientMetadata: { codey_route: "relay" },
  });
  runtime.patch.dispose();
});

test("official route selection does not inherit an active third party provider", async () => {
  const runtime = await loadPatch({
    status: "ok",
    model: "relay/gpt-5.6-sol",
    default_model: "relay/gpt-5.6-sol",
    model_provider: "relay",
    models: ["gpt-5.6-terra", "relay/gpt-5.6-sol"],
    model_metadata: [
      {
        model: "gpt-5.6-terra",
        display_name: "OpenAI 官方直登 / gpt-5.6-terra",
        route_name: "OpenAI 官方直登",
        provider_id: "openai",
        source_model: "gpt-5.6-terra",
      },
      {
        model: "relay/gpt-5.6-sol",
        display_name: "第三方线路 / gpt-5.6-sol",
        route_name: "第三方线路",
        provider_id: "relay",
        source_model: "gpt-5.6-sol",
      },
    ],
  }, [statsigClient()]);

  const request = {
    detail: {
      type: "mcp-request",
      request: {
        id: "official-from-third-party-runtime",
        method: "turn/start",
        params: { model: "gpt-5.6-terra" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", request);

  assert.deepEqual(request.detail.request.params, {
    model: "gpt-5.6-terra",
    responsesapiClientMetadata: { codey_route: "openai" },
  });

  const staleProviderRequest = {
    detail: {
      type: "mcp-request",
      request: {
        id: "official-from-stale-third-party-provider",
        method: "turn/start",
        params: { model: "gpt-5.6-terra", model_provider: "relay" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", staleProviderRequest);

  assert.deepEqual(staleProviderRequest.detail.request.params, {
    model: "gpt-5.6-terra",
    responsesapiClientMetadata: { codey_route: "openai" },
  });
  runtime.patch.dispose();
});

test("model IDs dedupe and match without case drift", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["Provider-Coder", " provider-coder ", "Provider-Reasoner"],
    default_model: "provider-coder",
  }, [statsigClient()]);

  assert.deepEqual(runtime.patch.snapshot(), {
    loaded: true,
    models: ["Provider-Coder", "Provider-Reasoner"],
    defaultModel: "Provider-Coder",
  });
  const event = {
    detail: {
      type: "mcp-request",
      request: {
        id: "case-insensitive-model",
        method: "turn/start",
        params: { model: "PROVIDER-REASONER" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", event);
  assert.equal(event.detail.request.params.model, "Provider-Reasoner");
  runtime.patch.dispose();
});

test("unchanged catalog retries and interactions do not repeat full React discovery", async () => {
  const catalog = {
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  };
  const runtime = await loadPatch(catalog, [statsigClient()]);

  assert.equal(runtime.wildcardScanCount(), 1);
  runtime.dispatchDocumentEvent("pointerdown");
  runtime.dispatchDocumentEvent("focusin");
  await Promise.resolve();
  assert.equal(runtime.wildcardScanCount(), 1);

  await runtime.patch.setCatalog(catalog);
  assert.equal(runtime.wildcardScanCount(), 1);
  assert.equal(runtime.patch.delivery().revision, 1);

  await runtime.runNextTimer();
  await runtime.runNextTimer();
  assert.equal(runtime.wildcardScanCount(), 1);

  await runtime.patch.setCatalog({
    ...catalog,
    models: ["gpt-5.6-sol", "provider-new"],
  });
  assert.equal(runtime.wildcardScanCount(), 2);
  assert.equal(runtime.patch.delivery().revision, 2);
  runtime.patch.dispose();
});

test("query client discovery reaches deep provider stacks", async () => {
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);
  // Current renderer builds memoize the host fiber far below the provider
  // stack holding the query client, so discovery must survive the hops up
  // the return chain before reaching the client context value.
  let fiber = { memoizedProps: { queryClient } };
  for (let index = 0; index < 10; index += 1) {
    fiber = { memoizedProps: {}, return: fiber };
  }
  const body = new FakeElementCore("body");
  body.__reactFiber$codeyTest = fiber;
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  }, [statsigClient()], { documentBody: body });

  const delivery = runtime.patch.delivery();
  assert.equal(delivery.queryClients, 1);
  assert.equal(delivery.queryEntries, 1);
  assert.deepEqual(queryClient.models(), ["gpt-5.6-sol"]);
  runtime.patch.dispose();
});

test("configured third-party models survive direct and wrapped requests", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["claude-opus-4-8", "deepseek-reasoner"],
    default_model: "claude-opus-4-8",
  }, [statsigClient()]);
  const direct = {
    detail: {
      type: "mcp-request",
      request: {
        method: "turn/start",
        params: { threadId: "valid-thread", model: "deepseek-reasoner" },
      },
    },
  };
  const wrapped = {
    detail: {
      type: "mcp-request",
      request: {
        method: "send-cli-request-for-host",
        params: {
          hostId: "local",
          method: "turn/start",
          params: { threadId: "valid-thread", model: "deepseek-reasoner" },
        },
      },
    },
  };

  runtime.dispatchWindowEvent("codex-message-from-view", direct);
  runtime.dispatchWindowEvent("codex-message-from-view", wrapped);

  assert.equal(direct.detail.request.params.model, "deepseek-reasoner");
  assert.equal(
    wrapped.detail.request.params.params.model,
    "deepseek-reasoner",
  );
  runtime.patch.dispose();
});

test("valid models survive and an explicit unknown wrapped model is preserved", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol", "gpt-5.6-terra"],
    default_model: "gpt-5.6-sol",
  }, [statsigClient()]);
  const direct = {
    detail: {
      type: "mcp-request",
      request: {
        method: "turn/start",
        params: { threadId: "valid-thread", model: "gpt-5.6-terra" },
      },
    },
  };
  const wrapped = {
    detail: {
      type: "mcp-request",
      request: {
        method: "send-cli-request-for-host",
        params: {
          hostId: "local",
          method: "turn/start",
          params: { threadId: "stale-thread", model: "claude-opus-4-8" },
        },
      },
    },
  };

  runtime.dispatchWindowEvent("codex-message-from-view", direct);
  runtime.dispatchWindowEvent("codex-message-from-view", wrapped);

  assert.equal(direct.detail.request.params.model, "gpt-5.6-terra");
  assert.equal(
    wrapped.detail.request.params.params.model,
    "claude-opus-4-8",
  );
  runtime.patch.dispose();
});

test("missing turn model receives the current route default", async () => {
  const runtime = await loadPatch({
    status: "ok",
    models: ["provider-current"],
    default_model: "provider-current",
  }, [statsigClient()]);
  const event = {
    detail: {
      type: "mcp-request",
      request: {
        method: "turn/start",
        params: { threadId: "legacy-thread" },
      },
    },
  };
  runtime.dispatchWindowEvent("codex-message-from-view", event);

  assert.equal(event.detail.request.params.model, "provider-current");
  runtime.patch.dispose();
});

test("an unchanged model list repairs missing reasoning effort options", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);
  const existing = queryClient.model("gpt-5.6-sol");
  existing.supportedReasoningEfforts = [];
  delete existing.defaultReasoningEffort;

  const { patch } = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  }, [client], { queryClient });

  const repaired = queryClient.model("gpt-5.6-sol");
  assert.deepEqual(
    repaired.supportedReasoningEfforts.map((effort) => effort.reasoningEffort),
    ["minimal", "low", "medium", "high", "xhigh"],
  );
  assert.equal(repaired.defaultReasoningEffort, "medium");
  patch.dispose();
});

test("an unchanged model list repairs missing native Fast tiers", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);
  const existing = queryClient.model("gpt-5.6-sol");
  existing.serviceTiers = [{
    id: "standard",
    name: "Standard",
    description: "Default speed",
  }];
  existing.additionalSpeedTiers = ["standard"];
  delete existing.defaultServiceTier;

  const { patch } = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
  }, [client], { queryClient });

  const repaired = queryClient.model("gpt-5.6-sol");
  assert.deepEqual(repaired.serviceTiers, [
    {
      id: "standard",
      name: "Standard",
      description: "Default speed",
    },
    {
      id: "priority",
      name: "Fast",
      description: "1.5x speed, increased usage",
    },
  ]);
  assert.deepEqual(repaired.additionalSpeedTiers, ["standard", "fast"]);
  assert.equal(repaired.defaultServiceTier, null);
  patch.dispose();
});

test("catalog model metadata overrides stale native reasoning efforts", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);

  const { patch } = await loadPatch({
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
    model_metadata: [{
      model: "gpt-5.6-sol",
      supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
      default_reasoning_effort: "low",
    }],
  }, [client], { queryClient });

  const repaired = queryClient.model("gpt-5.6-sol");
  assert.deepEqual(
    repaired.supportedReasoningEfforts.map((effort) => effort.reasoningEffort),
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.equal(repaired.defaultReasoningEffort, "low");
  patch.dispose();
});

test("third-party metadata replaces a stale high-only native descriptor", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["provider-fast-coder"]);
  const stale = queryClient.model("provider-fast-coder");
  stale.defaultReasoningEffort = "high";
  stale.supportedReasoningEfforts = [{
    reasoningEffort: "high",
    description: "high effort",
  }];

  const { patch } = await loadPatch({
    status: "ok",
    models: ["provider-fast-coder"],
    default_model: "provider-fast-coder",
    model_metadata: [{
      model: "provider-fast-coder",
      supported_reasoning_efforts: ["low", "medium", "high", "xhigh"],
      default_reasoning_effort: "low",
    }],
  }, [client], { queryClient });

  const repaired = queryClient.model("provider-fast-coder");
  assert.deepEqual(
    repaired.supportedReasoningEfforts.map((effort) => effort.reasoningEffort),
    ["low", "medium", "high", "xhigh"],
  );
  assert.equal(repaired.defaultReasoningEffort, "low");
  patch.dispose();
});

test("a refresh applies changed reasoning metadata when model ids stay unchanged", async () => {
  const client = statsigClient();
  const queryClient = activeModelQueryClient(["gpt-5.6-sol"]);
  const catalogResponse = {
    status: "ok",
    models: ["gpt-5.6-sol"],
    default_model: "gpt-5.6-sol",
    model_metadata: [{
      model: "gpt-5.6-sol",
      supported_reasoning_efforts: ["low", "medium"],
      default_reasoning_effort: "low",
    }],
  };
  const { patch } = await loadPatch(catalogResponse, [client], { queryClient });

  catalogResponse.model_metadata[0] = {
    model: "gpt-5.6-sol",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
    default_reasoning_effort: "high",
  };
  await patch.refresh();

  const refreshed = queryClient.model("gpt-5.6-sol");
  assert.deepEqual(
    refreshed.supportedReasoningEfforts.map((effort) => effort.reasoningEffort),
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.equal(refreshed.defaultReasoningEffort, "high");
  patch.dispose();
});

test("a stale bridge response cannot overwrite a backend-pushed catalog", async () => {
  const client = statsigClient();
  let resolveCatalog;
  const staleCatalog = new Promise((resolve) => {
    resolveCatalog = resolve;
  });
  const runtime = await loadPatch(() => staleCatalog, [client], {
    bridgeReady: false,
  });
  runtime.connectBridge();
  await Promise.resolve();
  await Promise.resolve();
  const staleRefresh = runtime.patch.refresh();

  assert.equal(await runtime.patch.setCatalog({
    status: "ok",
    models: ["provider-current"],
    default_model: "provider-current",
  }), true);
  resolveCatalog({
    status: "ok",
    models: ["provider-stale"],
    default_model: "provider-stale",
  });
  await staleRefresh;

  assert.deepEqual(runtime.patch.snapshot(), {
    loaded: true,
    models: ["provider-current"],
    defaultModel: "provider-current",
  });
  runtime.patch.dispose();
});

test("a synced channel with no supported models clears the native allowlist", async () => {
  const client = statsigClient();
  const { patch } = await loadPatch({
    status: "not_configured",
    models: [],
    default_model: "",
  }, [client]);

  assert.deepEqual(client.external.value.available_models, []);
  assert.equal(client.external.value.default_model, "");
  assert.deepEqual(
    client.getDynamicConfig(MODEL_CONFIG_ID).value.available_models,
    [],
  );
  patch.dispose();
});

test("the catalog load retries when the bridge appears after injection", async () => {
  const client = statsigClient();
  const runtime = await loadPatch({
    status: "ok",
    models: ["gpt-5.3-codex-spark"],
    default_model: "gpt-5.3-codex-spark",
  }, [client], { bridgeReady: false });

  assert.equal(runtime.patch.snapshot().loaded, false);
  runtime.connectBridge();
  await runtime.runNextTimer();

  assert.deepEqual(runtime.patch.snapshot(), {
    loaded: true,
    models: ["gpt-5.3-codex-spark"],
    defaultModel: "gpt-5.3-codex-spark",
  });
  assert.deepEqual(client.external.value.available_models, ["gpt-5.3-codex-spark"]);
  runtime.patch.dispose();
});

test("failed catalog responses preserve the native allowlist", async () => {
  const client = statsigClient();
  const { patch } = await loadPatch({
    status: "failed",
    message: "catalog unavailable",
  }, [client]);

  assert.equal(patch.snapshot().loaded, false);
  assert.deepEqual(
    client.external.value.available_models,
    ["gpt-5.6-sol", "gpt-5.3-codex"],
  );
  patch.dispose();
});

test("frozen Statsig results and Map memo caches receive patched copies", async () => {
  const frozenConfig = Object.freeze({
    value: Object.freeze({
      available_models: ["gpt-5.3-codex"],
      default_model: "gpt-5.3-codex",
    }),
  });
  const memoCache = new Map([[`c|${MODEL_CONFIG_ID}`, frozenConfig]]);
  const client = {
    _memoCache: memoCache,
    getDynamicConfig: () => frozenConfig,
  };
  const { patch } = await loadPatch({
    status: "ok",
    models: ["gpt-5.3-codex-spark"],
    default_model: "gpt-5.3-codex-spark",
  }, [client]);

  assert.notEqual(memoCache.get(`c|${MODEL_CONFIG_ID}`), frozenConfig);
  assert.deepEqual(
    memoCache.get(`c|${MODEL_CONFIG_ID}`).value.available_models,
    ["gpt-5.3-codex-spark"],
  );
  assert.deepEqual(
    client.getDynamicConfig(MODEL_CONFIG_ID).value.available_models,
    ["gpt-5.3-codex-spark"],
  );
  patch.dispose();
});
