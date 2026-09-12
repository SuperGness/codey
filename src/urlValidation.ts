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
