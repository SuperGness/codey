// 开发预览专用：所有状态仅存于内存，不访问配置、文件、凭证或进程。
import type {
  Inventory,
  Scope,
  SkillCacheInventory,
} from "../features/codex-extensions/types";

// 预览使用不压缩的 ZIP 条目，避免为模拟导出增加运行时依赖。
function previewZip(filename: string, content: string): string {
  const name = new TextEncoder().encode(filename),
    body = new TextEncoder().encode(content);
  let crc = 0xffffffff;
  for (const byte of body) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++)
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  crc = (crc ^ 0xffffffff) >>> 0;
  const bytes = new Uint8Array(
      30 + name.length + body.length + 46 + name.length + 22,
    ),
    view = new DataView(bytes.buffer);
  const u16 = (offset: number, value: number) =>
    view.setUint16(offset, value, true);
  const u32 = (offset: number, value: number) =>
    view.setUint32(offset, value, true);
  u32(0, 0x04034b50);
  u16(4, 20);
  u32(14, crc);
  u32(18, body.length);
  u32(22, body.length);
  u16(26, name.length);
  bytes.set(name, 30);
  bytes.set(body, 30 + name.length);
  const central = 30 + name.length + body.length;
  u32(central, 0x02014b50);
  u16(central + 4, 20);
  u16(central + 6, 20);
  u32(central + 16, crc);
  u32(central + 20, body.length);
  u32(central + 24, body.length);
  u16(central + 28, name.length);
  bytes.set(name, central + 46);
  const end = central + 46 + name.length;
  u32(end, 0x06054b50);
  u16(end + 8, 1);
  u16(end + 10, 1);
  u32(end + 12, 46 + name.length);
  u32(end + 16, central);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function createCodexExtensionsPreview(platform: string) {
  const home = platform === "windows" ? "C:\\Users\\preview" : "/Users/preview";
  const states = new Map<
    string,
    { inventory: Inventory; bodies: Record<string, string>; version: number }
  >();
  const parameters = new URLSearchParams(window.location.search);
  let conflictOnce = parameters.get("extensions") === "conflict";
  const scenario = parameters.get("extensions");
  let cacheVersion = 1;
  let cacheConflictOnce = scenario === "cache-conflict";
  const cache: SkillCacheInventory = {
    revision: "cache-preview-1",
    warnings: ["开发预览：缓存操作仅修改内存模拟数据。"],
    skills: scenario === "empty" ? [] : [{
      id: "plugin-browser-cache", name: "browser", version: "1.2.0",
      description: "浏览网页并检查本地页面的插件 Skill 缓存。",
      enabled: false, enabledKnown: false, scope: "plugin", ownership: "plugin",
      readOnly: true, canRemove: scenario !== "permission",
      reason: scenario === "permission" ? "预览：缓存无删除权限" : undefined,
      sourcePath: `${home}/.codex/plugins/cache/preview/browser/1.2.0/skills/browser`,
      manifestPath: `${home}/.codex/plugins/cache/preview/browser/1.2.0/skills/browser/SKILL.md`,
    }],
  };

  return async (request: Record<string, unknown>): Promise<unknown> => {
    const scope = (request.scope ?? { kind: "user" }) as Scope;
    const key = scope.kind === "user" ? "user" : scope.projectPath;
    const root =
      scope.kind === "user" ? `${home}/.codex` : `${scope.projectPath}/.codex`;
    if (!states.has(key)) {
      states.set(key, {
        version: 1,
        bodies: {
          docs: JSON.stringify({
            url: "https://example.invalid/mcp",
            enabled: true,
          }),
          local: JSON.stringify({
            command: "example-mcp",
            args: [],
            enabled: false,
          }),
        },
        inventory: {
          scope,
          configPath: `${root}/config.toml`,
          skillConfigPath: `${home}/.codex/config.toml`,
          revision: "preview-1",
          mcps: [
            {
              id: "docs",
              name: "docs",
              enabled: true,
              sourcePath: `${root}/config.toml`,
              transport: "http",
              readOnly: false,
              summary: "HTTP 连接",
            },
            {
              id: "local",
              name: "local",
              enabled: false,
              sourcePath: `${root}/config.toml`,
              transport: "stdio",
              readOnly: false,
              summary: "本地进程",
            },
          ],
          skills: [
            {
              id: "review",
              name: "review",
              description: "检查项目变更和测试覆盖。",
              enabled: true,
              enabledKnown: true,
              sourcePath: `${root}/skills/review`,
              manifestPath: `${root}/skills/review/SKILL.md`,
              scope: scope.kind,
              ownership: "external",
              readOnly: false,
            },
            {
              id: "builtin",
              name: "skill-creator",
              description: "系统内置的 Skill 编写指南。",
              enabled: true,
              enabledKnown: true,
              sourcePath: `${home}/.codex/skills/.system/skill-creator`,
              manifestPath: `${home}/.codex/skills/.system/skill-creator/SKILL.md`,
              scope: "system",
              ownership: "builtin",
              readOnly: true,
              reason: "Codex 系统资源不可修改",
            },
          ],
          warnings: ["开发预览：操作仅修改模拟数据，不会读写你的 Codex 配置。"],
          applyNotice:
            "MCP 保存后自动刷新 Codex 配置；Skill 变更请在新会话中确认。运行时覆盖可能影响生效状态。",
        },
      });
      const inventory = states.get(key)!.inventory;
      for (const entry of [...inventory.mcps, ...inventory.skills]) {
        entry.updatedAt = "2026-09-20T08:00:00Z";
        entry.configurationStatus = "valid";
        entry.canToggle = !entry.readOnly;
        entry.canCheck = !entry.readOnly;
        entry.canEdit =
          !entry.readOnly &&
          (!("ownership" in entry) || entry.ownership === "managed");
        // 与后端一致：外部安装可以直接删除，系统内置与插件缓存只读。
        entry.canRemove =
          !entry.readOnly &&
          (!("ownership" in entry) || entry.ownership !== "builtin");
      }
      inventory.mcps.forEach((entry) => {
        entry.scope = scope.kind;
      });
      inventory.skills[0].dependencies = [{ type: "mcp", name: "docs" }];
      inventory.skills[0].version = "1.0.0";
      if (scenario === "empty") {
        inventory.mcps = [];
        inventory.skills = [];
      }
      if (scenario === "large") {
        inventory.mcps = Array.from({ length: 1000 }, (_, index) => ({
          ...inventory.mcps[index % 2],
          id: `mcp-${index}`,
          name: `服务 ${String(index).padStart(4, "0")}`,
        }));
        inventory.skills = Array.from({ length: 1000 }, (_, index) => ({
          ...inventory.skills[0],
          id: `skill-${index}`,
          name: `技能 ${String(index).padStart(4, "0")}`,
        }));
        for (const entry of inventory.mcps)
          states.get(key)!.bodies[entry.id] = JSON.stringify({
            command: "example-mcp",
            enabled: false,
          });
      }
      if (scenario === "permission")
        for (const entry of [...inventory.mcps, ...inventory.skills]) {
          entry.readOnly = true;
          entry.canEdit = false;
          entry.canToggle = false;
          entry.canCheck = false;
          entry.canRemove = false;
          entry.reason = "预览：当前资源无写入权限";
        }
      if (scenario === "invalid") {
        inventory.mcps[0].configurationStatus = "invalid";
        inventory.mcps[0].error = "预览：缺少有效的 command 或 URL";
        inventory.skills[0].configurationStatus = "invalid";
        inventory.skills[0].error = "预览：Skill 缺少 description 元数据";
      }
    }
    const state = states.get(key)!;
    const action = String(request.action);
    const id = String(request.id ?? "");
    if (scenario === "offline")
      throw new Error("预览：服务不可用，请检查连接后重试");
    if (scenario === "timeout")
      await new Promise((resolve) => setTimeout(resolve, 20000));
    if (action === "list_skill_cache") return structuredClone(cache);
    if (action === "read_skill_cache" || action === "remove_skill_cache") {
      const entry = cache.skills.find((item) => item.id === id);
      if (!entry) throw new Error("缓存 Skill 不存在，请刷新列表");
      if (action === "read_skill_cache") return {
        id, revision: cache.revision, readOnly: true,
        content: `---\nname: ${entry.name}\ndescription: ${entry.description}\n---\n\n# 浏览器使用指南\n\n这是内存预览内容。\n`,
      };
      if (request.confirmed !== true) throw new Error("请先确认删除缓存 Skill");
      if (cacheConflictOnce) {
        cacheConflictOnce = false;
        cache.revision = `cache-preview-${++cacheVersion}`;
        throw new Error("缓存已被其他程序修改，请刷新后重新操作");
      }
      if (request.revision !== cache.revision) throw new Error("缓存已变化，请刷新后重新操作");
      if (entry.canRemove === false) throw new Error("权限不足：此缓存不能删除");
      if (scenario === "cache-timeout") await new Promise((resolve) => setTimeout(resolve, 20000));
      cache.skills = cache.skills.filter((item) => item.id !== id);
      cache.revision = `cache-preview-${++cacheVersion}`;
      return { cache: structuredClone(cache), message: "模拟缓存 Skill 已删除。" };
    }
    const mcp = () => {
      const item = state.inventory.mcps.find((entry) => entry.id === id);
      if (!item) throw new Error("MCP 不存在，请刷新列表");
      return item;
    };
    const skill = () => {
      const item = state.inventory.skills.find((entry) => entry.id === id);
      if (!item) throw new Error("Skill 不存在，请刷新列表");
      return item;
    };
    if (action === "pick_project") return { path: `${home}/Projects/demo` };
    if (action === "pick_skill")
      return { path: `${home}/Downloads/demo-skill` };
    if (action === "list") {
      if (parameters.get("extensions") === "error")
        throw new Error("预览：配置无效，未执行修改");
      return structuredClone(state.inventory);
    }
    if (action === "get_mcp") {
      mcp();
      return {
        id,
        configJson: JSON.parse(state.bodies[id]),
        revision: state.inventory.revision,
      };
    }
    if (action === "read_skill") {
      const entry = skill();
      return {
        id,
        revision: state.inventory.revision,
        readOnly: entry.ownership !== "managed",
        content:
          state.bodies[id] ??
          `---\nname: ${entry.name}\ndescription: ${entry.description}\n---\n# ${entry.name}\n`,
      };
    }
    if (action === "preview_skill_install") {
      const sourcePath = String(request.sourcePath ?? "");
      if (!/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(sourcePath))
        throw new Error("请使用绝对路径");
      return {
        name: "demo-skill",
        description: "本地导入的演示 Skill。",
        sourcePath,
        sourceKind: sourcePath.toLowerCase().endsWith(".zip") ? "zip" : "directory",
        files: [{ path: "SKILL.md", size: 128, executable: false }],
        fileCount: 1,
        totalBytes: 128,
        sourceRevision: `source-preview:${sourcePath}`,
      };
    }
    if (action === "validate_skill" || action === "test_mcp") {
      const entry = action === "test_mcp" ? mcp() : skill();
      if (entry.canCheck === false) throw new Error("当前资源不允许检查");
      if (action === "test_mcp" && request.confirmed !== true)
        throw new Error("请先确认 MCP 连接测试");
      const valid = entry.configurationStatus !== "invalid";
      return {
        ok: valid,
        summary: valid
          ? "预览检查通过，未执行真实程序或连接网络。"
          : "预览：配置检查失败",
        ...(action === "test_mcp"
          ? {
              serverInfo: { name: "preview-mcp", version: "1.0.0" },
              protocolVersion: "2025-03-26",
            }
          : {}),
        checks: [
          {
            name: action === "test_mcp" ? "MCP 握手" : "Skill 元数据",
            ok: valid,
            message: valid ? "模拟数据有效" : (entry.error ?? "配置无效"),
          },
        ],
      };
    }
    if (action === "export_skill") {
      const entry = skill();
      return {
        filename: `${entry.name}.zip`,
        mediaType: "application/zip",
        dataBase64: previewZip(
          "SKILL.md",
          state.bodies[id] ??
            `---\nname: ${entry.name}\ndescription: ${entry.description}\n---\n# ${entry.name}\n`,
        ),
      };
    }
    if (conflictOnce) {
      conflictOnce = false;
      state.inventory.revision = `preview-${++state.version}`;
      throw new Error("预览：配置已被其他程序修改，请刷新后重新操作");
    }
    if (request.revision !== state.inventory.revision)
      throw new Error("配置已变化，请刷新后重新操作");
    if (scenario === "permission")
      throw new Error("权限不足：预览配置无法写入");
    if (
      [
        "remove_mcp",
        "uninstall_skill",
        "set_mcps_enabled",
        "set_skills_enabled",
      ].includes(action) &&
      request.confirmed !== true
    )
      throw new Error("请先确认操作影响");
    if (
      ["set_mcp_enabled", "set_skill_enabled"].includes(action) &&
      request.enabled === true &&
      request.confirmed !== true
    )
      throw new Error("请先确认操作影响");
    switch (action) {
      case "save_mcp": {
        if ("configToml" in request) throw new Error("仅支持 configJson 配置");
        const json = request.configJson;
        if (!json || typeof json !== "object" || Array.isArray(json))
          throw new Error("JSON 配置必须为对象");
        const config = structuredClone(json as Record<string, unknown>);
        if (config.headers !== undefined && config.http_headers !== undefined)
          throw new Error("headers 与 http_headers 不能同时提供");
        if (config.headers !== undefined) {
          config.http_headers = config.headers;
          delete config.headers;
        }
        if (
          config.type !== undefined &&
          !["stdio", "http", "streamable-http", "streamableHttp"].includes(
            String(config.type),
          )
        )
          throw new Error("不支持此连接类型");
        const command =
          typeof config.command === "string" && config.command.trim();
        const url = typeof config.url === "string" && config.url.trim();
        if (!!command === !!url)
          throw new Error("必须设置 command 或 url，且只能设置一种");
        if (
          (config.type === "stdio" && !command) ||
          (config.type && config.type !== "stdio" && !url)
        )
          throw new Error("连接类型与配置不一致");
        delete config.type;
        const validate = (value: unknown): void => {
          if (value === null) throw new Error("配置不支持 null");
          if (typeof value === "object")
            Object.values(value as object).forEach(validate);
        };
        validate(config);
        if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) throw new Error("服务标识无效");
        const existing = state.inventory.mcps.find((entry) => entry.id === id);
        if (existing && request.createOnly === true)
          throw new Error("此 MCP 服务标识已存在，请打开原有服务进行编辑");
        if (existing && (existing.readOnly || existing.canEdit === false))
          throw new Error("此服务不可编辑");
        if (!existing) config.enabled = true;
        const enabled = config.enabled !== false;
        if (enabled && !existing?.enabled && request.confirmed !== true)
          throw new Error("保存将启用 MCP，请先确认信任此服务");
        const entry = {
          id,
          name: id,
          enabled,
          sourcePath: state.inventory.configPath,
          transport: Boolean(url) ? ("http" as const) : ("stdio" as const),
          readOnly: false,
          summary: "预览服务",
          scope: scope.kind,
          configurationStatus: "valid" as const,
          updatedAt: new Date().toISOString(),
        };
        state.inventory.mcps = [
          ...state.inventory.mcps.filter((item) => item.id !== id),
          entry,
        ];
        state.bodies[id] = JSON.stringify(config);
        break;
      }
      case "set_mcp_enabled": {
        if (mcp().readOnly || mcp().canToggle === false)
          throw new Error("此服务不可修改");
        mcp().enabled = Boolean(request.enabled);
        state.bodies[id] = JSON.stringify({
          ...JSON.parse(state.bodies[id]),
          enabled: Boolean(request.enabled),
        });
        break;
      }
      case "remove_mcp":
        if (mcp().readOnly || mcp().canRemove === false)
          throw new Error("此服务不可移除");
        state.inventory.mcps = state.inventory.mcps.filter(
          (item) => item.id !== id,
        );
        delete state.bodies[id];
        break;
      case "set_skill_enabled":
        if (
          skill().readOnly ||
          skill().canToggle === false ||
          skill().enabledKnown === false
        )
          throw new Error("此 Skill 不可切换状态");
        skill().enabled = Boolean(request.enabled);
        break;
      case "save_skill":
        if (skill().readOnly || skill().ownership !== "managed")
          throw new Error("此 Skill 不可编辑");
        state.bodies[id] = String(request.content);
        break;
      case "uninstall_skill":
        if (skill().readOnly || skill().canRemove === false)
          throw new Error("Skill 当前不允许删除，请检查目录或冲突规则");
        state.inventory.skills = state.inventory.skills.filter(
          (item) => item.id !== id,
        );
        delete state.bodies[id];
        break;
      case "set_mcps_enabled":
      case "set_skills_enabled": {
        const ids = request.ids;
        if (
          !Array.isArray(ids) ||
          !ids.length ||
          ids.length > 100 ||
          new Set(ids).size !== ids.length
        )
          throw new Error("请选择 1 至 100 个不同资源");
        const entries =
          action === "set_mcps_enabled"
            ? state.inventory.mcps
            : state.inventory.skills;
        const targets = ids.map((target) => {
          const item = entries.find((entry) => entry.id === target);
          if (
            !item ||
            item.readOnly ||
            item.canToggle === false ||
            ("enabledKnown" in item && item.enabledKnown === false)
          )
            throw new Error("包含不存在或不可修改的资源，未执行任何修改");
          return item;
        });
        targets.forEach((entry) => {
          entry.enabled = Boolean(request.enabled);
          entry.updatedAt = new Date().toISOString();
          if (action === "set_mcps_enabled")
            state.bodies[entry.id] = JSON.stringify({
              ...JSON.parse(state.bodies[entry.id]),
              enabled: Boolean(request.enabled),
            });
        });
        break;
      }
      case "create_skill": {
        const content = String(request.content);
        const name = /^name:\s*(\S+)/m.exec(content)?.[1];
        const description = /^description:\s*(.+)/m.exec(content)?.[1];
        if (!name || !description || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))
          throw new Error("Skill 元数据无效，请使用小写名称和描述");
        if (state.inventory.skills.some((entry) => entry.name === name))
          throw new Error("安装目标已存在");
        state.inventory.skills.push({
          id: name,
          name,
          description,
          enabled: false,
          enabledKnown: true,
          sourcePath: `${root}/skills/${name}`,
          manifestPath: `${root}/skills/${name}/SKILL.md`,
          scope: scope.kind,
          ownership: "managed",
          readOnly: false,
          configurationStatus: "valid",
          updatedAt: new Date().toISOString(),
        });
        state.bodies[name] = content;
        break;
      }
      case "install_skill": {
        if (!/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(String(request.sourcePath)))
          throw new Error("请使用绝对路径");
        if (request.sourceRevision !== undefined && request.sourceRevision !== `source-preview:${request.sourcePath}`)
          throw new Error("Skill 来源已变化，请重新预览后安装");
        if (state.inventory.skills.some((item) => item.id === "demo-skill"))
          throw new Error("安装目标已存在，请先解决同名目录冲突");
        state.inventory.skills.push({
          id: "demo-skill",
          name: "demo-skill",
          description: "本地导入的演示 Skill。",
          enabled: false,
          enabledKnown: true,
          sourcePath: `${root}/skills/demo-skill`,
          manifestPath: `${root}/skills/demo-skill/SKILL.md`,
          scope: scope.kind,
          ownership: "managed",
          readOnly: false,
          configurationStatus: "valid",
          updatedAt: new Date().toISOString(),
        });
        break;
      }
      default:
        throw new Error("不支持的预览操作");
    }
    state.inventory.revision = `preview-${++state.version}`;
    const mcpMutation = ["save_mcp", "set_mcp_enabled", "set_mcps_enabled", "remove_mcp"].includes(action);
    return {
      inventory: structuredClone(state.inventory),
      applyStatus: mcpMutation ? "applied" : "restart-required",
      message: mcpMutation ? "预览 MCP 配置已保存并刷新。" : "预览修改已保存。",
    };
  };
}
