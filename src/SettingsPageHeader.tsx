import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

// 各页继续拥有标题内容和操作状态，由公共布局把标题放到正文滚动区外。
export const SettingsPageHeaderTarget = createContext<HTMLElement | null | undefined>(undefined);

type SettingsPageHeaderProps = {
  id: string;
  title: string;
  description?: string;
  actions?: ReactNode;
  badge?: ReactNode;
  icon?: ReactNode;
};

export function SettingsPageHeader({
  id,
  title,
  description,
  actions,
  badge,
  icon,
}: SettingsPageHeaderProps) {
  const target = useContext(SettingsPageHeaderTarget);
  const header = (
    <header className="settings-page-header">
      <div className="settings-page-header-main">
        {icon && (
          <div className="settings-page-icon-pill" aria-hidden="true">
            {icon}
          </div>
        )}
        <div className="settings-page-heading">
          <div className="settings-page-title-row">
            <h2 id={id}>{title}</h2>
            {badge}
          </div>
          {description && <p className="settings-page-description">{description}</p>}
        </div>
      </div>
      {actions && <div className="settings-page-actions">{actions}</div>}
    </header>
  );
  return target === undefined ? header : target ? createPortal(header, target) : null;
}
