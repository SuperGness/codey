// 配置中的空字符串表示移除该请求头，编辑时以 null 呈现。
export function headersTextFromMap(headers: Record<string, string> | undefined) {
  return JSON.stringify(
    Object.fromEntries(Object.entries(headers || {}).map(([name, value]) => [name, value === "" ? null : value])),
    null,
    2,
  );
}

export function parseHeadersText(text: string): Record<string, string> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    throw new Error("请输入合法的 JSON 对象");
  }
  if (!parsed || Array.isArray(parsed) || typeof parsed !== "object") {
    throw new Error("请求头必须是 JSON 对象");
  }
  const entries = Object.entries(parsed);
  if (entries.length > 128) throw new Error("请求头最多 128 个");
  const names = new Set<string>();
  const encoder = new TextEncoder();
  let totalBytes = 0;
  return Object.fromEntries(entries.map(([name, value]) => {
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name)) {
      throw new Error("请求头名称只能包含英文字母、数字和有效的 HTTP 标点字符");
    }
    const normalizedName = name.toLowerCase();
    if (names.has(normalizedName)) throw new Error(`请求头 ${normalizedName} 重复，名称不区分大小写`);
    names.add(normalizedName);
    if (value !== null && typeof value !== "string") throw new Error(`请求头 ${normalizedName} 的值必须是字符串或 null`);
    const headerValue = value === null ? "" : value as string;
    if (/[\x00-\x08\x0a-\x1f\x7f]/.test(headerValue)) throw new Error(`请求头 ${normalizedName} 的值包含非法控制字符`);
    if (headerValue !== "" && headerValue.trim() === "") throw new Error(`请求头 ${normalizedName} 不能只包含空白；请用 null 删除`);
    const valueBytes = encoder.encode(headerValue).length;
    if (valueBytes > 8192) throw new Error(`请求头 ${normalizedName} 的值不能超过 8 KiB`);
    totalBytes += encoder.encode(normalizedName).length + valueBytes;
    if (totalBytes > 32768) throw new Error("请求头名称和值合计不能超过 32 KiB");
    return [normalizedName, headerValue];
  }));
}
