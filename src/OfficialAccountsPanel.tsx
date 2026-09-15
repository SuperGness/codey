import { useCallback, useEffect, useRef, useState } from "react";
import { IconCheck, IconLogin2 as IconLogin, IconPlus, IconRefresh, IconTrash, IconUserCircle } from "@tabler/icons-react";

import { invoke } from "./api";
import { errorText } from "./appUtils";
import { Badge, Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Tooltip } from "./components/ui";
import type { OfficialAccount, OfficialAccountsResult } from "./App.types";
import type { AccountUsageSnapshot } from "./quotaEstimate";

type LoginStart = { loginId: string; authUrl: string; browserOpened?: boolean };
type LoginPoll = OfficialAccountsResult & { status: "wait" | "ok" | "failed" | "expired"; message?: string };

const LOGIN_POLL_INTERVAL_MS = 1500;

function formatPlan(plan?: string) {
  if (!plan) return "";
  const known: Record<string, string> = { free: "Free", plus: "Plus", pro: "Pro", team: "Team", business: "Business", enterprise: "Enterprise", edu: "Edu" };
  return known[plan.toLowerCase()] || plan;
}

function formatResetTime(resetsAt?: number) {
  if (!resetsAt) return "";
  const remaining = resetsAt * 1000 - Date.now();
  if (remaining <= 0) return "即将重置";
  const hours = Math.floor(remaining / 3_600_000);
  if (hours >= 48) return `${Math.floor(hours / 24)} 天后重置`;
  if (hours >= 1) return `${hours} 小时后重置`;
  return `${Math.max(1, Math.floor(remaining / 60_000))} 分钟后重置`;
}

function windowLabel(minutes: number) {
  if (minutes === 300) return "5 小时";
  if (minutes === 10080) return "每周";
  if (minutes % 1440 === 0) return `${minutes / 1440} 天`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时`;
  return `${minutes} 分钟`;
}

function UsageLine({ snapshot }: { snapshot: AccountUsageSnapshot | null }) {
  if (!snapshot) return <small className="official-account-usage">正在读取额度…</small>;
  if (snapshot.status !== "ok") {
    return <small className="official-account-usage is-muted">{snapshot.message || "额度暂不可用"}</small>;
  }
  const windows = [snapshot.primary, snapshot.secondary].filter(
    (window): window is NonNullable<typeof window> => Boolean(window),
  );
  if (windows.length === 0) return <small className="official-account-usage is-muted">官方未返回额度窗口</small>;
  return (
    <small className="official-account-usage">
      {windows.map((window, index) => (
        <span key={`${window.windowMinutes}-${index}`}>
          {windowLabel(window.windowMinutes)} 剩余 {Math.max(0, 100 - Math.round(window.usedPercent))}%
          {window.resetsAt ? `（${formatResetTime(window.resetsAt)}）` : ""}
        </span>
      ))}
      {snapshot.stale ? <span className="is-muted">（上次成功数据）</span> : null}
    </small>
  );
}

export type OfficialAccountsPanelProps = {
  officialAccountAvailable: boolean;
  isBusy: boolean;
  popupContainer: HTMLElement | null;
  onAccountsChanged: (result: OfficialAccountsResult) => void;
  onNotice: (notice: { tone: "success" | "info" | "error"; text: string }) => void;
};

export function OfficialAccountsPanel({
  officialAccountAvailable,
  isBusy,
  popupContainer,
  onAccountsChanged,
  onNotice,
}: OfficialAccountsPanelProps) {
  const [accounts, setAccounts] = useState<OfficialAccount[] | null>(null);
  const [loadError, setLoadError] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const [usage, setUsage] = useState<AccountUsageSnapshot | null>(null);
  const [login, setLogin] = useState<LoginStart | null>(null);
  const [loginError, setLoginError] = useState("");
  const [copied, setCopied] = useState(false);
  const loginRef = useRef<LoginStart | null>(null);
  loginRef.current = login;

  const applyResult = useCallback(
    (result: OfficialAccountsResult) => {
      if (Array.isArray(result.accounts)) setAccounts(result.accounts);
      onAccountsChanged(result);
    },
    [onAccountsChanged],
  );

  const refresh = useCallback(async () => {
    try {
      const result = await invoke<OfficialAccountsResult>("list_official_accounts");
      setAccounts(result.accounts ?? []);
      setLoadError("");
    } catch (error) {
      setLoadError(errorText(error));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const defaultAccount = accounts?.find((account) => account.isDefault) ?? null;
  const defaultAccountId = defaultAccount?.id ?? null;
  const refreshUsage = useCallback(async (force: boolean) => {
    if (!defaultAccountId || !officialAccountAvailable) {
      setUsage(null);
      return;
    }
    try {
      setUsage(await invoke<AccountUsageSnapshot>("query_official_account_usage", { forceRefresh: force }));
    } catch (error) {
      setUsage({ status: "error", message: errorText(error) });
    }
  }, [defaultAccountId, officialAccountAvailable]);

  useEffect(() => {
    setUsage(null);
    void refreshUsage(false);
  }, [refreshUsage]);

  // Poll a pending login until the callback completes or the user closes it.
  useEffect(() => {
    if (!login) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      if (!active) return;
      try {
        const result = await invoke<LoginPoll>("poll_official_account_login", { loginId: login.loginId });
        if (!active) return;
        if (result.status === "wait") {
          timer = setTimeout(() => void poll(), LOGIN_POLL_INTERVAL_MS);
          return;
        }
        if (result.status === "ok") {
          applyResult(result);
          setLogin(null);
          onNotice({ tone: "success", text: result.warning ? `账号已添加；${result.warning}` : "官方账号已添加" });
          return;
        }
        setLoginError(result.message || (result.status === "expired" ? "登录已过期，请重新开始" : "登录失败"));
      } catch (error) {
        if (!active) return;
        setLoginError(errorText(error));
      }
    };
    void poll();
    return () => {
      active = false;
      if (timer) clearTimeout(timer);
    };
  }, [login, applyResult, onNotice]);

  async function startLogin() {
    setPending("login");
    setLoginError("");
    setCopied(false);
    try {
      const result = await invoke<LoginStart>("start_official_account_login");
      setLogin(result);
    } catch (error) {
      onNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPending(null);
    }
  }

  async function closeLogin() {
    const current = loginRef.current;
    setLogin(null);
    setLoginError("");
    if (current) {
      try {
        await invoke("cancel_official_account_login", { loginId: current.loginId });
      } catch {
        // The session expires on its own; nothing to surface.
      }
    }
  }

  async function copyAuthUrl() {
    if (!login) return;
    try {
      await navigator.clipboard.writeText(login.authUrl);
      setCopied(true);
    } catch {
      setCopied(false);
      onNotice({ tone: "info", text: "无法访问剪贴板，请手动复制登录链接" });
    }
  }

  async function importCurrent() {
    setPending("import");
    try {
      const result = await invoke<OfficialAccountsResult>("import_current_codex_login");
      applyResult(result);
      onNotice({ tone: "success", text: "已导入当前 Codex 登录的官方账号" });
    } catch (error) {
      onNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPending(null);
    }
  }

  async function setDefault(account: OfficialAccount) {
    setPending(`default:${account.id}`);
    try {
      const result = await invoke<OfficialAccountsResult>("set_default_official_account", { accountId: account.id });
      applyResult(result);
      onNotice({
        tone: result.warning ? "info" : "success",
        text: result.warning
          ? `已切换默认官方账号；${result.warning}`
          : result.restartRequired
            ? "已切换默认官方账号，Codex 重启后以该账号登录"
            : "已切换默认官方账号；本地路由已即时切换，Codex 其余登录态在重启后完全生效",
      });
    } catch (error) {
      onNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPending(null);
    }
  }

  async function remove(account: OfficialAccount) {
    const label = account.email || account.accountId || account.id;
    if (!window.confirm(`移除官方账号 ${label}？${account.isDefault ? "它是当前默认账号，移除后 Codex 将退出该账号登录。" : ""}`)) return;
    setPending(`remove:${account.id}`);
    try {
      const result = await invoke<OfficialAccountsResult>("remove_official_account", { accountId: account.id });
      applyResult(result);
      onNotice({ tone: result.warning ? "info" : "success", text: result.warning ? `账号已移除；${result.warning}` : "官方账号已移除" });
    } catch (error) {
      onNotice({ tone: "error", text: errorText(error) });
    } finally {
      setPending(null);
    }
  }

  const disabled = isBusy || pending !== null;

  return (
    <div className="official-accounts-panel" aria-label="官方账号">
      <div className="official-accounts-header">
        <span className="official-accounts-title">官方账号</span>
        <div className="official-accounts-actions">
          <Tooltip content="把 Codex 里已登录的 ChatGPT 账号加入列表">
            <Button variant="link" color="primary" size="xs" disabled={disabled} loading={pending === "import"} onClick={() => void importCurrent()}>
              <IconLogin size={13} aria-hidden="true" />
              <span>导入当前登录</span>
            </Button>
          </Tooltip>
          <Button variant="brand-outline" size="xs" disabled={disabled} loading={pending === "login"} onClick={() => void startLogin()}>
            <IconPlus size={13} aria-hidden="true" />
            <span>添加账号</span>
          </Button>
        </div>
      </div>

      {loadError && <small className="official-accounts-error">{loadError}</small>}
      {accounts && accounts.length === 0 && !loadError && (
        <small className="official-accounts-empty">尚未添加官方账号。添加后设为默认，Codex 才能使用官方线路。</small>
      )}

      {accounts && accounts.length > 0 && (
        <ul className="official-account-list">
          {accounts.map((account) => {
            const label = account.email || account.accountId || account.id;
            const plan = formatPlan(account.planType);
            return (
              <li key={account.id} className={`official-account-item${account.isDefault ? " is-default" : ""}`}>
                <IconUserCircle size={18} className="official-account-avatar" aria-hidden="true" />
                <div className="official-account-main">
                  <div className="official-account-line">
                    <strong title={label}>{label}</strong>
                    {plan && <Badge variant="outline">{plan}</Badge>}
                    {account.isDefault && (
                      <Badge variant={officialAccountAvailable ? "success" : "warning"}>
                        {officialAccountAvailable ? "默认 · 已启用" : "默认 · 待重启"}
                      </Badge>
                    )}
                  </div>
                  {account.isDefault ? (
                    <div className="official-account-line">
                      <UsageLine snapshot={usage} />
                      <Button variant="link" color="primary" size="icon-sm" disabled={disabled} onClick={() => void refreshUsage(true)} aria-label="刷新额度" title="刷新额度">
                        <IconRefresh size={13} aria-hidden="true" />
                      </Button>
                    </div>
                  ) : (
                    <small className="official-account-usage is-muted">未启用</small>
                  )}
                </div>
                <div className="official-account-controls">
                  {!account.isDefault && (
                    <Button
                      variant="filled"
                      size="xs"
                      disabled={disabled}
                      loading={pending === `default:${account.id}`}
                      onClick={() => void setDefault(account)}
                    >
                      <IconCheck size={13} aria-hidden="true" />
                      <span>设为默认并显示额度</span>
                    </Button>
                  )}
                  <Button
                    variant="link"
                    color="danger"
                    size="icon-sm"
                    disabled={disabled}
                    loading={pending === `remove:${account.id}`}
                    onClick={() => void remove(account)}
                    aria-label={`移除官方账号 ${label}`}
                    title="移除账号"
                  >
                    <IconTrash size={13} aria-hidden="true" />
                  </Button>
                </div>
              </li>
            );
          })}
        </ul>
      )}

      <Dialog open={login !== null} onOpenChange={(open) => { if (!open) void closeLogin(); }}>
        <DialogContent className="official-login-dialog" container={popupContainer}>
          <DialogHeader>
            <DialogTitle>添加官方账号</DialogTitle>
            <DialogDescription>
              {login?.browserOpened === false
                ? "未能自动打开浏览器，请复制下方链接手动打开并完成 ChatGPT 登录。"
                : "已在系统浏览器打开 ChatGPT 登录页，完成登录后会自动回到这里。"}
            </DialogDescription>
          </DialogHeader>
          <div className="official-login-body">
            <code className="official-login-url" title={login?.authUrl}>{login?.authUrl}</code>
            {loginError
              ? <small className="official-accounts-error">{loginError}</small>
              : <small className="official-login-waiting">等待浏览器完成登录…（10 分钟内有效）</small>}
          </div>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => void copyAuthUrl()}>
              {copied ? "已复制" : "复制登录链接"}
            </Button>
            {loginError ? (
              <Button size="sm" onClick={() => void startLogin()}>重新开始</Button>
            ) : (
              <Button variant="secondary" size="sm" onClick={() => void closeLogin()}>取消</Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
