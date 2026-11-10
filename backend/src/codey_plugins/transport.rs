//! 传输实例快照与生命周期回调共用同一执行队列。
use super::lifecycle::LifecyclePlugin;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock, Weak},
};

type Plugins = BTreeMap<String, Arc<LifecyclePlugin>>;
static PLUGINS: OnceLock<arc_swap::ArcSwap<Plugins>> = OnceLock::new();
fn plugins() -> &'static arc_swap::ArcSwap<Plugins> {
    PLUGINS.get_or_init(|| arc_swap::ArcSwap::from_pointee(BTreeMap::new()))
}
pub(super) fn publish(next: Plugins) {
    for (id, old) in plugins().load().iter() {
        if !next.get(id).is_some_and(|new| Arc::ptr_eq(old, new)) {
            old.set_active(false);
            Session(old.clone()).cleanup(codey_plugin_sdk::transport::STOP, serde_json::json!({}));
        }
    }
    for plugin in next.values() {
        plugin.set_active(true);
    }
    plugins().store(Arc::new(next));
}
#[derive(Clone)]
pub(crate) struct Session(Arc<LifecyclePlugin>);

/// 路由固定启用时的实例，弱引用不阻止停用后的销毁。
#[derive(Clone)]
pub(crate) struct Instance(Weak<LifecyclePlugin>);
impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInstance").finish_non_exhaustive()
    }
}
impl Instance {
    pub(crate) fn capture(id: &str) -> Option<Self> {
        Session::open(id)
            .ok()
            .map(|session| Self(Arc::downgrade(&session.0)))
    }
    pub(crate) fn open(&self) -> Result<Session, String> {
        self.0
            .upgrade()
            .filter(|plugin| plugin.is_active())
            .map(Session)
            .ok_or_else(|| "plugin_disabled".into())
    }
}
impl Session {
    pub(crate) fn cleanup(&self, method: &'static str, params: Value) {
        if ![
            codey_plugin_sdk::transport::CANCEL,
            codey_plugin_sdk::transport::STOP,
        ]
        .contains(&method)
        {
            return;
        }
        // 管理接口及响应析构不保证在 Tokio 线程内；共享后备执行器负责短清理回调。
        static CLEANUP_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, std::io::Error>> =
            OnceLock::new();
        let runtime = tokio::runtime::Handle::try_current().ok().or_else(|| {
            CLEANUP_RUNTIME
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .max_blocking_threads(2)
                        .thread_name("codey-plugin-cleanup")
                        .enable_time()
                        .build()
                })
                .as_ref()
                .ok()
                .map(|runtime| runtime.handle().clone())
        });
        if let Some(runtime) = runtime {
            let session = self.clone();
            runtime.spawn(async move {
                let _ = session.call(method, params).await;
            });
        }
    }
    pub(crate) fn open(id: &str) -> Result<Self, String> {
        #[cfg(test)]
        if let Ok(result) = TEST_TRANSPORTS.try_with(|plugins| {
            plugins
                .get(id)
                .cloned()
                .map(Self)
                .ok_or_else(|| "plugin_disabled".into())
        }) {
            return result;
        }
        plugins()
            .load()
            .get(id)
            .cloned()
            .map(Self)
            .ok_or_else(|| "plugin_disabled".into())
    }
    pub(crate) async fn call(&self, method: &'static str, params: Value) -> Result<Value, String> {
        self.0
            .call(method, params, None)
            .await
            .map_err(|error| error.code)
    }
}

// 账号刷新由宿主账号服务串行处理；插件无法指定路径或获取 refresh token。
type AccountFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<codey_plugin_sdk::transport::Credentials, String>>
            + Send,
    >,
>;
type AccountHandler = Arc<dyn Fn(String) -> AccountFuture + Send + Sync>;
static ACCOUNT_HANDLER: std::sync::Mutex<Option<AccountHandler>> = std::sync::Mutex::new(None);
#[allow(dead_code)]
pub(crate) fn set_account_handler(handler: AccountHandler) {
    *ACCOUNT_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}
#[allow(dead_code)]
pub(crate) async fn account_credentials(
    email: &str,
) -> Result<codey_plugin_sdk::transport::Credentials, String> {
    let handler = ACCOUNT_HANDLER
        .lock()
        .map_err(|_| "插件账号服务不可用")?
        .clone()
        .ok_or("插件账号服务尚未就绪")?;
    handler(email.to_string()).await
}

#[cfg(test)]
tokio::task_local! { static TEST_TRANSPORTS: Plugins; }
#[cfg(test)]
// SDK 宿主测试也会编译此模块，但只有后端路由测试使用该辅助函数。
#[allow(dead_code)]
pub(crate) async fn with_test_transport<F: std::future::Future>(
    id: &str,
    plugin: super::lifecycle::TestPlugin,
    future: F,
) -> F::Output {
    TEST_TRANSPORTS
        .scope(BTreeMap::from([(id.to_string(), plugin.plugin)]), future)
        .await
}
#[cfg(test)]
impl Session {
    pub(crate) fn disable_for_test(&self) {
        self.0.set_active(false);
    }
}

#[cfg(all(test, feature = "unit-tests"))]
mod tests {
    use super::*;
    #[test]
    fn stop_cleanup_runs_without_a_caller_runtime() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let plugin = super::super::lifecycle::TestPlugin::new("sync.cleanup", move |method, _| {
            sender.send(method.to_owned()).unwrap();
            Ok(serde_json::json!({}))
        });
        let session = Session(plugin.plugin);
        session.disable_for_test();
        session.cleanup(codey_plugin_sdk::transport::STOP, serde_json::json!({}));
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            codey_plugin_sdk::transport::STOP
        );
    }
}
