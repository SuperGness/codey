import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("notice toast stays inside the app shell and auto-dismisses", async () => {
  const [appSource, stylesSource] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
  ]);

  assert.match(stylesSource, /\.notice-toast\s*\{[\s\S]*?position:\s*absolute;/);
  assert.match(stylesSource, /max-width:\s*min\(360px,\s*calc\(100% - 32px\)\)/);
  assert.doesNotMatch(
    stylesSource,
    /\.notice-toast\s*\{[\s\S]*?position:\s*fixed;/,
  );

  assert.match(
    appSource,
    /useEffect\(\(\) => \{[\s\S]*if \(!config \|\| !notice\.text\) return;/,
  );
  assert.match(appSource, /timeoutMs = notice\.tone === "error" \? 7000 : 4000/);
  assert.match(appSource, /window\.setTimeout\(/);
  assert.match(
    appSource,
    /current\.text === token \? \{ tone: "info", text: "" \} : current/,
  );
});