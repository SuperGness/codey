import { useCallback, useEffect, useRef, useState } from "react";
import { IconBrandOpenai, IconCheck, IconClipboard, IconLogin2 as IconLogin, IconPlus, IconRefresh, IconSparkles, IconTrash } from "@tabler/icons-react";

import { invoke } from "./api";
import { errorText } from "./appUtils";
import { listOfficialAccounts, rememberOfficialAccounts } from "./officialAccountsRequests";
import { Badge, Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, TextArea, Tooltip } from "./components/ui";
import { maskEmail } from "./sensitiveText";
import type { Confirmation, OfficialAccount, OfficialAccountsResult } from "./App.types";
import type { AccountUsageSnapshot } from "./quotaEstimate";

type LoginStart = { loginId: string; authUrl: string; browserOpened?: boolean };
type LoginPoll = OfficialAccountsResult & { status: "wait" | "ok" | "failed" | "expired"; message?: string };

const LOGIN_POLL_INTERVAL_MS = 1500;
// 账号较多时连续额度查询容易触发官方风控，逐条读取之间留出间隔。
const USAGE_QUERY_STAGGER_MS = 200;

function formatPlan(plan?: string) {
  if (!plan) return "";
  const known: Record<string, string> = { free: "Free", plus: "Plus", pro: "Pro", team: "Team", business: "Business", enterprise: "Enterprise", edu: "Edu" };
  return known[plan.toLowerCase()] || plan;
}

function planTagClass(plan?: string) {
  const p = plan?.toLowerCase();
  if (p === "plus") return "official-account-plan-badge is-plus";
  if (p === "pro") return "official-account-plan-badge is-pro";
  if (p === "team" || p === "business" || p === "enterprise") return "official-account-plan-badge is-team";
  return "official-account-plan-badge is-default";
}

function formatSpecificResetTime(resetsAt?: number): { full: string; short: string; exact: string; relative: string } | null {
  if (!resetsAt) return null;
  const date = new Date(resetsAt * 1000);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;

  const remaining = resetsAt * 1000 - Date.now();
  let relative = "";
  if (remaining <= 0) {
    relative = "即将重置";
  } else {
    const hours = Math.floor(remaining / 3_600_000);
    if (hours >= 48) {
      relative = `${Math.floor(hours / 24)} 天后`;
    } else if (hours >= 1) {
      relative = `${hours} 小时后`;
    } else {
      relative = `${Math.max(1, Math.floor(remaining / 60_000))} 分钟后`;
    }
  }

  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();

  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  const isTomorrow =
    date.getFullYear() === tomorrow.getFullYear() &&
    date.getMonth() === tomorrow.getMonth() &&
    date.getDate() === tomorrow.getDate();

  let dateLabel = "";
  if (isToday) {
    dateLabel = "今天 ";
  } else if (isTomorrow) {
    dateLabel = "明天 ";
  } else if (date.getFullYear() === now.getFullYear()) {
    dateLabel = `${date.getMonth() + 1}月${date.getDate()}日 `;
  } else {
    dateLabel = `${date.getFullYear()}/${date.getMonth() + 1}/${date.getDate()} `;
  }

  const exact = `${dateLabel}${time}`;
  return {
    exact,
    relative,
    short: `${exact} 重置`,
    full: `${exact} 重置${relative ? `（${relative}）` : ""}`,
  };
}

function windowLabel(minutes: number) {
  if (minutes === 300) return "5 小时";
  if (minutes === 10080) return "每周";
  if (minutes % 1440 === 0) return `${minutes / 1440} 天`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时`;
  return `${minutes} 分钟`;
}

// 失效账号不再查询额度：卡片直接显示本地失效原因，重新添加账号后才恢复查询。
function invalidUsageSnapshot(account: OfficialAccount): AccountUsageSnapshot {
  return {
    status: "error",
    reason: "official_account_invalid",
    message: account.invalidReason || "账号已失效，不再查询额度；重新添加该账号即可恢复",
  };
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
    <div className="official-account-usage-row">
      {windows.map((window, index) => {
        const remainingPercent = Math.max(0, Math.min(100, 100 - Math.round(window.usedPercent)));
        const resetInfo = formatSpecificResetTime(window.resetsAt);
        const toneClass =
          remainingPercent <= 10
            ? "is-danger"
            : remainingPercent <= 30
              ? "is-warning"
              : "is-normal";
        const fullTimeTitle = window.resetsAt
          ? `重置时间：${new Date(window.resetsAt * 1000).toLocaleString("zh-CN")}（已消耗 ${Math.round(window.usedPercent)}%，剩余 ${remainingPercent}%）`
          : `已消耗 ${Math.round(window.usedPercent)}%，剩余 ${remainingPercent}%`;
        const resetLabel = resetInfo
          ? resetInfo.relative
            ? resetInfo.relative.endsWith("重置")
              ? resetInfo.relative
              : `${resetInfo.relative}重置`
            : resetInfo.short
          : null;
        return (
          <div
            key={`${window.windowMinutes}-${index}`}
            className="official-account-usage-item"
            title={fullTimeTitle}
          >
            <span className="official-account-usage-window">{windowLabel(window.windowMinutes)}</span>
            <div className="official-account-progress-track" aria-hidden="true">
              <div
                className={`official-account-progress-fill ${toneClass}`}
                style={{ width: `${remainingPercent}%` }}
              />
            </div>
            <span className={`official-account-usage-percent ${toneClass}`}>剩余 {remainingPercent}%</span>
            {resetLabel && (
              <span className="official-account-usage-reset">
                · {resetLabel}
                {resetInfo?.exact && (
                  <span className="official-account-usage-reset-time">（{resetInfo.exact}）</span>
                )}
              </span>
            )}
          </div>
        );
      })}
      {snapshot.stale ? <span className="official-account-usage-stale">（上次成功数据）</span> : null}
    </div>
  );
}

export type OfficialAccountsPanelProps = {
  officialAccountAvailable: boolean;
  isBusy: boolean;
  /** 「线路与模型」菜单是否打开；关闭时不再获取官方额度与套餐数据。 */
  active: boolean;
  maskSensitive?: boolean;
  popupContainer: HTMLElement | null;
  onAccountsLoaded?: (accounts: OfficialAccount[] | null) => void;
  onAccountsChanged: (result: OfficialAccountsResult) => void;
  onNotice: (notice: { tone: "success" | "info" | "error"; text: string }) => void;
  onRequestConfirmation?: (confirmation: Confirmation) => void;
};

export function OfficialAccountsPanel({
  officialAccountAvailable,
  isBusy,
  active,
  maskSensitive = false,
  popupContainer,
  onAccountsLoaded,
  onAccountsChanged,
  onNotice,
  onRequestConfirmation,
}: OfficialAccountsPanelProps) {
  const [accounts, setAccounts] = useState<OfficialAccount[] | null>(null);
  const [loadError, setLoadError] = useState("");
  const [pending, setPending] = useState<string | null>(null);
  const [confirmAccount, setConfirmAccount] = useState<OfficialAccount | null>(null);
  const [usages, setUsages] = useState<Record<string, AccountUsageSnapshot | null>>({});
  const [login, setLogin] = useState<LoginStart | null>(null);
  const [loginError, setLoginError] = useState("");
  const [copied, setCopied] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const [manualInput, setManualInput] = useState("");
  const [manualError, setManualError] = useState("");
  const loginRef = useRef<LoginStart | null>(null);
  loginRef.current = login;

  const applyResult = useCallback(
    (result: OfficialAccountsResult) => {
      if (Array.isArray(result.accounts)) {
        rememberOfficialAccounts(result);
        setAccounts(result.accounts);
      }
      onAccountsChanged(result);
    },
    [onAccountsChanged],
  );

  const refresh = useCallback(async (force = false) => {
    try {
      const result = await listOfficialAccounts({ force });
      const loaded = result.accounts ?? [];
      if (!Array.isArray(loaded)) throw new Error("官方账号列表格式无效，请重新加载");
      setAccounts(loaded);
      onAccountsLoaded?.(loaded);
      setLoadError("");
    } catch (error) {
      setLoadError(errorText(error));
      onAccountsLoaded?.(null);
    }
  }, [onAccountsLoaded]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 官方线路随账号库派生：账号失效后线路从配置里移除，账号恢复后重新出现。
  // 这里一次取回最新的账号列表、配置和模型状态。
  const refreshRoutes = useCallback(async () => {
    try {
      const result = await invoke<OfficialAccountsResult>("refresh_official_account_routes");
      applyResult(result);
      setLoadError("");
    } catch (error) {
      setLoadError(errorText(error));
    }
  }, [applyResult]);

  const accountsRef = useRef<OfficialAccount[]>([]);
  accountsRef.current = accounts ?? [];

  const refreshUsage = useCallback(async (accountId: string, force: boolean) => {
    setUsages((current) => ({ ...current, [accountId]: current[accountId] ?? null }));
    try {
      const snapshot = await invoke<AccountUsageSnapshot>("query_official_account_usage", {
        accountId,
        forceRefresh: force,
      });
      setUsages((current) => ({ ...current, [accountId]: snapshot }));
      // 后端确认账号失效后重算线路：卡片随即标红、隐藏切换默认入口，失效
      // 账号的线路同时从列表里移除。只在状态发生变化时重算一次，已标记失效的
      // 账号再次返回失效原因不会重复触发。
      const known = accountsRef.current.find((account) => account.id === accountId);
      const becameInvalid =
        snapshot.reason === "official_account_invalid" && !known?.invalid;
      const recovered = Boolean(known?.invalid) && snapshot.status === "ok";
      if (becameInvalid || recovered) {
        void refreshRoutes();
      }
    } catch (error) {
      setUsages((current) => ({ ...current, [accountId]: { status: "error", message: errorText(error) } }));
    }
  }, [refreshRoutes]);

  // 官方额度和套餐只在「线路与模型」菜单打开期间获取：每次打开都按当前账号
  // 列表重取一遍（后端 60 秒缓存之外的请求会真的打到官方），菜单关闭后不再
  // 后台轮询，账号卡片里的套餐也只在打开菜单时更新。查询跟随账号 id 与失效
  // 状态，检测到失效后重读列表不会重启整轮查询。
  const accountListKey =
    accounts?.map((account) => `${account.id}:${account.invalid ? 1 : 0}`).join("\n") ?? "";
  useEffect(() => {
    if (!active) return;
    const list = accountsRef.current;
    if (list.length === 0) return;
    let cancelled = false;
    void (async () => {
      // 逐个账号顺序读取额度，并在两次请求之间留出间隔，避免同时向官方接口发起多个请求。
      let queried = 0;
      for (const account of list) {
        if (cancelled) return;
        if (account.invalid) {
          // 失效账号不再查询额度，卡片直接显示本地失效原因。
          setUsages((current) => ({ ...current, [account.id]: invalidUsageSnapshot(account) }));
          continue;
        }
        if (queried > 0) {
          await new Promise((resolve) => setTimeout(resolve, USAGE_QUERY_STAGGER_MS));
          if (cancelled) return;
        }
        queried += 1;
        await refreshUsage(account.id, false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [active, accountListKey, refreshUsage]);

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

  function openManual() {
    setManualError("");
    setManualInput("");
    setManualOpen(true);
  }

  function closeManual() {
    if (pending === "manual") return;
    setManualOpen(false);
    setManualInput("");
    setManualError("");
  }

  async function submitManual() {
    const credential = manualInput.trim();
    if (!credential) {
      setManualError("请粘贴 Refresh Token 或 OAuth JSON");
      return;
    }
    setPending("manual");
    setManualError("");
    try {
      const result = await invoke<OfficialAccountsResult>("import_official_account_credential", { credential });
      applyResult(result);
      setManualOpen(false);
      setManualInput("");
      onNotice({
        tone: result.warning ? "info" : "success",
        text: result.warning ? `账号已添加；${result.warning}` : "官方账号已添加",
      });
    } catch (error) {
      setManualError(errorText(error));
    } finally {
      setPending(null);
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
      // 失败原因可能是账号已失效，重读列表即可显示失效标识并收起切换入口。
      void refresh(true);
    } finally {
      setPending(null);
    }
  }

  // 页面开启脱敏时，账号标签与提示统一使用脱敏后的邮箱。
  const accountLabel = useCallback(
    (account: OfficialAccount) => {
      const email = account.email?.trim();
      if (!email) return account.accountId || account.id;
      return maskSensitive ? maskEmail(email) : email;
    },
    [maskSensitive],
  );

  async function executeRemove(account: OfficialAccount) {
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

  function handleRemove(account: OfficialAccount) {
    const label = accountLabel(account);
    const title = `移除官方账号「${label}」？`;
    const description = account.isDefault
      ? "它是当前默认账号，移除后 Codex 将退出该账号登录。"
      : "确定要移除该官方账号吗？移除后将无法继续使用该账号。";
    if (onRequestConfirmation) {
      onRequestConfirmation({
        action: "delete-official-account",
        title,
        description,
        confirmLabel: "移除账号",
        run: () => void executeRemove(account),
      });
      return;
    }
    setConfirmAccount(account);
  }

  const disabled = isBusy || pending !== null;

  return (
    <div className="official-accounts-panel" aria-label="官方账号">
      <div className="official-accounts-header">
        <div className="official-accounts-title-group">
          <div className="official-accounts-avatar-pill" aria-hidden="true">
            <IconBrandOpenai size={15} />
          </div>
          <div className="official-accounts-heading-text">
            <div className="official-accounts-title-row">
              <span className="official-accounts-title">官方账号</span>
              {accounts && accounts.length > 0 && (
                <Badge variant="secondary">{accounts.length} 个账号</Badge>
              )}
            </div>
            <small className="official-accounts-subtitle">
              管理 ChatGPT 官方账号身份与使用额度
            </small>
          </div>
        </div>
        <div className="official-accounts-actions">
          <Tooltip content="把 Codex 里已登录的 ChatGPT 账号加入列表">
            <Button variant="link" color="primary" size="xs" disabled={disabled} loading={pending === "import"} onClick={() => void importCurrent()}>
              <IconLogin size={13} aria-hidden="true" />
              <span>导入当前登录</span>
            </Button>
          </Tooltip>
          <Tooltip content="粘贴 Refresh Token 或 OAuth JSON">
            <Button variant="link" color="primary" size="xs" disabled={disabled} onClick={openManual}>
              <IconClipboard size={13} aria-hidden="true" />
              <span>手动输入</span>
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
        <small className="official-accounts-empty">尚未添加官方账号。添加后每个账号都会成为一条官方线路，设为默认只决定 Codex 的登录身份和页头额度来源。</small>
      )}

      {accounts && accounts.length > 0 && (
        <ul className="official-account-list">
          {accounts.map((account) => {
            const label = accountLabel(account);
            // 账号记录里的套餐只反映添加账号时的登录信息。额度查询每次都带
            // 官方当前的套餐，有它时以它为准，降级后才不会继续显示 Pro。
            const snapshotPlan = usages[account.id]?.status === "ok" ? usages[account.id]?.planType : undefined;
            const planType = snapshotPlan || account.planType;
            const plan = formatPlan(planType);
            return (
              <li
                key={account.id}
                className={`official-account-item${account.isDefault ? " is-default" : ""}${account.invalid ? " is-invalid" : ""}`}
              >
                <div className="official-account-item-top">
                  <div className="official-account-item-left">
                    <div className="official-account-item-icon-pill" aria-hidden="true">
                      <IconBrandOpenai size={16} className="official-account-avatar" />
                    </div>
                    <div className="official-account-main">
                      <div className="official-account-line">
                        <strong title={label}>{label}</strong>
                        {plan && (
                          <span className={planTagClass(planType)}>
                            {planType?.toLowerCase() === "pro" ? (
                              <IconSparkles size={11} stroke={2.2} className="official-account-plan-icon" aria-hidden="true" />
                            ) : null}
                            <span>{plan}</span>
                          </span>
                        )}
                        {account.invalid ? (
                          <Badge variant="destructive" title={account.invalidReason}>
                            {account.isDefault ? "默认 · 已失效" : "账号已失效"}
                          </Badge>
                        ) : account.isDefault ? (
                          <Badge variant={officialAccountAvailable ? "success" : "warning"}>
                            {officialAccountAvailable ? "默认 · 已启用" : "默认 · 待重启"}
                          </Badge>
                        ) : null}
                      </div>
                    </div>
                  </div>
                  <div className="official-account-controls">
                    <Button
                      variant="link"
                      color="primary"
                      size="icon-sm"
                      disabled={disabled}
                      onClick={() => void refreshUsage(account.id, true)}
                      aria-label="刷新额度"
                      title="刷新额度"
                    >
                      <IconRefresh size={13} aria-hidden="true" />
                    </Button>
                    {!account.isDefault && !account.invalid && (
                      <Button
                        variant="filled"
                        size="xs"
                        disabled={disabled}
                        loading={pending === `default:${account.id}`}
                        onClick={() => void setDefault(account)}
                      >
                        <IconCheck size={13} aria-hidden="true" />
                        <span>设为默认</span>
                      </Button>
                    )}
                    <Button
                      variant="link"
                      color="danger"
                      size="icon-sm"
                      disabled={disabled}
                      loading={pending === `remove:${account.id}`}
                      onClick={() => void handleRemove(account)}
                      aria-label={`移除官方账号 ${label}`}
                      title="移除账号"
                    >
                      <IconTrash size={13} aria-hidden="true" />
                    </Button>
                  </div>
                </div>
                <div className="official-account-usage-container">
                  <UsageLine snapshot={usages[account.id] ?? null} />
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

      <Dialog open={manualOpen} onOpenChange={(open) => { if (!open) closeManual(); }}>
        <DialogContent
          className="sm:w-[520px]"
          container={popupContainer}
          onEscapeKeyDown={(event) => { if (pending === "manual") event.preventDefault(); }}
          onPointerDownOutside={(event) => { if (pending === "manual") event.preventDefault(); }}
        >
          <DialogHeader>
            <DialogTitle>手动添加官方账号</DialogTitle>
            <DialogDescription>
              粘贴 Refresh Token，或包含 access_token 与 refresh_token 的 OAuth JSON。只含 Refresh Token 时会向官方换取登录；已含 access token 的 JSON 直接保存，不会轮换来源令牌。
            </DialogDescription>
          </DialogHeader>
          <div className="official-login-body">
            <TextArea
              aria-label="Refresh Token 或 OAuth JSON"
              autoFocus
              autoComplete="off"
              className="min-h-[148px] resize-y font-mono text-xs leading-relaxed"
              disabled={pending === "manual"}
              placeholder="Refresh Token 或 OAuth JSON"
              spellCheck={false}
              value={manualInput}
              onChange={(event) => {
                setManualInput(event.target.value);
                if (manualError) setManualError("");
              }}
            />
            {manualError ? <small className="official-accounts-error">{manualError}</small> : null}
          </div>
          <DialogFooter>
            <Button variant="outline" size="sm" disabled={pending === "manual"} onClick={closeManual}>取消</Button>
            <Button size="sm" disabled={disabled || manualInput.trim() === ""} loading={pending === "manual"} onClick={() => void submitManual()}>
              添加
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={confirmAccount !== null} onOpenChange={(open) => { if (!open) setConfirmAccount(null); }}>
        <DialogContent className="confirmation-dialog" container={popupContainer}>
          <DialogHeader>
            <DialogTitle>
              {confirmAccount ? `移除官方账号「${accountLabel(confirmAccount)}」？` : "移除官方账号？"}
            </DialogTitle>
            <DialogDescription>
              {confirmAccount?.isDefault
                ? "它是当前默认账号，移除后 Codex 将退出该账号登录。"
                : "确定要移除该官方账号吗？移除后将无法继续使用该账号。"}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmAccount(null)}>取消</Button>
            <Button
              variant="destructive"
              disabled={disabled}
              loading={confirmAccount ? pending === `remove:${confirmAccount.id}` : false}
              onClick={() => {
                const target = confirmAccount;
                setConfirmAccount(null);
                if (target) void executeRemove(target);
              }}
            >
              <IconTrash aria-hidden="true" />
              移除账号
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
