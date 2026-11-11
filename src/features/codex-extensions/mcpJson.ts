import { exceedsByteLimit } from "./state";
import type { EditorDraft } from "./types";

export const NEW_MCP_JSON =
  '{\n  "mcpServers": {\n    "my-server": {\n      "command": "npx",\n      "args": ["-y", "your-mcp-package"]\n    }\n  }\n}\n';
export interface McpJsonService {
  name: string;
  config: Record<string, unknown>;
}
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

const CONTENT_BYTE_LIMIT = 1024 * 1024;

function parseServices(content: string): McpJsonService[] {
  if (exceedsByteLimit(content, CONTENT_BYTE_LIMIT))
    throw new Error("JSON 内容不能超过 1 MB。");
  let value: unknown;
  try {
    value = JSON.parse(content.replace(/^\uFEFF/, ""));
  } catch {
    throw new Error("JSON 格式无效，请检查双引号、逗号和括号。");
  }
  if (!object(value))
    throw new Error("JSON 顶层必须是配置对象，不能是数组或 null。");
  const wrappers = ["mcpServers", "mcp_servers"].filter((key) =>
    Object.prototype.hasOwnProperty.call(value, key),
  );
  if (wrappers.length > 1)
    throw new Error("请仅保留 mcpServers 或 mcp_servers 中的一种包装。");
  const services = wrappers.length ? value[wrappers[0]] : { "": value };
  if (!object(services) || !Object.keys(services).length)
    throw new Error("服务列表必须是非空对象。");
  return Object.entries(services).map(([name, config]) => {
    if (wrappers.length && !name.trim()) throw new Error("服务名称不能为空。");
    if (!object(config) || !Object.keys(config).length)
      throw new Error("每个服务都需要非空配置对象。");
    if (
      !(typeof config.command === "string" && config.command.trim()) &&
      !(typeof config.url === "string" && config.url.trim())
    )
      throw new Error("每个服务至少需要非空的 command 或 url。");
    return { name, config };
  });
}

// 渲染与按键都会校验同一份草稿内容：按内容记忆化，避免每键重新解析整份 JSON。
// 返回的是只读缓存结果，调用方不得就地修改。
let cachedContent: string | null = null;
let cachedServices: McpJsonService[] | null = null;
let cachedError: Error | null = null;

export function parseMcpJson(content: string): McpJsonService[] {
  if (content === cachedContent) {
    if (cachedError) throw cachedError;
    if (cachedServices) return cachedServices;
  }
  try {
    const services = parseServices(content);
    cachedContent = content;
    cachedServices = services;
    cachedError = null;
    return services;
  } catch (cause) {
    cachedContent = content;
    cachedServices = null;
    cachedError = cause instanceof Error ? cause : new Error(String(cause));
    throw cachedError;
  }
}

export function selectedMcpJson(
  draft: Pick<EditorDraft, "content" | "jsonService">,
): McpJsonService {
  const services = parseMcpJson(draft.content);
  const selected =
    services.length === 1
      ? services[0]
      : services.find((item) => item.name === draft.jsonService);
  if (!selected) throw new Error("请选择本次要导入的服务，一次保存一个服务。");
  return selected;
}

export function updateMcpJsonDraft(
  draft: EditorDraft,
  content: string,
): EditorDraft {
  let previousName = draft.jsonService;
  if (previousName === undefined) {
    try {
      previousName = selectedMcpJson(draft).name;
    } catch {
      /* 尚未完成选择。 */
    }
  }
  try {
    const services = parseMcpJson(content);
    const selected =
      services.length === 1
        ? services[0]
        : services.find((service) => service.name === previousName);
    return {
      ...draft,
      content,
      jsonService: selected?.name,
      id:
        draft.isNew &&
        selected?.name &&
        (selected.name !== previousName || !draft.id)
          ? selected.name
          : draft.id,
    };
  } catch {
    // 输入暂时不完整时保留已选服务和自定义标识，修复 JSON 后继续沿用。
    return { ...draft, content, jsonService: previousName };
  }
}

export function mcpJsonError(draft: EditorDraft): string {
  if (draft.kind !== "mcp") return "";
  try {
    selectedMcpJson(draft);
    return "";
  } catch (cause) {
    return cause instanceof Error ? cause.message : "JSON 配置无效。";
  }
}

export function exportMcpJson(
  id: string,
  config: Record<string, unknown>,
): string {
  return JSON.stringify({ mcpServers: { [id]: config } }, null, 2) + "\n";
}
