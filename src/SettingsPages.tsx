import type { ComponentProps } from "react";
import { IconLayoutDashboard } from "@tabler/icons-react";

import { invoke } from "./api";
import type { Config } from "./App.types";
import { CodeyPluginsSection } from "./CodeyPluginsSection";
import { FeaturePolicyCard, SubagentPolicyCard } from "./FeaturePolicyCard";
import { CodexExtensionsPage, type ExtensionTransport } from "./features/codex-extensions";
import { ModelSection } from "./ModelSection";
import { OperationsPanel } from "./OperationsPanel";
import { PromptOptimizationCard } from "./PromptOptimizationCard";
import { SettingsPageHeader } from "./SettingsPageHeader";

const extensionRequest: ExtensionTransport = (request) =>
  invoke("codex_extensions", { request });

type ModelProps = ComponentProps<typeof ModelSection>;
type FeatureProps = ComponentProps<typeof FeaturePolicyCard>;
type PromptProps = ComponentProps<typeof PromptOptimizationCard>;
type SubagentProps = ComponentProps<typeof SubagentPolicyCard>;
type OperationsProps = ComponentProps<typeof OperationsPanel>;

export type SettingsSectionInput = {
  config: Config;
  fastContextToolsStatus: FeatureProps["fastContextToolsStatus"];
  operationsStatus: OperationsProps["status"];
  busy: string | null;
  isBusy: boolean;
  pluginMarketplaceStatus: OperationsProps["pluginMarketplaceStatus"];
  onRepairPluginMarketplace: OperationsProps["onRepairPluginMarketplace"];
  onRepairMainProcessInjection: OperationsProps["onRepairMainProcessInjection"];
  onRepairCodexConfig: () => void;
  configRepairNotice: OperationsProps["configRepairNotice"];
  injectionRepairing: boolean;
  onRestart: () => void;
  restartStatusUnknown: boolean;
  showRestartAction: boolean;
  popupContainer: HTMLElement | null;
  onAnalyzeDiagnosticStorage: FeatureProps["onAnalyzeDiagnosticStorage"];
  onConfigChange: FeatureProps["onConfigChange"];
  onAddChannel: FeatureProps["onAddChannel"];
  onChannelChange: FeatureProps["onChannelChange"];
  onRequestRemoveChannel: FeatureProps["onRequestRemoveChannel"];
  clientPlatform?: string;
  officialAccountAvailable: boolean;
  provider: ModelProps["currentProvider"];
  modelState: ModelProps["modelState"];
  dirty: boolean;
  canSyncCurrentProvider: boolean;
  subagentModelOptions: SubagentProps["subagentModelOptions"];
  onToggleLocalRouter: ModelProps["onToggleLocalRouter"];
  onToggleRouteRequestLog: ModelProps["onToggleRouteRequestLog"];
  onOpenUsageAnalysis: ModelProps["onOpenUsageAnalysis"];
  onSaveRoute: ModelProps["onSaveRoute"];
  onSetRouteEnabled: ModelProps["onSetRouteEnabled"];
  onReorderRoute: ModelProps["onReorderRoute"];
  onReorderRouteModels: ModelProps["onReorderRouteModels"];
  onDeleteRoute: ModelProps["onDeleteRoute"];
  onFetchRouteModels: ModelProps["onFetchRouteModels"];
  onOfficialAccountsChanged: ModelProps["onOfficialAccountsChanged"];
  onModelNotice: ModelProps["onNotice"];
  onPromptNotice: PromptProps["onNotice"];
  onSaveOfficialRouteSettings: ModelProps["onSaveOfficialRouteSettings"];
  onSetDefaultModel: ModelProps["onSetDefaultModel"];
  onRequestConfirmation: ModelProps["onRequestConfirmation"];
  onSubagentOptimizationChange: SubagentProps["onSubagentOptimizationChange"];
};

export function buildSettingsSections({
  config,
  fastContextToolsStatus,
  operationsStatus,
  busy,
  isBusy,
  pluginMarketplaceStatus,
  onRepairPluginMarketplace,
  onRepairMainProcessInjection,
  onRepairCodexConfig,
  configRepairNotice,
  injectionRepairing,
  onRestart,
  restartStatusUnknown,
  showRestartAction,
  popupContainer,
  onAnalyzeDiagnosticStorage,
  onConfigChange,
  onAddChannel,
  onChannelChange,
  onRequestRemoveChannel,
  clientPlatform,
  officialAccountAvailable,
  provider,
  modelState,
  dirty,
  canSyncCurrentProvider,
  subagentModelOptions,
  onToggleLocalRouter,
  onToggleRouteRequestLog,
  onOpenUsageAnalysis,
  onSaveRoute,
  onSetRouteEnabled,
  onReorderRoute,
  onReorderRouteModels,
  onDeleteRoute,
  onFetchRouteModels,
  onOfficialAccountsChanged,
  onModelNotice,
  onPromptNotice,
  onSaveOfficialRouteSettings,
  onSetDefaultModel,
  onRequestConfirmation,
  onSubagentOptimizationChange,
}: SettingsSectionInput) {
  return {
    overview: (
      <>
        <SettingsPageHeader
          id="overview-title"
          title="基础功能"
          icon={<IconLayoutDashboard size={15} />}
          description="查看 Codex 运行状态，管理客户端功能与通知。"
        />
        <OperationsPanel
          codexAppPath={config.codexAppPath}
          fastContextToolsStatus={fastContextToolsStatus}
          status={operationsStatus}
          busy={busy}
          isBusy={isBusy}
          pluginMarketplaceStatus={pluginMarketplaceStatus}
          onRepairPluginMarketplace={onRepairPluginMarketplace}
          onRepairMainProcessInjection={onRepairMainProcessInjection}
          onRepairCodexConfig={onRepairCodexConfig}
          configRepairNotice={configRepairNotice}
          injectionRepairing={injectionRepairing}
          onRestart={onRestart}
          restartStatusUnknown={restartStatusUnknown}
          showRestartAction={showRestartAction}
        />
        <FeaturePolicyCard
          config={config}
          fastContextToolsStatus={fastContextToolsStatus}
          isMacClient={clientPlatform === "macos"}
          isWindowsClient={clientPlatform === "windows"}
          cleanupBusy={busy === "clear-diagnostic-storage"}
          onAnalyzeDiagnosticStorage={onAnalyzeDiagnosticStorage}
          popupContainer={popupContainer}
          isBusy={isBusy}
          onConfigChange={onConfigChange}
          onAddChannel={onAddChannel}
          onChannelChange={onChannelChange}
          onRequestRemoveChannel={onRequestRemoveChannel}
        />
      </>
    ),
    models: (
      <ModelSection
        config={config}
        currentProvider={provider}
        officialAccountAvailable={officialAccountAvailable}
        popupContainer={popupContainer}
        modelState={modelState}
        dirty={dirty}
        canSyncCurrentProvider={canSyncCurrentProvider}
        isBusy={isBusy}
        busy={busy}
        showAccountUsageInHeader={config.showAccountUsageInHeader}
        subagentModelOptions={subagentModelOptions}
        onToggleLocalRouter={onToggleLocalRouter}
        onToggleRouteRequestLog={onToggleRouteRequestLog}
        onOpenUsageAnalysis={onOpenUsageAnalysis}
        onSaveRoute={onSaveRoute}
        onSetRouteEnabled={onSetRouteEnabled}
        onReorderRoute={onReorderRoute}
        onReorderRouteModels={onReorderRouteModels}
        onDeleteRoute={onDeleteRoute}
        onFetchRouteModels={onFetchRouteModels}
        onOfficialAccountsChanged={onOfficialAccountsChanged}
        onNotice={onModelNotice}
        onSaveOfficialRouteSettings={onSaveOfficialRouteSettings}
        onSetDefaultModel={onSetDefaultModel}
        onConfigChange={onConfigChange}
        onRequestConfirmation={onRequestConfirmation}
      />
    ),
    prompt: (
      <PromptOptimizationCard
        config={config}
        isBusy={isBusy}
        subagentModelOptions={subagentModelOptions}
        onConfigChange={onConfigChange}
        onNotice={onPromptNotice}
      />
    ),
    subagents: (
      <SubagentPolicyCard
        config={config}
        isBusy={isBusy}
        subagentModelOptions={subagentModelOptions}
        onConfigChange={onConfigChange}
        onSubagentOptimizationChange={onSubagentOptimizationChange}
      />
    ),
    plugins: <CodeyPluginsSection container={popupContainer} />,
    mcp: (active: boolean) => (
      <CodexExtensionsPage kind="mcp" active={active} request={extensionRequest} container={popupContainer} />
    ),
    skills: (active: boolean) => (
      <CodexExtensionsPage kind="skill" active={active} request={extensionRequest} container={popupContainer} />
    ),
  };
}
