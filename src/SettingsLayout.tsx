import { useId, useState, type ReactNode, type RefObject } from "react";
import {
  IconBook2,
  IconDeviceMobile,
  IconLayoutDashboard,
  IconPlugConnected,
  IconRoute,
  IconServer,
  IconSparkles,
  IconUsersGroup,
} from "@tabler/icons-react";

const SETTINGS_PAGES = [
  { id: "overview", title: "基础功能", icon: IconLayoutDashboard, group: "核心配置" },
  { id: "models", title: "线路与模型", icon: IconRoute, group: "核心配置" },
  { id: "prompt", title: "提示词优化", icon: IconSparkles, group: "核心配置" },
  { id: "subagents", title: "子代理优化", icon: IconUsersGroup, group: "核心配置" },
  { id: "remote", title: "远程控制", icon: IconDeviceMobile, group: "核心配置" },
  { id: "plugins", title: "Codey 插件", icon: IconPlugConnected, group: "扩展生态" },
  { id: "mcp", title: "MCP 管理", icon: IconServer, group: "扩展生态" },
  { id: "skills", title: "Skill 管理", icon: IconBook2, group: "扩展生态" },
] as const;

type SettingsPageId = (typeof SETTINGS_PAGES)[number]["id"];
type SettingsSection = ReactNode | ((active: boolean) => ReactNode);

type SettingsLayoutProps = {
  sections: Record<SettingsPageId, SettingsSection>;
  sidebarFooter?: ReactNode;
  contentRef?: RefObject<HTMLDivElement | null>;
};

export function SettingsLayout({ sections, sidebarFooter, contentRef }: SettingsLayoutProps) {
  const [activePage, setActivePage] = useState<SettingsPageId>("overview");
  const [visitedPages, setVisitedPages] = useState<ReadonlySet<SettingsPageId>>(
    () => new Set(["overview"]),
  );
  const id = useId();

  function selectPage(page: SettingsPageId) {
    setVisitedPages((visited) => visited.has(page) ? visited : new Set([...visited, page]));
    setActivePage(page);
  }

  const corePages = SETTINGS_PAGES.filter((p) => p.group === "核心配置");
  const extensionPages = SETTINGS_PAGES.filter((p) => p.group === "扩展生态");

  return (
    <div className="settings-layout">
      <nav className="settings-sidebar" aria-label="配置功能菜单">
        <div className="settings-sidebar-menu">
        <div className="settings-nav-group">
          <div className="settings-nav-group-title">核心配置</div>
          {corePages.map((page) => {
            const Icon = page.icon;
            return (
              <button
                key={page.id}
                id={`${id}-menu-${page.id}`}
                type="button"
                className="settings-nav-item"
                aria-current={activePage === page.id ? "page" : undefined}
                aria-controls={`${id}-page-${page.id}`}
                onClick={() => selectPage(page.id)}
              >
                <Icon size={16} stroke={1.8} aria-hidden="true" />
                <span>{page.title}</span>
              </button>
            );
          })}
        </div>

        <div className="settings-nav-group">
          <div className="settings-nav-group-title">扩展生态</div>
          {extensionPages.map((page) => {
            const Icon = page.icon;
            return (
              <button
                key={page.id}
                id={`${id}-menu-${page.id}`}
                type="button"
                className="settings-nav-item"
                aria-current={activePage === page.id ? "page" : undefined}
                aria-controls={`${id}-page-${page.id}`}
                onClick={() => selectPage(page.id)}
              >
                <Icon size={16} stroke={1.8} aria-hidden="true" />
                <span>{page.title}</span>
              </button>
            );
          })}
        </div>
        </div>
        {sidebarFooter && <div className="settings-sidebar-footer">{sidebarFooter}</div>}
      </nav>

      <div className="settings-content" id="codey-settings-content" tabIndex={-1}>
        {/* 首次访问时挂载模块，之后保留草稿、展开状态和异步加载结果。 */}
        {SETTINGS_PAGES.map((page) => {
          const active = activePage === page.id;
          const content = sections[page.id];
          return (
            <section
              key={page.id}
              id={`${id}-page-${page.id}`}
              ref={active ? contentRef : undefined}
              className="settings-page page-scroll"
              aria-labelledby={`${id}-menu-${page.id}`}
              hidden={!active}
              tabIndex={0}
            >
              {visitedPages.has(page.id) && (
                <div className="page">
                  {typeof content === "function" ? content(active) : content}
                </div>
              )}
            </section>
          );
        })}
      </div>
    </div>
  );
}
