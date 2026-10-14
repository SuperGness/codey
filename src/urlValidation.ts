/** Validates optional per-route outbound proxy URLs before they are saved. */
export function validateOutboundProxyUrl(value: string, label = "上游代理") {
  const normalized = value.trim();
  if (!normalized) return "";
  try {
    const parsed = new URL(normalized);
    if (
      !["http:", "https:", "socks5:", "socks5h:"].includes(parsed.protocol) ||
      !parsed.hostname
    ) {
      return `${label}必须是 http、https、socks5 或 socks5h 地址`;
    }
    return "";
  } catch {
    return `${label}格式无效`;
  }
}

/** Validates the Codex launch proxy before it is saved. Stricter than route
 * proxies: Chromium's --proxy-server cannot authenticate and SOCKS rules
 * require an explicit port. */
export function validateCodexLaunchProxy(value: string, label = "Codex 启动代理") {
  const normalized = value.trim();
  if (!normalized) return "";
  try {
    const parsed = new URL(normalized);
    if (
      !["http:", "https:", "socks5:", "socks5h:"].includes(parsed.protocol) ||
      !parsed.hostname
    ) {
      return `${label}必须是 http、https、socks5 或 socks5h 地址`;
    }
    if (parsed.username || parsed.password) {
      return `${label}不能包含用户名或密码`;
    }
    // 非特殊协议（socks5/socks5h）的 pathname 是空串而非 "/"。
    if (!["", "/"].includes(parsed.pathname) || parsed.search || parsed.hash) {
      return `${label}只能包含协议、地址和端口`;
    }
    if (
      (parsed.protocol === "socks5:" || parsed.protocol === "socks5h:") &&
      !parsed.port
    ) {
      return `${label}的 SOCKS 地址必须带端口`;
    }
    return "";
  } catch {
    return `${label}格式无效`;
  }
}

/** Optional gateway address. Blank keeps the caller's default endpoint. */
export function validateOptionalOutboundApiUrl(value: string, label = "网关地址") {
  const normalized = value.trim();
  if (!normalized) return "";
  return validateOutboundApiUrl(normalized, label);
}

/** Validates custom API endpoints before they are saved. */
export function validateOutboundApiUrl(value: string, label = "API URL") {
  const normalized = value.trim();
  if (!normalized) return `请输入 ${label}`;
  try {
    const parsed = new URL(normalized);
    if (!["http:", "https:"].includes(parsed.protocol) || !parsed.hostname) {
      return `${label}必须是有效的 HTTP(S) 地址`;
    }
    if (parsed.username || parsed.password) {
      return `${label}不能包含用户名或密码，请在 Key 字段单独填写凭据`;
    }
    return "";
  } catch {
    return `${label}格式无效`;
  }
}
