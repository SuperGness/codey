export const CODEY_API_COMMANDS = [
  "load_codey_config",
  "save_codey_config",
  "sync_current_provider",
  "sync_prompt_optimization_current_provider",
  "fetch_current_provider_models",
  "save_selected_models",
  "save_default_model",
  "runtime_status",
  "refresh_injection_status",
  "refresh_diagnostic_storage_stats",
  "refresh_trace_log_stats",
  "launch_codey",
  "restart_codey",
  "clear_diagnostic_storage",
  "clear_codex_trace_logs",
  "test_webhook",
  "test_notification_channel",
  "reveal_notification_channel",
  "reveal_prompt_optimization_api_key",
  "optimize_prompt",
  "test_prompt_optimization",
  "fetch_prompt_optimization_models",
  "check_for_updates",
  "download_update",
  "install_downloaded_update",
  "plugin_marketplace_status",
  "repair_plugin_marketplace",
  "workflow_capabilities",
  "workflow_start",
  "workflow_steer",
  "workflow_list",
  "workflow_get",
  "workflow_events",
  "workflow_artifact",
  "workflow_pause",
  "workflow_resume",
  "workflow_cancel",
  "workflow_retry_node",
  "workflow_reply_interaction",
  "workflow_bypass_audit",
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

export async function invoke<T>(
  command: CodeyApiCommand,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (typeof window.__codeyInvokeApi !== "function") {
    throw new Error("Codey bridge 尚未连接，请退出 Codex 后重新启动 Codey");
  }
  const value = await window.__codeyInvokeApi(command, args) as { status?: string; message?: string };
  if (value?.status === "failed") throw new Error(value.message || "Codey bridge 请求失败");
  return value as T;
}
