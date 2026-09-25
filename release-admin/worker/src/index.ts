import { grayTargetCount, isIdempotencyKey } from "./policy";

export interface Env {
  DB: D1Database;
  SESSION_SECRET: string;
  CORS_ORIGIN?: string;
  SESSION_TTL_SECONDS?: string;
}

interface D1Result<T = Record<string, unknown>> {
  results?: T[];
  success?: boolean;
  meta?: { changes?: number };
}
interface D1Statement {
  bind(...values: unknown[]): D1Statement;
  first<T = Record<string, unknown>>(column?: string): Promise<T | null>;
  all<T = Record<string, unknown>>(): Promise<D1Result<T>>;
  run(): Promise<D1Result>;
}
interface D1Database {
  prepare(query: string): D1Statement;
  batch(statements: D1Statement[]): Promise<D1Result[]>;
}

const JSON_HEADERS = { "content-type": "application/json; charset=utf-8" };
const SESSION_COOKIE = "release_admin_session";
const MAX_BODY_BYTES = 512 * 1024;
const VERSION_STATUSES = ["draft", "pending", "gray", "full", "paused", "withdrawn", "archived"] as const;
const PUBLISH_MODES = ["full", "gray", "targeted"] as const;
type Role = "admin" | "operator" | "viewer";
type PublishMode = (typeof PUBLISH_MODES)[number];

type Context = { env: Env; request: Request; admin?: Admin };
type Admin = { id: string; username: string; role: Role; disabled: number };

class HttpError extends Error {
  constructor(public status: number, message: string, public details?: unknown) { super(message); }
}

function now(): number { return Math.floor(Date.now() / 1000); }
function id(prefix: string): string { return `${prefix}_${crypto.randomUUID()}`; }
function b64(bytes: ArrayBuffer | Uint8Array): string {
  const data = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const byte of data) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}
function fromB64(input: string): Uint8Array {
  const value = input.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((input.length + 3) % 4);
  const binary = atob(value);
  return Uint8Array.from(binary, c => c.charCodeAt(0));
}
async function sha256(value: string): Promise<string> {
  return b64(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
}
async function passwordHash(password: string): Promise<string> {
  if (password.length < 12 || password.length > 256) throw new HttpError(400, "密码长度必须为 12 至 256 个字符");
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(password), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits({ name: "PBKDF2", salt, iterations: 120_000, hash: "SHA-256" }, key, 256);
  return `pbkdf2$120000$${b64(salt)}$${b64(bits)}`;
}
async function passwordVerify(password: string, stored: string): Promise<boolean> {
  const [kind, iterationsRaw, saltRaw, expected] = stored.split("$");
  const iterations = Number(iterationsRaw);
  if (kind !== "pbkdf2" || !Number.isInteger(iterations) || iterations < 100_000 || !saltRaw || !expected) return false;
  try {
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(password), "PBKDF2", false, ["deriveBits"]);
    const bits = await crypto.subtle.deriveBits({ name: "PBKDF2", salt: fromB64(saltRaw), iterations, hash: "SHA-256" }, key, 256);
    const actual = fromB64(b64(bits));
    const wanted = fromB64(expected);
    if (actual.length !== wanted.length) return false;
    let diff = 0;
    for (let i = 0; i < actual.length; i++) diff |= actual[i] ^ wanted[i];
    return diff === 0;
  } catch { return false; }
}
function response(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { ...JSON_HEADERS, ...headers } });
}
function errorResponse(error: unknown): Response {
  if (error instanceof HttpError) return response({ error: error.message, details: error.details }, error.status);
  console.error(error);
  return response({ error: "服务器内部错误" }, 500);
}
function requireRole(ctx: Context, roles: Role[]): Admin {
  if (!ctx.admin) throw new HttpError(401, "需要登录");
  if (!roles.includes(ctx.admin.role)) throw new HttpError(403, "权限不足");
  return ctx.admin;
}
function parseCookies(request: Request): Record<string, string> {
  const value = request.headers.get("cookie") ?? "";
  return Object.fromEntries(value.split(";").map(part => part.trim().split("=")).filter(pair => pair.length === 2).map(([k, v]) => [k, decodeURIComponent(v)]));
}
function cookie(name: string, value: string, maxAge: number): string {
  return `${name}=${encodeURIComponent(value)}; Max-Age=${maxAge}; Path=/; HttpOnly; Secure; SameSite=Lax`;
}
async function readJson(request: Request): Promise<Record<string, any>> {
  const length = Number(request.headers.get("content-length") ?? 0);
  if (length > MAX_BODY_BYTES) throw new HttpError(413, "请求体过大");
  const text = await request.text();
  if (new TextEncoder().encode(text).byteLength > MAX_BODY_BYTES) throw new HttpError(413, "请求体过大");
  try {
    const value = JSON.parse(text);
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
    return value;
  } catch { throw new HttpError(400, "请求体必须是 JSON 对象"); }
}
function requiredString(value: unknown, name: string, max = 500): string {
  if (typeof value !== "string" || value.trim().length === 0 || value.length > max) throw new HttpError(400, `${name}无效`);
  return value.trim();
}
function optionalString(value: unknown, max = 20_000): string | null {
  if (value == null || value === "") return null;
  if (typeof value !== "string" || value.length > max) throw new HttpError(400, "文本字段无效");
  return value;
}
function validateVersion(value: unknown): string {
  const version = requiredString(value, "版本号", 64);
  if (!/^v?\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) throw new HttpError(400, "版本号必须符合 semver 格式");
  return version.replace(/^v/, "");
}
function validateHttpsUrl(value: unknown, name: string): string | null {
  const url = optionalString(value, 2048);
  if (!url) return null;
  try { const parsed = new URL(url); if (parsed.protocol !== "https:") throw new Error(); } catch { throw new HttpError(400, `${name}必须使用 HTTPS URL`); }
  return url;
}
function parsePositiveInt(value: unknown, name: string, max: number): number | null {
  if (value == null || value === "") return null;
  const number = Number(value);
  if (!Number.isInteger(number) || number < 1 || number > max) throw new HttpError(400, `${name}无效`);
  return number;
}
function parsePagination(url: URL): { limit: number; offset: number } {
  const limit = Math.min(Math.max(Number(url.searchParams.get("limit") ?? 50), 1), 200);
  const offset = Math.max(Number(url.searchParams.get("offset") ?? 0), 0);
  if (!Number.isInteger(limit) || !Number.isInteger(offset)) throw new HttpError(400, "分页参数无效");
  return { limit, offset };
}

async function authenticate(request: Request, env: Env): Promise<Admin | undefined> {
  const token = parseCookies(request)[SESSION_COOKIE];
  if (!token) return undefined;
  const tokenHash = await sha256(`${env.SESSION_SECRET}:${token}`);
  const row = await env.DB.prepare(`SELECT a.id, a.username, a.role, a.disabled FROM sessions s JOIN admins a ON a.id=s.admin_id WHERE s.token_hash=? AND s.expires_at>? AND a.disabled=0`).bind(tokenHash, now()).first<Admin>();
  if (!row) return undefined;
  await env.DB.prepare("UPDATE sessions SET last_seen_at=? WHERE token_hash=?").bind(now(), tokenHash).run();
  return row;
}
async function audit(env: Env, adminId: string | null, action: string, entityType: string, entityId: string | null, metadata?: unknown): Promise<void> {
  await env.DB.prepare("INSERT INTO audit_logs (id,admin_id,action,entity_type,entity_id,metadata_json,created_at) VALUES (?,?,?,?,?,?,?)").bind(id("audit"), adminId, action, entityType, entityId, metadata == null ? null : JSON.stringify(metadata), now()).run();
}

async function enforceRateLimit(env: Env, scope: string, max: number, windowSeconds: number): Promise<void> {
  const timestamp = now();
  const cutoff = timestamp - windowSeconds;
  await env.DB.prepare(`INSERT INTO rate_limits (scope,count,window_started_at) VALUES (?,?,?)
    ON CONFLICT(scope) DO UPDATE SET
      count=CASE WHEN window_started_at < ? THEN 1 ELSE count+1 END,
      window_started_at=CASE WHEN window_started_at < ? THEN excluded.window_started_at ELSE window_started_at END`)
    .bind(scope, 1, timestamp, cutoff, cutoff).run();
  const row = await env.DB.prepare("SELECT count FROM rate_limits WHERE scope=?").bind(scope).first<any>();
  if (Number(row?.count ?? 0) > max) throw new HttpError(429, "请求过于频繁，请稍后重试");
}

function requestIp(request: Request): string {
  return (request.headers.get("cf-connecting-ip") ?? request.headers.get("x-forwarded-for")?.split(",")[0] ?? "unknown").trim().slice(0, 128) || "unknown";
}

function validateCsrf(request: Request, env: Env): void {
  const method = request.method.toUpperCase();
  if (!["POST", "PATCH", "PUT", "DELETE"].includes(method)) return;
  if (!parseCookies(request)[SESSION_COOKIE]) return;
  const origin = request.headers.get("origin");
  const expected = env.CORS_ORIGIN?.replace(/\/$/, "") || new URL(request.url).origin;
  if (!origin || origin.replace(/\/$/, "") !== expected) throw new HttpError(403, "请求来源未通过校验");
  const contentType = request.headers.get("content-type")?.split(";", 1)[0].trim().toLowerCase();
  if (contentType !== "application/json") throw new HttpError(415, "写操作必须使用 JSON 请求体");
}

async function login(ctx: Context): Promise<Response> {
  await enforceRateLimit(ctx.env, `login:${requestIp(ctx.request)}`, 10, 15 * 60);
  const body = await readJson(ctx.request);
  const username = requiredString(body.username, "用户名", 128);
  const password = requiredString(body.password, "密码", 256);
  const admin = await ctx.env.DB.prepare("SELECT id,username,password_hash,role,disabled FROM admins WHERE username=?").bind(username).first<any>();
  if (!admin || admin.disabled || !(await passwordVerify(password, admin.password_hash))) throw new HttpError(401, "用户名或密码错误");
  const token = b64(crypto.getRandomValues(new Uint8Array(32)));
  const tokenHash = await sha256(`${ctx.env.SESSION_SECRET}:${token}`);
  const ttl = Math.min(Math.max(Number(ctx.env.SESSION_TTL_SECONDS ?? 86_400), 900), 2_592_000);
  const timestamp = now();
  await ctx.env.DB.prepare("INSERT INTO sessions (id,admin_id,token_hash,expires_at,created_at,last_seen_at) VALUES (?,?,?,?,?,?)").bind(id("session"), admin.id, tokenHash, timestamp + ttl, timestamp, timestamp).run();
  await audit(ctx.env, admin.id, "login", "session", null);
  return response({ admin: { id: admin.id, username: admin.username, role: admin.role }, expiresAt: timestamp + ttl }, 200, { "set-cookie": cookie(SESSION_COOKIE, token, ttl) });
}
async function logout(ctx: Context): Promise<Response> {
  const token = parseCookies(ctx.request)[SESSION_COOKIE];
  if (token) await ctx.env.DB.prepare("DELETE FROM sessions WHERE token_hash=?").bind(await sha256(`${ctx.env.SESSION_SECRET}:${token}`)).run();
  if (ctx.admin) await audit(ctx.env, ctx.admin.id, "logout", "session", null);
  return response({ ok: true }, 200, { "set-cookie": cookie(SESSION_COOKIE, "", 0) });
}

async function createVersion(ctx: Context): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const body = await readJson(ctx.request);
  const version = validateVersion(body.version);
  const releaseNotes = optionalString(body.releaseNotes);
  const manifestUrl = validateHttpsUrl(body.manifestUrl, "清单地址");
  const artifactUrl = validateHttpsUrl(body.artifactUrl, "产物地址");
  const artifactSha256 = body.artifactSha256 == null ? null : requiredString(body.artifactSha256, "SHA-256", 128).toLowerCase();
  if (artifactSha256 && !/^[a-f0-9]{64}$/.test(artifactSha256)) throw new HttpError(400, "SHA-256 无效");
  const artifactSize = body.artifactSize == null ? null : parsePositiveInt(body.artifactSize, "产物大小", Number.MAX_SAFE_INTEGER);
  const timestamp = now();
  const versionId = id("ver");
  try {
    await ctx.env.DB.prepare("INSERT INTO versions (id,version,release_notes,manifest_url,artifact_url,artifact_sha256,artifact_size,status,created_by,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)").bind(versionId, version, releaseNotes, manifestUrl, artifactUrl, artifactSha256, artifactSize, "draft", admin.id, timestamp, timestamp).run();
  } catch (error) {
    if (String(error).toLowerCase().includes("unique")) throw new HttpError(409, "版本号已存在");
    throw error;
  }
  await audit(ctx.env, admin.id, "version.create", "version", versionId, { version });
  return response({ id: versionId, version, status: "draft", createdAt: timestamp });
}
async function listVersions(ctx: Context, url: URL): Promise<Response> {
  requireRole(ctx, ["admin", "operator", "viewer"]);
  const { limit, offset } = parsePagination(url);
  const status = url.searchParams.get("status");
  if (status && !(VERSION_STATUSES as readonly string[]).includes(status)) throw new HttpError(400, "状态无效");
  const query = status ? "SELECT v.*, a.username AS creator FROM versions v JOIN admins a ON a.id=v.created_by WHERE v.status=? ORDER BY v.created_at DESC LIMIT ? OFFSET ?" : "SELECT v.*, a.username AS creator FROM versions v JOIN admins a ON a.id=v.created_by ORDER BY v.created_at DESC LIMIT ? OFFSET ?";
  const result = status ? await ctx.env.DB.prepare(query).bind(status, limit, offset).all() : await ctx.env.DB.prepare(query).bind(limit, offset).all();
  return response({ items: result.results ?? [], limit, offset });
}
async function getVersion(ctx: Context, versionId: string): Promise<Response> {
  requireRole(ctx, ["admin", "operator", "viewer"]);
  const version = await ctx.env.DB.prepare("SELECT v.*, a.username AS creator FROM versions v JOIN admins a ON a.id=v.created_by WHERE v.id=?").bind(versionId).first();
  if (!version) throw new HttpError(404, "版本不存在");
  const publishes = await ctx.env.DB.prepare("SELECT * FROM publish_batches WHERE version_id=? ORDER BY created_at DESC").bind(versionId).all();
  return response({ ...version, publishes: publishes.results ?? [] });
}
async function updateVersion(ctx: Context, versionId: string): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const current = await ctx.env.DB.prepare("SELECT * FROM versions WHERE id=?").bind(versionId).first<any>();
  if (!current) throw new HttpError(404, "版本不存在");
  if (["full", "gray", "paused"].includes(current.status)) throw new HttpError(409, "正在发布的版本不可编辑");
  const body = await readJson(ctx.request);
  const fields: Record<string, unknown> = {};
  if (body.version !== undefined) fields.version = validateVersion(body.version);
  if (body.releaseNotes !== undefined) fields.release_notes = optionalString(body.releaseNotes);
  if (body.manifestUrl !== undefined) fields.manifest_url = validateHttpsUrl(body.manifestUrl, "清单地址");
  if (body.artifactUrl !== undefined) fields.artifact_url = validateHttpsUrl(body.artifactUrl, "产物地址");
  if (body.artifactSha256 !== undefined) {
    const value = body.artifactSha256 == null ? null : requiredString(body.artifactSha256, "SHA-256", 128).toLowerCase();
    if (value && !/^[a-f0-9]{64}$/.test(value)) throw new HttpError(400, "SHA-256 无效");
    fields.artifact_sha256 = value;
  }
  if (body.artifactSize !== undefined) fields.artifact_size = body.artifactSize == null ? null : parsePositiveInt(body.artifactSize, "产物大小", Number.MAX_SAFE_INTEGER);
  if (!Object.keys(fields).length) throw new HttpError(400, "没有可更新字段");
  const timestamp = now();
  const assignments = Object.keys(fields).map(key => `${key}=?`).join(",");
  try {
    await ctx.env.DB.prepare(`UPDATE versions SET ${assignments}, updated_at=? WHERE id=?`).bind(...Object.values(fields), timestamp, versionId).run();
  } catch (error) { if (String(error).toLowerCase().includes("unique")) throw new HttpError(409, "版本号已存在"); throw error; }
  await audit(ctx.env, admin.id, "version.update", "version", versionId, { fields: Object.keys(fields) });
  return getVersion(ctx, versionId);
}
async function archiveVersion(ctx: Context, versionId: string): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const current = await ctx.env.DB.prepare("SELECT status,active_publish_id FROM versions WHERE id=?").bind(versionId).first<any>();
  if (!current) throw new HttpError(404, "版本不存在");
  if (current.active_publish_id || current.status === "paused") throw new HttpError(409, "正在发布的版本不可归档");
  await ctx.env.DB.prepare("UPDATE versions SET status='archived',updated_at=? WHERE id=? AND active_publish_id IS NULL").bind(now(), versionId).run();
  await audit(ctx.env, admin.id, "version.archive", "version", versionId);
  return response({ ok: true, status: "archived" });
}

async function resolveTargets(ctx: Context, mode: PublishMode, body: Record<string, any>): Promise<string[]> {
  if (mode === "targeted") {
    if (!Array.isArray(body.machineNos) || body.machineNos.length < 1 || body.machineNos.length > 500) throw new HttpError(400, "定向发布需要 1 至 500 个机器号");
    const machineNos = [...new Set(body.machineNos.map(value => requiredString(value, "机器号", 128)))];
    const placeholders = machineNos.map(() => "?").join(",");
    const rows = await ctx.env.DB.prepare(`SELECT id,machine_no FROM devices WHERE disabled=0 AND machine_no IN (${placeholders})`).bind(...machineNos).all<any>();
    const found = new Set((rows.results ?? []).map(row => row.machine_no));
    const missing = machineNos.filter(machineNo => !found.has(machineNo));
    if (missing.length) throw new HttpError(404, "存在无效或已停用的机器号", { missing });
    return (rows.results ?? []).map(row => row.id);
  }
  const all = await ctx.env.DB.prepare(`SELECT id FROM devices WHERE disabled=0${mode === "gray" ? " AND whitelisted=1" : ""} ORDER BY id`).all<any>();
  let devices = (all.results ?? []).map(row => row.id);
  if (mode === "gray") {
    const percentage = body.percentage == null ? null : parsePositiveInt(body.percentage, "灰度比例", 100);
    const deviceLimit = body.deviceLimit == null ? null : parsePositiveInt(body.deviceLimit, "灰度数量", 100_000_000);
    if (percentage == null && deviceLimit == null && body.whitelistOnly !== true) throw new HttpError(400, "灰度发布需要白名单、比例或设备数量");
    if (percentage != null && deviceLimit != null) throw new HttpError(400, "比例和设备数量只能选择一个");
    const count = grayTargetCount(devices.length, percentage, deviceLimit, body.whitelistOnly === true);
    devices = devices.slice(0, Math.min(count, devices.length));
  }
  return devices;
}
async function createPublish(ctx: Context, versionId: string): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const body = await readJson(ctx.request);
  const mode = requiredString(body.mode, "推送方式", 20) as PublishMode;
  if (!PUBLISH_MODES.includes(mode)) throw new HttpError(400, "推送方式无效");
  const key = requiredString(ctx.request.headers.get("idempotency-key"), "Idempotency-Key", 200);
  if (!isIdempotencyKey(key)) throw new HttpError(400, "Idempotency-Key 格式无效");
  const existing = await ctx.env.DB.prepare("SELECT * FROM publish_batches WHERE requested_by=? AND idempotency_key=?").bind(admin.id, key).first();
  const requestHash = await sha256(JSON.stringify({ mode, percentage: body.percentage ?? null, deviceLimit: body.deviceLimit ?? null, whitelistOnly: body.whitelistOnly === true, machineNos: Array.isArray(body.machineNos) ? [...new Set(body.machineNos)].sort() : [], scheduledAt: body.scheduledAt ?? null, notes: body.notes ?? null }));
  if (existing) {
    if (existing.request_hash && existing.request_hash !== requestHash) throw new HttpError(409, "幂等键已用于另一组发布参数");
    return response(existing);
  }
  const version = await ctx.env.DB.prepare("SELECT * FROM versions WHERE id=?").bind(versionId).first<any>();
  if (!version) throw new HttpError(404, "版本不存在");
  if (!version.manifest_url) throw new HttpError(409, "发布前必须配置 HTTPS 更新清单地址");
  if (version.active_publish_id || version.status === "paused") throw new HttpError(409, "该版本已有进行中的发布");
  const targetIds = await resolveTargets(ctx, mode, body);
  if (!targetIds.length) throw new HttpError(409, "没有符合条件的目标设备");
  const batchId = id("pub");
  const timestamp = now();
  const publishStatus = "scheduled";
  const versionStatus = mode === "gray" ? "gray" : mode === "full" ? "full" : "pending";
  const percentage = mode === "gray" && body.percentage != null ? parsePositiveInt(body.percentage, "灰度比例", 100) : null;
  const deviceLimit = mode === "gray" && body.deviceLimit != null ? parsePositiveInt(body.deviceLimit, "灰度数量", 100_000_000) : null;
  const scheduledAt = body.scheduledAt == null ? timestamp : Number(body.scheduledAt);
  if (!Number.isInteger(scheduledAt) || scheduledAt < timestamp - 60) throw new HttpError(400, "发布时间无效");
  const statements: D1Statement[] = [
    ctx.env.DB.prepare(`UPDATE versions SET status=?,active_publish_id=?,updated_at=? WHERE id=? AND active_publish_id IS NULL AND status <> 'paused'`).bind(versionStatus, batchId, timestamp, versionId),
    ctx.env.DB.prepare("INSERT INTO publish_batches (id,version_id,mode,status,percentage,device_limit,scheduled_at,notes,requested_by,idempotency_key,request_hash,target_count,created_at,updated_at) SELECT ?,?,?,?,?,?,?,?,?,?,?,?, ?,? WHERE (SELECT active_publish_id FROM versions WHERE id=?)=?").bind(batchId, versionId, mode, publishStatus, percentage, deviceLimit, scheduledAt, optionalString(body.notes, 4000), admin.id, key, requestHash, targetIds.length, timestamp, timestamp, versionId, batchId),
  ];
  for (const deviceId of targetIds) statements.push(ctx.env.DB.prepare("INSERT INTO publish_targets (batch_id,device_id,status,updated_at) SELECT ?,?,?,? WHERE EXISTS (SELECT 1 FROM publish_batches WHERE id=?)").bind(batchId, deviceId, "pending", timestamp, batchId));
  await ctx.env.DB.batch(statements);
  const check = await ctx.env.DB.prepare("SELECT active_publish_id FROM versions WHERE id=?").bind(versionId).first<any>();
  const created = await ctx.env.DB.prepare("SELECT id FROM publish_batches WHERE id=?").bind(batchId).first<any>();
  if (check?.active_publish_id !== batchId || !created) throw new HttpError(409, "版本发布状态已被其他操作占用");
  await audit(ctx.env, admin.id, "publish.create", "publish", batchId, { versionId, mode, targetCount: targetIds.length });
  return response({ id: batchId, versionId, mode, status: publishStatus, targetCount: targetIds.length }, 201);
}
async function listPublishes(ctx: Context, url: URL): Promise<Response> {
  requireRole(ctx, ["admin", "operator", "viewer"]);
  const { limit, offset } = parsePagination(url);
  const result = await ctx.env.DB.prepare("SELECT p.*,v.version FROM publish_batches p JOIN versions v ON v.id=p.version_id ORDER BY p.created_at DESC LIMIT ? OFFSET ?").bind(limit, offset).all();
  return response({ items: result.results ?? [], limit, offset });
}
async function mutatePublish(ctx: Context, batchId: string, action: "pause" | "resume" | "withdraw"): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const batch = await ctx.env.DB.prepare("SELECT * FROM publish_batches WHERE id=?").bind(batchId).first<any>();
  if (!batch) throw new HttpError(404, "发布批次不存在");
  const timestamp = now();
  if (action === "pause") {
    if (!["scheduled", "running"].includes(batch.status)) throw new HttpError(409, "当前状态不可暂停");
    await ctx.env.DB.batch([
      ctx.env.DB.prepare("UPDATE publish_batches SET status='paused',updated_at=? WHERE id=? AND status IN ('scheduled','running')").bind(timestamp, batchId),
      ctx.env.DB.prepare("UPDATE versions SET status='paused',updated_at=? WHERE active_publish_id=?").bind(timestamp, batchId),
    ]);
  } else if (action === "resume") {
    if (batch.status !== "paused") throw new HttpError(409, "当前状态不可恢复");
    const nextVersionStatus = batch.mode === "gray" ? "gray" : batch.mode === "full" ? "full" : "pending";
    await ctx.env.DB.batch([
      ctx.env.DB.prepare("UPDATE publish_batches SET status='running',updated_at=? WHERE id=? AND status='paused'").bind(timestamp, batchId),
      ctx.env.DB.prepare("UPDATE versions SET status=?,updated_at=? WHERE active_publish_id=?").bind(nextVersionStatus, timestamp, batchId),
    ]);
  } else {
    if (!["scheduled", "running", "paused"].includes(batch.status)) throw new HttpError(409, "当前状态不可撤回");
    await ctx.env.DB.batch([
      ctx.env.DB.prepare("UPDATE publish_batches SET status='withdrawn',updated_at=? WHERE id=? AND status IN ('scheduled','running','paused')").bind(timestamp, batchId),
      ctx.env.DB.prepare("UPDATE publish_targets SET status='withdrawn',updated_at=? WHERE batch_id=? AND status IN ('pending','offered')").bind(timestamp, batchId),
      ctx.env.DB.prepare("UPDATE versions SET status='withdrawn',active_publish_id=NULL,updated_at=? WHERE active_publish_id=?").bind(timestamp, batchId),
    ]);
  }
  await audit(ctx.env, admin.id, `publish.${action}`, "publish", batchId);
  return response({ ok: true, action });
}

async function registerDevice(ctx: Context): Promise<Response> {
  await enforceRateLimit(ctx.env, `device-register:${requestIp(ctx.request)}`, 20, 60 * 60);
  const body = await readJson(ctx.request);
  const installKey = requiredString(body.installKey, "设备注册密钥", 512);
  const installHash = await sha256(`${ctx.env.SESSION_SECRET}:install:${installKey}`);
  const existing = await ctx.env.DB.prepare("SELECT id,machine_no FROM devices WHERE install_key_hash=?").bind(installHash).first<any>();
  if (existing) return response({ deviceId: existing.id, machineNo: existing.machine_no });
  const deviceId = id("dev");
  const machineNo = `m_${b64(crypto.getRandomValues(new Uint8Array(18)))}`;
  const timestamp = now();
  try {
    await ctx.env.DB.prepare("INSERT INTO devices (id,machine_no,install_key_hash,user_ref,platform,arch,current_version,whitelisted,created_at,updated_at,last_seen_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)").bind(deviceId, machineNo, installHash, optionalString(body.userRef, 256), optionalString(body.platform, 64), optionalString(body.arch, 64), optionalString(body.currentVersion, 64), 0, timestamp, timestamp, timestamp).run();
  } catch (error) { if (String(error).toLowerCase().includes("unique")) throw new HttpError(409, "设备已注册，请重试"); throw error; }
  return response({ deviceId, machineNo }, 201);
}

async function authenticateDevice(ctx: Context, machineNo?: string): Promise<any> {
  const installKey = requiredString(ctx.request.headers.get("x-device-key"), "设备认证信息", 512);
  const installHash = await sha256(`${ctx.env.SESSION_SECRET}:install:${installKey}`);
  const device = await ctx.env.DB.prepare("SELECT * FROM devices WHERE install_key_hash=? AND disabled=0").bind(installHash).first<any>();
  if (!device) throw new HttpError(401, "设备认证失败");
  if (machineNo && machineNo !== device.machine_no) throw new HttpError(403, "机器号与设备认证信息不匹配");
  return device;
}

async function listDevices(ctx: Context, url: URL): Promise<Response> {
  requireRole(ctx, ["admin", "operator", "viewer"]);
  const { limit, offset } = parsePagination(url);
  const status = url.searchParams.get("status");
  const query = status === "disabled"
    ? "SELECT id,machine_no,user_ref,platform,arch,current_version,last_seen_at,created_at,updated_at,disabled,whitelisted FROM devices WHERE disabled=1 ORDER BY created_at DESC LIMIT ? OFFSET ?"
    : status === "active"
      ? "SELECT id,machine_no,user_ref,platform,arch,current_version,last_seen_at,created_at,updated_at,disabled,whitelisted FROM devices WHERE disabled=0 ORDER BY created_at DESC LIMIT ? OFFSET ?"
      : "SELECT id,machine_no,user_ref,platform,arch,current_version,last_seen_at,created_at,updated_at,disabled,whitelisted FROM devices ORDER BY created_at DESC LIMIT ? OFFSET ?";
  const result = await ctx.env.DB.prepare(query).bind(limit, offset).all();
  return response({ items: result.results ?? [], limit, offset });
}

async function updateDevice(ctx: Context, deviceId: string): Promise<Response> {
  const admin = requireRole(ctx, ["admin", "operator"]);
  const current = await ctx.env.DB.prepare("SELECT id FROM devices WHERE id=?").bind(deviceId).first();
  if (!current) throw new HttpError(404, "设备不存在");
  const body = await readJson(ctx.request);
  const fields: Record<string, number> = {};
  if (body.whitelisted !== undefined) {
    if (typeof body.whitelisted !== "boolean") throw new HttpError(400, "白名单状态无效");
    fields.whitelisted = body.whitelisted ? 1 : 0;
  }
  if (body.disabled !== undefined) {
    if (typeof body.disabled !== "boolean") throw new HttpError(400, "设备状态无效");
    fields.disabled = body.disabled ? 1 : 0;
  }
  if (!Object.keys(fields).length) throw new HttpError(400, "没有可更新字段");
  const assignments = Object.keys(fields).map(key => `${key}=?`).join(",");
  await ctx.env.DB.prepare(`UPDATE devices SET ${assignments},updated_at=? WHERE id=?`).bind(...Object.values(fields), now(), deviceId).run();
  await audit(ctx.env, admin.id, "device.update", "device", deviceId, fields);
  return response({ ok: true });
}
async function checkUpdate(ctx: Context, url: URL): Promise<Response> {
  const machineNo = url.searchParams.get("machineNo") ?? ctx.request.headers.get("x-machine-no") ?? undefined;
  const device = await authenticateDevice(ctx, machineNo);
  const timestamp = now();
  await ctx.env.DB.prepare("UPDATE devices SET last_seen_at=?,updated_at=? WHERE id=?").bind(timestamp, timestamp, device.id).run();
  const candidate = await ctx.env.DB.prepare(`SELECT p.id AS publish_id,p.mode,p.status,p.notes,v.id AS version_id,v.version,v.release_notes,v.manifest_url,v.artifact_url,v.artifact_sha256,v.artifact_size,pt.status AS target_status FROM publish_batches p JOIN versions v ON v.id=p.version_id JOIN publish_targets pt ON pt.batch_id=p.id AND pt.device_id=? AND pt.status IN ('pending','offered') WHERE p.status IN ('scheduled','running') AND (p.scheduled_at IS NULL OR p.scheduled_at<=?) ORDER BY v.created_at DESC LIMIT 1`).bind(device.id, timestamp).first<any>();
  if (!candidate) return response({ updateAvailable: false, currentVersion: device.current_version });
  if (device.current_version && device.current_version === candidate.version) {
    await ctx.env.DB.prepare("UPDATE publish_targets SET status='installed',installed_at=COALESCE(installed_at,?),updated_at=? WHERE batch_id=? AND device_id=? AND status IN ('pending','offered')").bind(timestamp, timestamp, candidate.publish_id, device.id).run();
    await ctx.env.DB.prepare("UPDATE publish_batches SET success_count=(SELECT COUNT(*) FROM publish_targets WHERE batch_id=? AND status IN ('downloaded','installed')),failure_count=(SELECT COUNT(*) FROM publish_targets WHERE batch_id=? AND status='failed'),updated_at=? WHERE id=?").bind(candidate.publish_id, candidate.publish_id, timestamp, candidate.publish_id).run();
    return response({ updateAvailable: false, currentVersion: device.current_version, publishId: candidate.publish_id });
  }
  await ctx.env.DB.prepare("UPDATE publish_targets SET status='offered',offered_at=COALESCE(offered_at,?),updated_at=? WHERE batch_id=? AND device_id=? AND status='pending'").bind(timestamp, timestamp, candidate.publish_id, device.id).run();
  return response({ updateAvailable: true, currentVersion: device.current_version, ...candidate, releaseNotes: candidate.release_notes ?? null });
}
async function reportDevice(ctx: Context, batchId: string): Promise<Response> {
  const body = await readJson(ctx.request);
  const machineNo = body.machineNo ?? ctx.request.headers.get("x-machine-no") ?? undefined;
  const status = requiredString(body.status, "状态", 32);
  if (!["downloaded", "installed", "failed"].includes(status)) throw new HttpError(400, "设备状态无效");
  const device = await authenticateDevice(ctx, machineNo);
  const timestamp = now();
  const updates = status === "downloaded" ? "status='downloaded',downloaded_at=COALESCE(downloaded_at,?),updated_at=?" : status === "installed" ? "status='installed',installed_at=COALESCE(installed_at,?),updated_at=?" : "status='failed',error_code=?,error_message=?,updated_at=?";
  const allowed = status === "downloaded" ? "('pending','offered','withdrawn')" : status === "installed" ? "('pending','offered','downloaded','withdrawn')" : "('pending','offered','downloaded','withdrawn')";
  const statement = status === "failed" ? ctx.env.DB.prepare(`UPDATE publish_targets SET ${updates} WHERE batch_id=? AND device_id=? AND status IN ${allowed}`).bind(optionalString(body.errorCode, 128), optionalString(body.errorMessage, 1000), timestamp, batchId, device.id) : ctx.env.DB.prepare(`UPDATE publish_targets SET ${updates} WHERE batch_id=? AND device_id=? AND status IN ${allowed}`).bind(timestamp, timestamp, batchId, device.id);
  const result = await statement.run();
  if (!(result.meta?.changes ?? 0)) throw new HttpError(409, "设备状态无法更新");
  if (status === "installed") await ctx.env.DB.prepare("UPDATE devices SET current_version=(SELECT v.version FROM publish_batches p JOIN versions v ON v.id=p.version_id WHERE p.id=?),updated_at=? WHERE id=?").bind(batchId, timestamp, device.id).run();
  await ctx.env.DB.prepare("UPDATE publish_batches SET success_count=(SELECT COUNT(*) FROM publish_targets WHERE batch_id=? AND status IN ('downloaded','installed')),failure_count=(SELECT COUNT(*) FROM publish_targets WHERE batch_id=? AND status='failed'),updated_at=? WHERE id=?").bind(batchId, batchId, timestamp, batchId).run();
  const remaining = await ctx.env.DB.prepare("SELECT COUNT(*) AS count FROM publish_targets WHERE batch_id=? AND status IN ('pending','offered')").bind(batchId).first<any>();
  if (Number(remaining?.count ?? 0) === 0) {
    const summary = await ctx.env.DB.prepare("SELECT p.mode,p.failure_count,v.id AS version_id FROM publish_batches p JOIN versions v ON v.id=p.version_id WHERE p.id=?").bind(batchId).first<any>();
    if (summary) {
      const finalBatchStatus = Number(summary.failure_count ?? 0) > 0 ? 'failed' : 'completed';
      const finalVersionStatus = summary.mode === 'gray' ? 'gray' : summary.mode === 'full' ? 'full' : 'pending';
      await ctx.env.DB.batch([
        ctx.env.DB.prepare("UPDATE publish_batches SET status=?,updated_at=? WHERE id=? AND status IN ('scheduled','running')").bind(finalBatchStatus, timestamp, batchId),
        ctx.env.DB.prepare("UPDATE versions SET status=?,active_publish_id=NULL,updated_at=? WHERE active_publish_id=?").bind(finalVersionStatus, timestamp, batchId),
      ]);
    }
  }
  return response({ ok: true });
}

async function handle(ctx: Context): Promise<Response> {
  const url = new URL(ctx.request.url);
  const method = ctx.request.method.toUpperCase();
  const path = url.pathname.replace(/\/+$/, "") || "/";
  if (method === "OPTIONS") return new Response(null, { status: 204 });
  if (path === "/health") return response({ ok: true, service: "release-admin" });
  if (path === "/api/auth/login" && method === "POST") return login(ctx);
  if (path === "/api/auth/logout" && method === "POST") return logout(ctx);
  if (path === "/api/devices/register" && method === "POST") return registerDevice(ctx);
  if (path === "/api/devices" && method === "GET") return listDevices(ctx, url);
  const deviceMatch = path.match(/^\/api\/devices\/([^/]+)$/);
  if (deviceMatch && method === "PATCH") return updateDevice(ctx, decodeURIComponent(deviceMatch[1]));
  if (path === "/api/updates/check" && method === "GET") return checkUpdate(ctx, url);
  const versionMatch = path.match(/^\/api\/versions\/([^/]+)(?:\/(archive|publishes))?$/);
  if (path === "/api/versions" && method === "GET") return listVersions(ctx, url);
  if (path === "/api/versions" && method === "POST") return createVersion(ctx);
  if (versionMatch) {
    const versionId = decodeURIComponent(versionMatch[1]);
    if (versionMatch[2] === "archive" && method === "POST") return archiveVersion(ctx, versionId);
    if (versionMatch[2] === "publishes" && method === "POST") return createPublish(ctx, versionId);
    if (!versionMatch[2] && method === "GET") return getVersion(ctx, versionId);
    if (!versionMatch[2] && method === "PATCH") return updateVersion(ctx, versionId);
  }
  if (path === "/api/publishes" && method === "GET") return listPublishes(ctx, url);
  const publishMatch = path.match(/^\/api\/publishes\/([^/]+)(?:\/(pause|resume|withdraw|report))?$/);
  if (publishMatch) {
    const batchId = decodeURIComponent(publishMatch[1]);
    if (publishMatch[2] === "report" && method === "POST") return reportDevice(ctx, batchId);
    if (publishMatch[2] && method === "POST") return mutatePublish(ctx, batchId, publishMatch[2] as "pause" | "resume" | "withdraw");
  }
  if (path === "/api/me" && method === "GET") { const admin = requireRole(ctx, ["admin", "operator", "viewer"]); return response({ id: admin.id, username: admin.username, role: admin.role }); }
  throw new HttpError(404, "接口不存在");
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const origin = env.CORS_ORIGIN;
    const headers: HeadersInit = { "cache-control": "no-store", "x-content-type-options": "nosniff", "referrer-policy": "no-referrer" };
    if (origin) Object.assign(headers, { "access-control-allow-origin": origin, "access-control-allow-credentials": "true", "access-control-allow-headers": "content-type,idempotency-key,x-machine-no,x-device-key", "access-control-allow-methods": "GET,POST,PATCH,OPTIONS" });
    let result: Response;
    try {
      validateCsrf(request, env);
      const ctx: Context = { request, env, admin: await authenticate(request, env) };
      result = await handle(ctx);
    } catch (error) { result = errorResponse(error); }
    Object.entries(headers).forEach(([key, value]) => result.headers.set(key, value));
    return result;
  },
};

export { passwordHash };
