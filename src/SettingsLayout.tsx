import { useId, useState, type ReactNode, type RefObject } from "react";
import {
  IconBook2,
  IconLayoutDashboard,
  IconPlugConnected,
  IconRoute,
  IconServer,
  IconSparkles,
  IconUsersGroup,
} from "@tabler/icons-react";
import { SettingsPageHeaderTarget } from "./SettingsPageHeader";

const SETTINGS_PAGES = [
  { id: "overview", title: "基础功能", icon: IconLayoutDashboard, group: "核心配置" },
  { id: "models", title: "线路与模型", icon: IconRoute, group: "核心配置" },
  { id: "prompt", title: "提示词优化", icon: IconSparkles, group: "核心配置" },
  { id: "subagents", title: "子代理优化", icon: IconUsersGroup, group: "核心配置" },
  { id: "plugins", title: "Codey 插件", icon: IconPlugConnected, group: "扩展生态" },
  { id: "mcp", title: "MCP 管理", icon: IconServer, group: "扩展生态" },
  { id: "skills", title: "Skill 管理", icon: IconBook2, group: "扩展生态" },
] as const;

const SETTINGS_GROUPS = ["核心配置", "扩展生态"] as const;
const SETTINGS_GROUP_ENTRIES = SETTINGS_GROUPS.map(
  (group) => [group, SETTINGS_PAGES.filter((page) => page.group === group)] as const,
);
const MENU_PAGES = SETTINGS_GROUP_ENTRIES.flatMap(([, pages]) => pages);
const MENU_NAVIGATION_KEYS = new Set([
  "ArrowDown",
  "ArrowUp",
  "ArrowRight",
  "ArrowLeft",
  "Home",
  "End",
]);

type SettingsPageId = (typeof SETTINGS_PAGES)[number]["id"];
type SettingsSection = ReactNode | ((active: boolean) => ReactNode);

type SettingsLayoutProps = {
  active?: boolean;
  brand?: ReactNode;
  hidden?: boolean;
  sections: Record<SettingsPageId, SettingsSection>;
  sidebarFooter?: ReactNode;
  contentRef?: RefObject<HTMLDivElement | null>;
};

function SettingsPageContent({ children, contentRef, labelledBy }: {
  children: ReactNode;
  contentRef?: RefObject<HTMLDivElement | null>;
  labelledBy: string;
}) {
  const [headerTarget, setHeaderTarget] = useState<HTMLDivElement | null>(null);

  return (
    <SettingsPageHeaderTarget.Provider value={headerTarget}>
      <div className="settings-page-header-host" ref={setHeaderTarget} />
      <div
        ref={contentRef}
        className="settings-page-scroll page-scroll"
        aria-labelledby={labelledBy}
        tabIndex={0}
      >
        <div className="page">{children}</div>
      </div>
    </SettingsPageHeaderTarget.Provider>
  );
}

export function SettingsLayout({ active: layoutActive = true, brand, sections, sidebarFooter, contentRef, hidden = false }: SettingsLayoutProps) {
  const [activePage, setActivePage] = useState<SettingsPageId>("overview");
  const [visitedPages, setVisitedPages] = useState<ReadonlySet<SettingsPageId>>(
    () => new Set(["overview"]),
  );
  const id = useId();

  function selectPage(page: SettingsPageId) {
    setVisitedPages((visited) => visited.has(page) ? visited : new Set([...visited, page]));
    setActivePage(page);
  }

  function handleMenuKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (!MENU_NAVIGATION_KEYS.has(event.key)) return;
    const items = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>("button.settings-nav-item"),
    );
    const currentIndex = items.indexOf(event.target as HTMLButtonElement);
    if (currentIndex === -1 || items.length === 0) return;
    event.preventDefault();
    let nextIndex = currentIndex;
    if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = items.length - 1;
    } else {
      const step = event.key === "ArrowDown" || event.key === "ArrowRight" ? 1 : -1;
      nextIndex = (currentIndex + step + items.length) % items.length;
    }
    const nextButton = items[nextIndex];
    const nextPage = MENU_PAGES[nextIndex];
    if (nextButton && nextPage) {
      selectPage(nextPage.id);
      nextButton.focus();
    }
  }

  return (
    <div className="settings-layout" hidden={hidden}>
      <nav className="settings-sidebar" aria-label="配置功能菜单">
        {brand && <div className="settings-sidebar-header">{brand}</div>}
        <div className="settings-sidebar-menu" onKeyDown={handleMenuKeyDown}>
          {SETTINGS_GROUP_ENTRIES.map(([group, pages]) => (
            <div key={group} className="settings-nav-group">
              <div className="settings-nav-group-title">{group}</div>
              {pages.map((page) => {
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
                    <Icon size={18} stroke={1.85} aria-hidden="true" />
                    <span>{page.title}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
        {sidebarFooter && <div className="settings-sidebar-footer">{sidebarFooter}</div>}
      </nav>

      <div className="settings-content" id="codey-settings-content" tabIndex={-1}>
        {/* 首次访问时挂载模块，之后保留草稿、展开状态和异步加载结果。 */}
        {SETTINGS_PAGES.map((page) => {
          const active = layoutActive && !hidden && activePage === page.id;
          const content = sections[page.id];
          return (
            <section
              key={page.id}
              id={`${id}-page-${page.id}`}
              className="settings-page"
              aria-labelledby={`${id}-menu-${page.id}`}
              hidden={!active}
            >
              {visitedPages.has(page.id) && (
                <SettingsPageContent
                  contentRef={active ? contentRef : undefined}
                  labelledBy={`${id}-menu-${page.id}`}
                >
                  {typeof content === "function" ? content(active) : content}
                </SettingsPageContent>
              )}
            </section>
          );
        })}
      </div>
    </div>
  );
}
