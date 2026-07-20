declare global {
  interface Window {
    __codeyInvokeApi?: (command: string, args: Record<string, unknown>) => Promise<unknown>;
  }
}

export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (typeof window.__codeyInvokeApi !== "function") {
    throw new Error("Codey bridge 尚未连接，请退出 Codex 后重新启动 Codey");
  }
  const value = await window.__codeyInvokeApi(command, args) as { status?: string; message?: string };
  if (value?.status === "failed") throw new Error(value.message || "Codey bridge 请求失败");
  return value as T;
}
