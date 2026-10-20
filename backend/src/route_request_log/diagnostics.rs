//! 有界路由诊断只接受内部枚举和计数，不接收请求正文、请求头或凭据。
use super::*;

pub(crate) const MAX_RECORDED_ATTEMPTS: usize = 8;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RouteDecisionReason {
    #[default]
    NotResolved,
    ExplicitAlias,
    RouteHint,
    DefaultOfficial,
    SessionBinding,
    UniqueCandidate,
    AutoReviewOfficial,
    AutoReviewMiscModel,
    PortableTargetHandoff,
    PortableSourceHandoff,
    PortableHandoffUnavailable,
    MissingModel,
    InvalidMetadata,
    AmbiguousModel,
    ModelNotEnabled,
    RouteUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RouteCheckRejection {
    RouteMissing,
    ModelNotEnabled,
}

/// 候选数仅指本次解析查阅的别名或原始模型列表，不扫描整个线路表。
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RouteDecision {
    pub settings_revision: u64,
    pub reason: RouteDecisionReason,
    pub candidate_count: u64,
    pub historical_alias: bool,
    pub defaulted_model: bool,
    pub rejected_hint: Option<RouteCheckRejection>,
    pub rejected_binding: Option<RouteCheckRejection>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptTrigger {
    Initial,
    PluginRetry,
    ReasoningRepair,
    TransportFallback,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptOutcome {
    Pending,
    Succeeded,
    Rejected,
    Failed,
    TimedOut,
    Cancelled,
    Superseded,
    Incomplete,
    DeliveryFailed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptPhase {
    Send,
    ResponseHeaders,
    ResponseBody,
    DownstreamDelivery,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpstreamAttemptLog {
    pub sequence: u32,
    pub transport: UpstreamTransport,
    pub trigger: AttemptTrigger,
    pub started_ms: u64,
    pub duration_ms: Option<u64>,
    pub header_ms: Option<u64>,
    pub status_code: Option<u16>,
    pub outcome: AttemptOutcome,
    pub phase: AttemptPhase,
}
