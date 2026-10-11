export const TOKENS_PER_K = 1_000;

/// 把 Token 数换算成 K 单位的显示文本，最多保留三位小数并去掉多余的零。
export function formatTokenK(tokens: number): string {
  const value = tokens / TOKENS_PER_K;
  return Number.isInteger(value) ? String(value) : String(Number(value.toFixed(3)));
}

/// 解析 K 单位的输入文本并换算回整数 Token，可带 K 后缀，非法或超出三位小数时返回 undefined。
export function parseTokenK(raw: string): number | undefined {
  const normalized = raw.trim();
  if (!/^(?:\d+|\d{1,3}(?:,\d{3})+)(?:\.\d{1,3})?[kK]?$/.test(normalized)) {
    return undefined;
  }
  const amount = normalized.replace(/,/g, "").replace(/[kK]$/, "");
  const tokens = Math.round(Number(amount) * TOKENS_PER_K);
  return Number.isSafeInteger(tokens) ? tokens : undefined;
}
