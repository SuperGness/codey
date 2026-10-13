use super::*;

/// 原生 Responses 回包的改写：Grok 的扁平工具名与整数参数，以及协作工具命名空间别名。
/// 协作别名在 xAI 还原之后执行，Grok 线路上同样能还原成 `collaboration`。
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeResponseFix {
    xai: Option<XaiResponseFix>,
    collaboration: Option<CollaborationAlias>,
}

impl From<XaiResponseFix> for NativeResponseFix {
    fn from(xai: XaiResponseFix) -> Self {
        Self {
            xai: Some(xai),
            collaboration: None,
        }
    }
}

pub(crate) fn current_native_response_fix() -> Option<NativeResponseFix> {
    let fix = NativeResponseFix {
        xai: current_xai_response_fix(),
        collaboration: current_collaboration_alias(),
    };
    (fix.xai.is_some() || fix.collaboration.is_some()).then_some(fix)
}

impl NativeResponseFix {
    pub(crate) fn apply(&self, value: &mut Value) -> bool {
        let mut changed = self.xai.as_ref().is_some_and(|fix| fix.apply(value));
        if let Some(alias) = &self.collaboration {
            changed |= alias.apply(value);
        }
        changed
    }

    pub(crate) fn rewrite_json_bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
            return Cow::Borrowed(bytes);
        };
        if !self.apply(&mut value) {
            return Cow::Borrowed(bytes);
        }
        serde_json::to_vec(&value)
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(bytes))
    }
}
