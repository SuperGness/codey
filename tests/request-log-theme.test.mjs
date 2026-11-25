import assert from "node:assert/strict";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import { readSource } from "./helpers/read-source.mjs";

const { installRequestLogTheme } = await loadTypeScriptModule(new URL("../src/requestLogTheme.ts", import.meta.url));

function fixture(search, systemDark) {
  const listeners = new Set();
  const preference = {
    matches: systemDark,
    addEventListener(type, callback) { assert.equal(type, "change"); listeners.add(callback); },
    removeEventListener(type, callback) { assert.equal(type, "change"); listeners.delete(callback); },
  };
  const document = {
    documentElement: { dataset: {}, style: {} },
    defaultView: {
      location: { search },
      matchMedia(query) { assert.equal(query, "(prefers-color-scheme: dark)"); return preference; },
    },
  };
  return {
    document,
    listeners,
    change(matches) {
      preference.matches = matches;
      for (const listener of listeners) listener();
    },
    expectTheme(theme) {
      assert.equal(document.documentElement.dataset.theme, theme);
      assert.equal(document.documentElement.style.colorScheme, theme);
    },
  };
}

test("request logs follow the system theme and changes even with legacy theme parameters", () => {
  for (const search of ["", "?theme=light", "?theme=dark", "?theme=system", "?theme=untrusted"]) {
    for (const systemDark of [false, true]) {
      const f = fixture(search, systemDark);
      const dispose = installRequestLogTheme(f.document);
      const initialTheme = systemDark ? "dark" : "light";
      f.expectTheme(initialTheme);
      assert.equal(f.listeners.size, 1);
      f.change(!systemDark);
      f.expectTheme(systemDark ? "light" : "dark");
      f.change(systemDark);
      f.expectTheme(initialTheme);
      dispose();
      assert.equal(f.listeners.size, 0);
      f.change(!systemDark);
      f.expectTheme(initialTheme);
    }
  }
});

test("removing the request log token from the URL preserves query parameters on reload", async () => {
  const entry = await readSource("src/request-logs-entry.tsx");
  const source = entry.match(/function installBrowserBridge\(\) \{[\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const tokenKey = "test-request-log-token";
  const values = new Map();
  const window = {
    location: { pathname: "/codey/request-logs", search: "?theme=dark", hash: "#test%2Dtoken" },
    sessionStorage: {
      setItem: (key, value) => values.set(key, value),
      getItem: (key) => values.get(key),
    },
    history: {
      replaceState(_state, _title, url) {
        assert.equal(url, "/codey/request-logs?theme=dark");
        window.location.hash = "";
      },
    },
  };
  const install = new Function("window", "REQUEST_LOG_TOKEN_KEY", `${compiled}; return installBrowserBridge;`)(window, tokenKey);
  install();
  assert.equal(values.get(tokenKey), "test-token");
  assert.equal(window.location.hash, "");
  install();
  assert.equal(window.location.search, "?theme=dark");
  assert.equal(typeof window.__codeyInvokeApi, "function");
});
