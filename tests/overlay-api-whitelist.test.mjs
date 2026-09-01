import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const api = await loadTypeScriptModule(new URL("../src/api.ts", import.meta.url));

test("overlay API paths reject commands outside the backend whitelist", () => {
  assert.equal(api.codeyApiPath("save_codey_config"), "/api/save_codey_config");
  assert.equal(api.isCodeyApiCommand("runtime_status"), true);
  assert.equal(api.isCodeyApiCommand("query_route_request_logs"), true);
  assert.equal(api.isCodeyApiCommand("clear_route_request_logs"), true);
  assert.equal(api.isCodeyApiCommand("../session/delete"), false);
  assert.throws(
    () => api.codeyApiPath("../session/delete"),
    /不允许的 Codey API 命令/,
  );
});

test("frontend and backend API command whitelists stay in sync", () => {
  const backend = fs.readFileSync(
    new URL("../backend/src/commands.rs", import.meta.url),
    "utf8",
  );
  const invokeApi = backend.slice(
    backend.indexOf("pub async fn invoke_api"),
    backend.indexOf("pub async fn load_codey_config"),
  );
  const backendCommands = [
    ...invokeApi.matchAll(/^\s*"([a-z][a-z0-9_]*)"\s*=>/gm),
  ].map((match) => match[1]);

  assert.deepEqual(
    [...api.CODEY_API_COMMANDS].sort(),
    backendCommands.sort(),
  );
});

test("overlay resolves bridge paths through the checked helper", () => {
  const overlay = fs.readFileSync(
    new URL("../src/overlay.tsx", import.meta.url),
    "utf8",
  );
  assert.match(overlay, /__codexSessionDeleteBridge\(codeyApiPath\(command\), args\)/);
  assert.doesNotMatch(overlay, /__codexSessionDeleteBridge\(`\/api\/\$\{command\}`/);
});
