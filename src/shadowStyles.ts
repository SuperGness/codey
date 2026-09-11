// Chromium 不在 ShadowRoot 内注册 @property；复用 Tailwind 已生成的初始值，
// 并让 HeroUI 的基础变量与派生变量在同一个 :host 上解析。
function propertyDefaults(sheet: CSSStyleSheet): string {
  return Array.from(sheet.cssRules)
    .filter((rule): rule is CSSLayerBlockRule => rule instanceof CSSLayerBlockRule && rule.name === "properties")
    .flatMap((layer) => Array.from(layer.cssRules))
    .filter((rule): rule is CSSSupportsRule => rule instanceof CSSSupportsRule)
    .flatMap((rule) => Array.from(rule.cssRules, (child) => child.cssText))
    .join("\n");
}

export function shadowStyles(css: string): string {
  const scoped = css.replace(/:root\b/g, ":host");
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(scoped);
  return `${scoped}\n@layer properties {${propertyDefaults(sheet)}}`;
}

// 构造一次样式表并直接由 ShadowRoot 采用：与先解析再塞 <style> 相比，
// 数百 KB 的 CSS 只解析一遍。
export function shadowStyleSheet(utilityCss: string, ...sheets: string[]): CSSStyleSheet {
  const sheet = new CSSStyleSheet();
  sheet.replaceSync([utilityCss.replace(/:root\b/g, ":host"), ...sheets].join("\n"));
  const defaults = propertyDefaults(sheet);
  if (defaults) sheet.insertRule(`@layer properties {${defaults}}`, sheet.cssRules.length);
  return sheet;
}
