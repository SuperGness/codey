import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { readImages, imagePickerError, MAX_IMAGE_BYTES, MAX_IMAGES } = await loadTypeScriptModule(new URL("../src/remote/images.ts", import.meta.url));
const file = (name = "photo.png", type = "image/png", size = 68) => ({ name, type, size });

function browser(t, mode = "ok") {
  const readers = [];
  class FileReader {
    static LOADING = 1;
    constructor() { readers.push(this); }
    readAsDataURL() {
      if (mode === "throw") throw new DOMException("private path", "NotReadableError");
      this.readyState = 1;
      if (mode === "pending") return;
      queueMicrotask(() => {
        this.readyState = 2; this.result = mode === "empty" ? "" : "data:application/octet-stream;base64,YQ==";
        if (mode === "read-error") this.onerror?.(); else this.onload?.();
      });
    }
    abort() { this.aborted = true; this.readyState = 2; this.onabort?.(); }
  }
  class Image {
    set src(value) { this.value = value; if (value) queueMicrotask(() => mode === "decode-error" ? this.onerror?.() : this.onload?.()); }
    get src() { return this.value; }
  }
  for (const [name, value] of Object.entries({ FileReader, Image })) {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { configurable: true, value });
    t.after(() => { if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name]; });
  }
  return readers;
}

test("photo reads preserve order, normalize empty MIME and only return a complete selection", async t => {
  browser(t);
  const images = await readImages([file(), file("camera.JPG", "")], [], new AbortController().signal);
  assert.deepEqual(images, [{ name: "photo.png", url: "data:image/png;base64,YQ==", size: 68 }, { name: "camera.JPG", url: "data:image/jpeg;base64,YQ==", size: 68 }]);
  assert.deepEqual(await readImages([], images, new AbortController().signal), []);
});

test("selection validates count, empty files, format and combined size before reading", async t => {
  const readers = browser(t);
  for (const [files, existing, message] of [
    [Array(MAX_IMAGES + 1).fill(file()), [], /最多/],
    [[file()], Array(MAX_IMAGES).fill({ size: 1 }), /最多/],
    [[file("p.png", "image/png", MAX_IMAGE_BYTES)], [{ size: 1 }], /4 MB/],
    [[file("p.png", "image/png", 0)], [], /为空/],
    [[file("p.svg", "image/svg+xml")], [], /格式/],
    [[file("p.heic", "image/heic")], [], /转换/],
    [[file("p.unknown", "")], [], /格式/],
  ]) await assert.rejects(readImages(files, existing, new AbortController().signal), message);
  assert.equal(readers.length, 0);
  assert.equal((await readImages([file("limit.png", "image/png", MAX_IMAGE_BYTES)], [], new AbortController().signal)).length, 1);
});

for (const mode of ["throw", "read-error", "empty", "decode-error"]) {
  test(`photo ${mode} reports a safe actionable error`, async t => {
    browser(t, mode);
    await assert.rejects(readImages([file()], [], new AbortController().signal), error => /图片|权限/.test(error.message) && !error.message.includes("private path"));
  });
}

test("leaving the composer aborts pending reads and prevents stale attachments", async t => {
  const readers = browser(t, "pending");
  const controller = new AbortController();
  const result = readImages([file()], [], controller.signal);
  controller.abort();
  await assert.rejects(result, { name: "AbortError" });
  assert.equal(readers[0].aborted, true);
  assert.equal(readers[0].onload, null);
  await assert.rejects(readImages([file()], [], controller.signal), { name: "AbortError" });
  assert.equal(readers.length, 1);
});

test("picker permission failures and unavailable browsers offer recovery without leaking details", () => {
  for (const name of ["NotAllowedError", "SecurityError"]) assert.match(imagePickerError(new DOMException("private", name)), /允许访问/);
  assert.match(imagePickerError(new Error("private")), /系统浏览器/);
  assert.doesNotMatch(imagePickerError(new DOMException("private", "NotSupportedError")), /private/);
});
