// Chromium 不在 ShadowRoot 内注册 @property；复用 Tailwind 已生成的初始值，
// 并让 HeroUI 的基础变量与派生变量在同一个 :host 上解析。
export function shadowStyles(css: string): string {
  const scoped = css.replace(/:root\b/g, ":host");
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(scoped);
  const defaults = Array.from(sheet.cssRules)
    .filter((rule): rule is CSSLayerBlockRule => rule instanceof CSSLayerBlockRule && rule.name === "properties")
    .flatMap((layer) => Array.from(layer.cssRules))
    .filter((rule): rule is CSSSupportsRule => rule instanceof CSSSupportsRule)
    .flatMap((rule) => Array.from(rule.cssRules, (child) => child.cssText))
    .join("\n");
  return `${scoped}\n@layer properties {${defaults}}`;
}
