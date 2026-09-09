import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const formatters = await loadTypeScriptModule(new URL("../src/formatters.ts", import.meta.url));
test("DiagnosticCleanupNotice renders styled hero metrics, badges, and targets accurately", async () => {
  const { createRequire } = await import("node:module");
  const { fileURLToPath } = await import("node:url");
  const React = (await import("react")).default;
  const { renderToStaticMarkup } = await import("react-dom/server");

  const noticeFilename = fileURLToPath(new URL("../src/DiagnosticCleanupNotice.tsx", import.meta.url));
  const noticeSource = await readFile(noticeFilename, "utf8");
  const noticeCompiled = ts.transpileModule(noticeSource, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  const noticeExports = {};
  const requireFromTest = createRequire(noticeFilename);
  new Function("require", "exports", "React", noticeCompiled)((name) => {
    if (name === "./formatters") return formatters;
    return requireFromTest(name);
  }, noticeExports, React);
  const { DiagnosticCleanupNotice } = noticeExports;

  const result = {
    status: "ok",
    traceCleanup: { databasesCleaned: 2, rowsDeleted: 0, bytesBefore: 114688, bytesAfter: 114688, bytesReclaimed: 0 },
    traceLogStats: { capturedAt: 1, databaseBytes: 114688 },
    crashpadCleanup: { reportsDeleted: 0, filesDeleted: 0, bytesBefore: 0, bytesAfter: 0, bytesReclaimed: 0, skippedRecentReports: 0, unmanagedFiles: 0 },
    crashpadPendingStats: { capturedAt: 1 },
    errors: [],
  };

  // Trace with 0 B savings (matching user screenshot)
  const traceHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, { result, target: "trace" }));
  assert.match(traceHtml, /释放空间/);
  assert.match(traceHtml, /0 B/);
  assert.match(traceHtml, /无需额外释放/);
  assert.match(traceHtml, /112 KB/);
  assert.match(traceHtml, /已处理.*2.*个日志库/);
  assert.match(traceHtml, /删除.*0.*条记录/);
  assert.doesNotMatch(traceHtml, /Crashpad/);

  // Trace with actual savings
  const traceSavingsResult = {
    ...result,
    traceCleanup: { databasesCleaned: 1, rowsDeleted: 20, bytesBefore: 4096, bytesAfter: 1024, bytesReclaimed: 3072 },
    traceLogStats: { capturedAt: 1, databaseBytes: 1024 },
  };
  const savingsHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, { result: traceSavingsResult, target: "trace" }));
  assert.match(savingsHtml, /3\.0 KB/);
  assert.match(savingsHtml, /has-savings/);
  assert.match(savingsHtml, /4\.0 KB/);
  assert.match(savingsHtml, /1\.0 KB/);
  assert.match(savingsHtml, /已处理.*1.*个日志库/);
  assert.match(savingsHtml, /删除.*20.*条记录/);

  // Crashpad with retained files
  const crashpadResult = {
    ...result,
    crashpadCleanup: { reportsDeleted: 2, filesDeleted: 4, bytesBefore: 8192, bytesAfter: 2048, bytesReclaimed: 6144, skippedRecentReports: 1, unmanagedFiles: 2 },
    crashpadPendingStats: { capturedAt: 1 },
  };
  const crashpadHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, { result: crashpadResult, target: "crashpad" }));
  assert.match(crashpadHtml, /6\.0 KB/);
  assert.match(crashpadHtml, /删除.*2.*份报告/);
  assert.match(crashpadHtml, /删除.*4.*个文件/);
  assert.match(crashpadHtml, /保留近期写入报告：1 份/);
  assert.match(crashpadHtml, /保留未知文件：2 个/);
  assert.doesNotMatch(crashpadHtml, /Trace 日志/);

  // Error alert
  const errorHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, { result: { ...result, errors: ["数据库正由另一个进程锁定"] }, target: "trace" }));
  assert.match(errorHtml, /未完成项/);
  assert.match(errorHtml, /数据库正由另一个进程锁定/);

  const failedHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, {
    result: { ...result, traceCleanup: null, traceLogStats: { capturedAt: 0 }, errors: ["数据库忙"] }, target: "trace",
  }));
  assert.match(failedHtml, /未能统计/);
  assert.match(failedHtml, /清理未完成/);
  assert.doesNotMatch(failedHtml, /无需额外释放/);
  const emptyHtml = renderToStaticMarkup(React.createElement(DiagnosticCleanupNotice, {
    result: { ...result, traceCleanup: { databasesCleaned: 0, rowsDeleted: 0, bytesBefore: 0, bytesReclaimed: 0 }, traceLogStats: { capturedAt: 1, databaseBytes: 0 } }, target: "trace",
  }));
  assert.match(emptyHtml, /删除.*0.*条记录/);
  assert.match(emptyHtml, /0 B/);
});
