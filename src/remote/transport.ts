export class RemoteError extends Error {
  constructor(message: string, readonly status: number) { super(message); }
}

export async function remoteRequest<T>(path: string, args?: unknown): Promise<T> {
  const response = await fetch(path, {
    method: args === undefined ? "GET" : "POST",
    credentials: "same-origin",
    headers: args === undefined ? undefined : { "Content-Type": "application/json" },
    body: args === undefined ? undefined : JSON.stringify(args),
    signal: AbortSignal.timeout(90_000),
  });
  const result = await response.json() as { status?: string; message?: string };
  if (!response.ok || result?.status === "failed") throw new RemoteError(result?.message || `请求失败 (${response.status})`, response.status);
  return result as T;
}

// randomUUID is unavailable on plain HTTP LAN origins; getRandomValues is
// available there and still provides cryptographically random request IDs.
export function requestId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export function pairingCode(hash: string): string {
  const code = new URLSearchParams(hash.replace(/^#/, "")).get("pair") || "";
  return /^[a-f0-9]{64}$/.test(code) ? code : "";
}

type Submission = { id: string; args: Record<string, unknown> };
const pending = new Map<string, Submission>();
const storageKey = (scope: string) => `codey-remote-pending:${scope}`;

export function readSubmission(scope: string): Submission | null {
  const current = pending.get(scope);
  if (current) return current;
  try {
    const value = JSON.parse(sessionStorage.getItem(storageKey(scope)) || "null");
    if (value && typeof value.id === "string" && /^[a-f0-9-]{36}$/.test(value.id) && value.args && typeof value.args === "object") return value;
  } catch { /* Private browsing may disable storage; keep the in-memory record. */ }
  return null;
}

// A page reload must not turn an unconfirmed mutation into a new request.
// This journal never resends anything itself and is scoped to the browser tab.
export function rememberSubmission(scope: string, args: Record<string, unknown>): Submission {
  const previous = readSubmission(scope);
  if (previous && JSON.stringify(previous.args) === JSON.stringify(args)) return previous;
  const record = { id: requestId(), args };
  pending.set(scope, record);
  try { sessionStorage.setItem(storageKey(scope), JSON.stringify(record)); }
  catch {
    // Large photos can exceed the tab's storage quota. Persist a small marker
    // so a reload still prevents duplicate submission and asks for reselection.
    try {
      const images = Array.isArray(args.images) ? args.images.map(image => ({ name: image.name, size: image.size, url: "" })) : undefined;
      sessionStorage.setItem(storageKey(scope), JSON.stringify({ id: record.id, args: { ...args, images } }));
    } catch { /* Private browsing may disable storage entirely. Keep memory. */ }
  }
  return record;
}

export function forgetSubmission(scope: string): void {
  pending.delete(scope);
  try { sessionStorage.removeItem(storageKey(scope)); } catch { /* See readSubmission. */ }
}

export function clearSubmissions(): void {
  pending.clear();
  try {
    const keys = Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index));
    for (const key of keys) if (key?.startsWith("codey-remote-pending:")) sessionStorage.removeItem(key);
  } catch { /* See readSubmission. */ }
}
