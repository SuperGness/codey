export const CODEY_API_COMMANDS = [
  "load_codey_config",
  "save_codey_config",
  "sync_current_provider",
  "delete_route",
  "fetch_route_models",
  "save_selected_models",
  "save_default_model",
  "save_official_route_models",
  "runtime_status",
  "open_route_request_logs",
  "query_route_request_logs",
  "query_route_request_log_stats",
  "query_official_account_usage",
  "store_official_account_usage",
  "list_official_accounts",
  "start_official_account_login",
  "poll_official_account_login",
  "cancel_official_account_login",
  "import_current_codex_login",
  "set_default_official_account",
  "remove_official_account",
  "clear_route_request_logs",
  "restart_codey",
  "clear_diagnostic_storage",
  "repair_codex_overlays",
  "test_notification_channel",
  "start_wechat_claw_login",
  "poll_wechat_claw_login",
  "optimize_prompt",
  "test_prompt_optimization",
  "fetch_prompt_optimization_models",
  "check_for_updates",
  "download_update",
  "install_downloaded_update",
  "plugin_marketplace_status",
  "repair_plugin_marketplace",
  "repair_main_process_injection",
] as const;

export type CodeyApiCommand = (typeof CODEY_API_COMMANDS)[number];

const codeyApiCommandSet = new Set<string>(CODEY_API_COMMANDS);

export function isCodeyApiCommand(command: string): command is CodeyApiCommand {
  return codeyApiCommandSet.has(command);
}

export function codeyApiPath(command: string): `/api/${CodeyApiCommand}` {
  if (!isCodeyApiCommand(command)) {
    throw new Error(`不允许的 Codey API 命令：${command}`);
  }
  return `/api/${command}`;
}

declare global {
  interface Window {
    __codeyInvokeApi?: (
      command: CodeyApiCommand,
      args: Record<string, unknown>,
    ) => Promise<unknown>;
  }
}

/** Cross-module marker: ESM loaders can hand different instances the class. */
export const CodeyApiErrorMarker: unique symbol = Symbol.for("codey.api.error");

/** A request the Codey bridge answered with an explicit failure. */
export class CodeyApiError extends Error {
  readonly [CodeyApiErrorMarker] = true;

  constructor(message: string) {
    super(message);
    this.name = "CodeyApiError";
  }
}

export function isCodeyApiError(error: unknown): error is CodeyApiError {
  return typeof error === "object" && error !== null && CodeyApiErrorMarker in error;
}

export async function invoke<T>(
  command: CodeyApiCommand,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (typeof window.__codeyInvokeApi !== "function") {
    throw new Error("Codey bridge 尚未连接，请退出 Codex 后重新启动 Codey");
  }
  const value = await window.__codeyInvokeApi(command, args) as { status?: string; message?: string };
  if (value?.status === "failed") {
    throw new CodeyApiError(value.message || "Codey bridge 请求失败");
  }
  return value as T;
}
