import {
  type Dispatch,
  type SetStateAction,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

import { invoke } from "./api";
import type {
  Confirmation,
  Config,
  InlineResult,
  Notice,
  UpdateCheck,
  UpdateDownload,
  UpdateInstallReport,
} from "./App.types";
import { errorText, withTimeout } from "./appUtils";
import { formatBytes } from "./formatters";

const UPDATE_AVAILABLE_EVENT = "codey-update-availability-changed";
const AUTO_UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;
const UPDATE_CHECK_TIMEOUT_MS = 12_000;
const DEFERRED_UPDATE_STORAGE_KEY = "codey.deferredUpdateVersion";

declare global {
  interface Window {
    __codeyUpdateAvailability?: UpdateCheck | null;
  }
}

/// 用户点过"稍后"的版本，记在会话里。自动检查因此不会在同一个版本上反复
/// 弹窗，用户重新打开 Codey 后仍会收到提醒。
function readDeferredVersion(): string | null {
  try {
    return window.sessionStorage.getItem(DEFERRED_UPDATE_STORAGE_KEY);
  } catch {
    return null;
  }
}

function writeDeferredVersion(version: string) {
  try {
    window.sessionStorage.setItem(DEFERRED_UPDATE_STORAGE_KEY, version);
  } catch {
    // 存储不可用时退回到"每次检查都会提示"的旧行为，不影响功能。
  }
}

function updateInstallReportText(report: UpdateInstallReport): string {
  return report.message
    ? `v${report.version} 更新未完成：${report.message}`
    : `v${report.version} 更新未完成，请重试`;
}

type UseAppUpdatesOptions = {
  embedded: boolean;
  configLoaded: boolean;
  codeyUpdatePolicy: Config["codeyUpdatePolicy"];
  isBusy: boolean;
  setBusy: Dispatch<SetStateAction<string | null>>;
  setNotice: Dispatch<SetStateAction<Notice>>;
  setConfirmation: Dispatch<SetStateAction<Confirmation | null>>;
  beforeInstall: () => Promise<void>;
};

const updateAvailable = (
  check: UpdateCheck | null | undefined,
): check is UpdateCheck => check?.updateAvailable === true;

function updateVersionAllowed(version: string, policy: Config["codeyUpdatePolicy"]): boolean {
  return policy === "experimental" || !version.includes("-");
}

function updatePromptKey(result: Pick<UpdateCheck, "latestVersion" | "rollback">): string {
  return result.rollback ? `rollback:${result.rollback.id}` : result.latestVersion;
}

function updateCheckText(result: UpdateCheck) {
  if (result.rollback) return `可从 v${result.currentVersion} 回退至 v${result.latestVersion}：${result.rollback.reason}`;
  const base = result.updateAvailable
    ? result.selectedAsset
      ? `发现 v${result.latestVersion} 更新（当前 v${result.currentVersion}）`
      : `发现 v${result.latestVersion} 更新，但当前系统暂无可安装包`
    : `当前已是最新版本 v${result.currentVersion}`;
  const notes = result.releaseNotes?.trim();
  return notes ? `${base}：${notes}` : base;
}

function updateResultTone(result: UpdateCheck): InlineResult["tone"] {
  return result.updateAvailable && !result.selectedAsset
    ? "error"
    : "success";
}

function publishUpdateAvailability(result: UpdateCheck | null) {
  window.__codeyUpdateAvailability = updateAvailable(result) ? result : null;
  window.dispatchEvent(
    new CustomEvent(UPDATE_AVAILABLE_EVENT, {
      detail: window.__codeyUpdateAvailability,
    }),
  );
}

export function useAppUpdates({
  embedded,
  configLoaded,
  codeyUpdatePolicy,
  isBusy,
  setBusy,
  setNotice,
  setConfirmation,
  beforeInstall,
}: UseAppUpdatesOptions) {
  const [updateResult, setUpdateResult] = useState<InlineResult>({
    tone: "idle",
    text: "",
  });
  const [updateCheck, setUpdateCheck] = useState<UpdateCheck | null>(null);
  const [downloadedUpdate, setDownloadedUpdate] =
    useState<UpdateDownload | null>(null);
  const updateCheckRef = useRef<UpdateCheck | null>(null);
  const promptedVersionRef = useRef<string | null>(null);
  const [automaticallyChecking, setAutomaticallyChecking] = useState(false);
  const manualCheckVersion = useRef(0);
  const updateCheckInFlightRef = useRef<Promise<UpdateCheck> | null>(null);
  const policyRef = useRef(codeyUpdatePolicy);
  const previousPolicyRef = useRef(codeyUpdatePolicy);
  policyRef.current = codeyUpdatePolicy;
  useEffect(() => {
    if (previousPolicyRef.current === codeyUpdatePolicy) return;
    previousPolicyRef.current = codeyUpdatePolicy;
    manualCheckVersion.current += 1;
    updateCheckInFlightRef.current = null;
    updateCheckRef.current = null;
    setUpdateCheck(null);
    setDownloadedUpdate(null);
    setUpdateResult({ tone: "idle", text: "" });
    publishUpdateAvailability(null);
    setConfirmation(current => current && ["download-update", "install-update"].includes(current.action) ? null : current);
  }, [codeyUpdatePolicy]);
  const requestUpdateCheck = useCallback((forceRefresh = false, manual = false) => {
    if (policyRef.current === "off" && !manual) return Promise.reject(new Error("已关闭自动检查更新"));
    const current = updateCheckInFlightRef.current;
    if (current && !forceRefresh) return current;
    const request = withTimeout(
      invoke<UpdateCheck>("check_for_updates", { forceRefresh, ...(manual ? { manual: true } : {}) }),
      UPDATE_CHECK_TIMEOUT_MS,
      "检查更新超时，请检查网络",
    ).finally(() => {
      if (updateCheckInFlightRef.current === request) {
        updateCheckInFlightRef.current = null;
      }
    });
    updateCheckInFlightRef.current = request;
    return request;
  }, []);

  useEffect(() => {
    updateCheckRef.current = updateCheck;
  }, [updateCheck]);

  // 上一次"安装并重启"的真实结果。助手把结论写在配置目录里，这里读一次并
  // 展示，避免用户只看到版本号没变却没有任何解释。
  useEffect(() => {
    if (!configLoaded) return;
    let cancelled = false;
    void (async () => {
      let report: UpdateInstallReport | null = null;
      try {
        report = await invoke<UpdateInstallReport | null>("update_install_report");
      } catch {
        return;
      }
      if (cancelled || !report || report.status === "installed") return;
      if (report.status === "started") {
        setNotice({
          tone: "info",
          text: `v${report.version} 更新未完成，请重新打开 Codey 或再次点击更新`,
        });
        return;
      }
      setNotice({ tone: "error", text: updateInstallReportText(report) });
    })();
    return () => {
      cancelled = true;
    };
  }, [configLoaded, setNotice]);

  useEffect(() => {
    const applyDetectedUpdate = (
      result: UpdateCheck | null | undefined,
    ) => {
      if (policyRef.current === "off" || !updateAvailable(result) || (policyRef.current === "stable" && result.latestVersion.includes("-"))) return;
      setUpdateCheck(result);
      setDownloadedUpdate(null);
      setUpdateResult({
        tone: updateResultTone(result),
        text: updateCheckText(result),
      });
    };

    applyDetectedUpdate(window.__codeyUpdateAvailability);
    const handleUpdateAvailabilityChanged = (event: Event) => {
      applyDetectedUpdate((event as CustomEvent<UpdateCheck | null>).detail);
    };
    window.addEventListener(
      UPDATE_AVAILABLE_EVENT,
      handleUpdateAvailabilityChanged,
    );
    return () => {
      window.removeEventListener(
        UPDATE_AVAILABLE_EVENT,
        handleUpdateAvailabilityChanged,
      );
    };
  }, []);

  useEffect(() => {
    if (embedded || !configLoaded || codeyUpdatePolicy === "off") return;
    let cancelled = false;
    let timer = 0;

    const shouldPause = () =>
      updateAvailable(updateCheckRef.current) ||
      updateAvailable(window.__codeyUpdateAvailability);

    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = 0;
        void checkForUpdatesSilently();
      }, AUTO_UPDATE_CHECK_INTERVAL_MS);
    };

    const checkForUpdatesSilently = async () => {
      if (cancelled) return;
      // 暂停只影响本次检查，定时器链必须继续，否则状态被手动清空后不再恢复自动检查。
      if (shouldPause()) return schedule();
      const manualVersion = manualCheckVersion.current;
      setAutomaticallyChecking(true);
      try {
        const result = await requestUpdateCheck();
        if (cancelled || manualVersion !== manualCheckVersion.current) return;
        if (result.updateAvailable) {
          setUpdateCheck(result);
          setDownloadedUpdate(null);
          setUpdateResult({
            tone: updateResultTone(result),
            text: updateCheckText(result),
          });
          publishUpdateAvailability(result);
          const deferredVersion = readDeferredVersion();
          if (
            result.selectedAsset &&
            promptedVersionRef.current !== updatePromptKey(result) &&
            deferredVersion !== updatePromptKey(result)
          ) {
            promptedVersionRef.current = updatePromptKey(result);
            askDownloadUpdate(result);
          }
          return;
        }
        setUpdateResult({
          tone: "success",
          text: updateCheckText(result),
        });
      } catch {
        if (!cancelled && manualVersion === manualCheckVersion.current) {
          setUpdateResult({ tone: "idle", text: "" });
        }
        // 更新地址不可达或检查超时时直接跳过；手动检查仍会展示具体错误。
      } finally {
        if (!cancelled) {
          setAutomaticallyChecking(false);
          schedule();
        }
      }
    };

    void checkForUpdatesSilently();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      setAutomaticallyChecking(false);
    };
  }, [codeyUpdatePolicy, configLoaded, embedded, requestUpdateCheck]);

  async function checkForUpdates() {
    if (!configLoaded || isBusy) return;
    manualCheckVersion.current += 1;
    const requestVersion = manualCheckVersion.current;
    setBusy("check-update");
    setUpdateResult({ tone: "pending", text: "正在检查更新…" });
    setUpdateCheck(null);
    setDownloadedUpdate(null);
    try {
      const result = await requestUpdateCheck(true, true);
      if (requestVersion !== manualCheckVersion.current) return;
      if (result.updateAvailable && !updateVersionAllowed(result.latestVersion, policyRef.current)) {
        throw new Error("当前更新策略仅检查稳定版，已忽略实验版更新");
      }
      setUpdateCheck(result);
      publishUpdateAvailability(result);
      const text = updateCheckText(result);
      setUpdateResult({
        tone: updateResultTone(result),
        text,
      });
      setNotice({
        tone:
          result.updateAvailable && result.selectedAsset
            ? "info"
            : result.updateAvailable
              ? "error"
              : "success",
        text,
      });
      if (result.updateAvailable && result.selectedAsset) {
        promptedVersionRef.current = updatePromptKey(result);
        if (
          downloadedUpdate?.latestVersion === result.latestVersion &&
          downloadedUpdate.publishId === result.publishId &&
          downloadedUpdate.policyId === result.policyId &&
          downloadedUpdate.fileName === result.selectedAsset.fileName &&
          downloadedUpdate.sha256 === result.selectedAsset.sha256 &&
          downloadedUpdate.size === result.selectedAsset.size
        ) {
          setDownloadedUpdate(downloadedUpdate);
          askInstallDownloadedUpdate(downloadedUpdate);
        } else {
          askDownloadUpdate(result);
        }
      }
    } catch (error) {
      if (requestVersion !== manualCheckVersion.current) return;
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askDownloadUpdate(check?: UpdateCheck | null) {
    const target = check ?? updateCheck;
    if (!target?.updateAvailable || !target.selectedAsset || !updateVersionAllowed(target.latestVersion, policyRef.current) || isBusy) return;
    setConfirmation({
      action: "download-update",
      title: target.rollback ? `回退 Codey 至 v${target.latestVersion}` : `发现 Codey 新版本 v${target.latestVersion}`,
      description: [
        target.rollback ? `管理员已授权从 v${target.currentVersion} 降级至 v${target.latestVersion}。回退原因：${target.rollback.reason}。请保存工作，确认后下载安装旧版本并重启。` : `当前版本为 v${target.currentVersion}，检测到新版本 v${target.latestVersion}。`,
        "是否立即下载更新？",
      ]
        .filter(Boolean)
        .join("\n\n"),
      releaseNotes: target.releaseNotes?.trim() || undefined,
      confirmLabel: target.rollback ? "下载回退版本" : "立即更新",
      run: () => void downloadUpdate(target),
      // 用户已经在这次运行里明确推迟过这个版本，自动检查就不再反复弹窗。
      onDismiss: () => writeDeferredVersion(updatePromptKey(target)),
    });
  }

  async function downloadUpdate(checkOverride?: UpdateCheck | null) {
    const target = checkOverride ?? updateCheck;
    if (
      !configLoaded ||
      isBusy ||
      !target?.updateAvailable ||
      !target.selectedAsset ||
      !updateVersionAllowed(target.latestVersion, policyRef.current)
    )
      return;
    setBusy("download-update");
    const requestVersion = manualCheckVersion.current;
    setDownloadedUpdate(null);
    setUpdateResult({ tone: "pending", text: "正在下载并校验更新…" });
    try {
      const result = await withTimeout(
        invoke<UpdateDownload>("download_update", { expectedVersion: target.latestVersion, expectedPolicyId: target.policyId ?? null }),
        300_000,
        "下载更新超时，请稍后重试",
      );
      if (requestVersion !== manualCheckVersion.current) return;
      setDownloadedUpdate(result);
      const text = `已下载 ${result.fileName}（${formatBytes(result.size)}），校验通过`;
      setUpdateResult({ tone: "success", text });
      setNotice({ tone: "success", text });
      askInstallDownloadedUpdate(result);
    } catch (error) {
      if (requestVersion !== manualCheckVersion.current) return;
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askInstallDownloadedUpdate(downloadOverride?: UpdateDownload | null) {
    const target = downloadOverride ?? downloadedUpdate;
    if (!target || !updateVersionAllowed(target.latestVersion, policyRef.current) || isBusy) return;
    setConfirmation({
      action: "install-update",
      title: target.rollback ? "确认回退并重启" : "安装更新",
      description: target.rollback ? `将安装旧版本 v${target.latestVersion} 并重启。原因：${target.rollback.reason}。安装前会再次验证回退授权，若已发布新版本则停止本次回退。` : `Codey 会先保存未保存的设置，再退出当前实例，安装 ${target.fileName}，然后尝试启动新版。`,
      confirmLabel: target.rollback ? "回退并重启" : "安装并重启",
      run: () => void installDownloadedUpdate(target),
      // 安装包已经下载好，"稍后"只影响自动提示，不影响用户从版本入口手动安装。
      onDismiss: () => writeDeferredVersion(updatePromptKey(target)),
    });
  }

  async function installDownloadedUpdate(downloadOverride?: UpdateDownload | null) {
    const target = downloadOverride ?? downloadedUpdate;
    if (!target || !updateVersionAllowed(target.latestVersion, policyRef.current) || isBusy) return;
    setBusy("install-update");
    setUpdateResult({ tone: "pending", text: "正在启动安装器…" });
    try {
      await beforeInstall();
      await invoke("install_downloaded_update", {
        filePath: target.filePath,
      });
      const text = "正在退出 Codey 并启动安装器…";
      setUpdateResult({ tone: "pending", text });
      setNotice({ tone: "info", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
      setBusy(null);
    }
  }

  return {
    automaticallyChecking,
    updateResult,
    updateCheck,
    downloadedUpdate,
    checkForUpdates,
    downloadUpdate,
    askDownloadUpdate,
    askInstallDownloadedUpdate,
  };
}
