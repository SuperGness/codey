import { execFile } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);
import { validateReleaseVersion } from "./release-policy.mjs";
export { validateReleaseVersion } from "./release-policy.mjs";

function section(text, name) {
  const headers = [...text.matchAll(/^\s*(\[\[?[^\]\r\n]+\]\]?)\s*(?:#[^\r\n]*)?$/gm)];
  const matches = headers.filter((header) => header[1] === `[${name}]`);
  if (matches.length !== 1) throw new Error(`无法确定 Cargo.toml 的 ${name} 配置`);
  const header = matches[0];
  const following = headers[headers.indexOf(header) + 1];
  return text.slice(header.index + header[0].length, following?.index ?? text.length);
}

function stringValue(text, key) {
  const values = [...text.matchAll(new RegExp(`^\\s*${key}\\s*=\\s*"([^"\\r\\n]+)"\\s*(?:#[^\\r\\n]*)?$`, "gm"))];
  if (values.length !== 1) throw new Error(`无法确定 Cargo 配置中的 ${key}`);
  return values[0][1];
}

export function validateSourceVersions(packageText, cargoText) {
  const version = validateReleaseVersion(JSON.parse(packageText).version);
  const cargoVersion = stringValue(section(cargoText, "workspace.package"), "version");
  if (version !== cargoVersion) throw new Error("源码 package.json 与 Cargo.toml 版本必须一致且符合 SemVer");
  return version;
}

export async function readSourceVersion(root = process.cwd()) {
  const [packageText, cargoText] = await Promise.all(["package.json", "Cargo.toml"].map((path) => readFile(join(root, path), "utf8")));
  return validateSourceVersions(packageText, cargoText);
}

function replaceVersion(text, oldVersion, version) {
  const pattern = /^(\s*version\s*=\s*")[^"\r\n]+("\s*(?:#[^\r\n]*)?)$/m;
  if (stringValue(text, "version") !== oldVersion) throw new Error("Cargo.lock 本地包版本与源码版本不一致");
  return text.replace(pattern, (_, prefix, suffix) => `${prefix}${version}${suffix}`);
}

function workspaceMembers(cargoText, root) {
  const workspace = section(cargoText, "workspace");
  const match = workspace.match(/^\s*members\s*=\s*\[([^\]]*)\]/m);
  if (!match) throw new Error("无法确定 Cargo workspace 成员");
  const contents = match[1].replace(/#[^\r\n]*/g, "");
  const tokens = [...contents.matchAll(/"([^"\\\r\n]+)"/g)];
  if (!tokens.length || contents.replace(/"([^"\\\r\n]+)"/g, "").replace(/[,\s]/g, "")) throw new Error("Cargo workspace 成员必须为明确的目录路径");
  return tokens.map((token) => {
    const member = token[1];
    const path = resolve(root, member, "Cargo.toml");
    if (/[\*?\[\]\\]/.test(member) || !path.startsWith(`${resolve(root)}${sep}`)) throw new Error("Cargo workspace 成员路径无效");
    return relative(root, path).split(sep).join("/");
  });
}

function updateLock(lock, packages, sourceVersion, version) {
  const found = new Set();
  const next = lock.replace(/^\[\[package\]\][\s\S]*?(?=^\[\[package\]\]|$(?![\s\S]))/gm, (entry) => {
    const name = stringValue(entry, "name");
    if (!packages.has(name) || /^\s*(?:source|checksum)\s*=/m.test(entry)) return entry;
    if (found.has(name)) throw new Error(`Cargo.lock 本地包重复：${name}`);
    found.add(name);
    return replaceVersion(entry, sourceVersion, version);
  });
  if (found.size !== packages.size) throw new Error("Cargo.lock 缺少继承 workspace 版本的本地包");
  return next.replace(/^\s*"([^"\r\n]+)"(?=,?\s*$)/gm, (reference, value) => {
    const match = value.match(/^(\S+) (\S+)$/);
    if (!match || !packages.has(match[1]) || match[2] !== sourceVersion) return reference;
    return reference.replace(value, `${match[1]} ${version}`);
  });
}

export async function prepareReleaseVersion(build, root = process.cwd()) {
  const version = validateReleaseVersion(build.version);
  if (!/^[a-f0-9]{40}$/.test(build.source_sha || "")) throw new Error("源码提交参数无效");
  const git = async (args) => (await execute("git", args, { cwd: root, maxBuffer: 4 * 1024 * 1024 })).stdout;
  if ((await git(["rev-parse", "HEAD"])).trim() !== build.source_sha) throw new Error("当前 git HEAD 与固定源码提交不一致");
  const source = async (path) => git(["show", `${build.source_sha}:${path}`]);
  const [packageText, cargoText, lockText] = await Promise.all(["package.json", "Cargo.toml", "Cargo.lock"].map(source));
  const sourceVersion = validateSourceVersions(packageText, cargoText);
  const packages = new Set();
  for (const path of workspaceMembers(cargoText, root)) {
    const member = section(await source(path), "package");
    if (/^\s*version\.workspace\s*=\s*true\s*(?:#[^\r\n]*)?$/m.test(member)) packages.add(stringValue(member, "name"));
  }
  if (!packages.size) throw new Error("Cargo workspace 没有继承版本的本地包");
  const packageJson = JSON.parse(packageText);
  packageJson.version = version;
  const workspacePackage = section(cargoText, "workspace.package");
  const nextCargo = cargoText.replace(workspacePackage, replaceVersion(workspacePackage, sourceVersion, version));
  const planned = [
    { path: "package.json", original: packageText, next: version === sourceVersion ? packageText : `${JSON.stringify(packageJson, null, 2)}\n` },
    { path: "Cargo.toml", original: cargoText, next: nextCargo },
    { path: "Cargo.lock", original: lockText, next: updateLock(lockText, packages, sourceVersion, version) },
  ];
  for (const file of planned) {
    file.current = await readFile(join(root, file.path), "utf8");
    const normalized = file.current.replace(/\r\n/g, "\n");
    if (normalized !== file.original.replace(/\r\n/g, "\n") && normalized !== file.next.replace(/\r\n/g, "\n")) throw new Error(`构建版本注入前 ${file.path} 已有其他修改`);
    if (file.current.includes("\r\n")) file.next = file.next.replace(/\r?\n/g, "\r\n");
  }
  for (const file of planned) if (file.current !== file.next) await writeFile(join(root, file.path), file.next);
  return { sourceVersion, version, packages: [...packages] };
}
