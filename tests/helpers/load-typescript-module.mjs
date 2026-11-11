import { access, readFile } from "node:fs/promises";
import ts from "typescript";

const compiledModuleCache = new Map();
const resolvedRelativeCache = new Map();

export async function loadTypeScriptModule(url) {
  return import(await compileModule(url, new Set()));
}

async function compileModule(url, ancestors) {
  const key = url.href;
  if (ancestors.has(key)) throw new Error(`测试模块存在循环导入：${key}`);
  if (compiledModuleCache.has(key)) return compiledModuleCache.get(key);
  const pending = compileSource(url, new Set([...ancestors, key])).catch((error) => {
    compiledModuleCache.delete(key);
    throw error;
  });
  compiledModuleCache.set(key, pending);
  return pending;
}

async function resolveRelativeModule(specifier, parent) {
  const cacheKey = `${parent.href}\0${specifier}`;
  if (resolvedRelativeCache.has(cacheKey)) return resolvedRelativeCache.get(cacheKey);
  const base = new URL(specifier, parent);
  const candidates = /\.[cm]?[jt]sx?$/.test(base.pathname)
    ? [base]
    : [base, ...[".ts", ".tsx", ".js", ".mjs", "/index.ts", "/index.tsx"].map(suffix => new URL(base.href + suffix))];
  for (const candidate of candidates) {
    try {
      await access(candidate);
      resolvedRelativeCache.set(cacheKey, candidate);
      return candidate;
    } catch (error) {
      if (error.code !== "ENOENT" && error.code !== "EISDIR") throw error;
    }
  }
  throw new Error(`无法解析测试模块导入：${specifier}（${parent.href}）`);
}

async function compileSource(url, ancestors) {
  const source = await readFile(url, "utf8");
  let compiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2020,
    },
  }).outputText;
  const parsed = ts.createSourceFile(url.pathname, compiled, ts.ScriptTarget.ES2020, true, ts.ScriptKind.JS);
  const replacements = [];
  for (const statement of parsed.statements) {
    if (!ts.isImportDeclaration(statement) && !ts.isExportDeclaration(statement)) continue;
    const specifier = statement.moduleSpecifier;
    if (!specifier || !ts.isStringLiteral(specifier) || !specifier.text.startsWith(".")) continue;
    const dependency = await resolveRelativeModule(specifier.text, url);
    const moduleUrl = await compileModule(dependency, ancestors);
    replacements.push({ start: specifier.getStart(parsed), end: specifier.end, text: JSON.stringify(moduleUrl) });
  }
  for (const replacement of replacements.reverse()) {
    compiled = compiled.slice(0, replacement.start) + replacement.text + compiled.slice(replacement.end);
  }
  return `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`;
}
