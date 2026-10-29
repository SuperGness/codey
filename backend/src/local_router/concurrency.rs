use super::*;

const OFFICIAL_QUEUE_TIMEOUT: Duration = Duration::from_secs(60);
const WAITING: u8 = 0;
const GRANTED: u8 = 1;
const CLAIMED: u8 = 2;
const REMOVED: u8 = 3;

/// 与配置快照分离，配置重载和失败回滚均保留正在运行的许可。
#[derive(Default)]
pub(crate) struct OfficialConcurrency {
    gates: Mutex<HashMap<String, Arc<OfficialGate>>>,
}

#[derive(Default)]
struct GateState {
    limit: u32,
    active: usize,
    enabled: bool,
    sequence: u64,
    waiters: BTreeMap<u64, Arc<Waiter>>,
}

#[derive(Default)]
struct OfficialGate {
    state: Mutex<GateState>,
}

struct Waiter {
    state: std::sync::atomic::AtomicU8,
    wake: tokio::sync::Notify,
    enqueued: Instant,
}

pub(crate) struct OfficialPermit {
    gate: Arc<OfficialGate>,
}

pub(crate) struct OfficialWait {
    gate: Arc<OfficialGate>,
    id: u64,
    waiter: Arc<Waiter>,
}

pub(crate) enum OfficialAdmission {
    Ready(OfficialPermit),
    Waiting(OfficialWait),
}

#[derive(Debug)]
pub(crate) struct OfficialRouteUnavailable;

impl std::fmt::Display for OfficialRouteUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("排队期间官方线路已停用或变更，请重新选择线路")
    }
}
impl std::error::Error for OfficialRouteUnavailable {}

impl OfficialConcurrency {
    pub(crate) fn update(&self, snapshot: &RouterSnapshot) {
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut limits = HashMap::<&str, u32>::new();
        for route in snapshot.routes.values() {
            if let Some(key) = route.concurrency_key.as_deref() {
                // 重复映射同一账号时采用最严格的非零限制。
                limits
                    .entry(key)
                    .and_modify(|limit| {
                        if *limit == 0
                            || (route.max_concurrency > 0 && route.max_concurrency < *limit)
                        {
                            *limit = route.max_concurrency;
                        }
                    })
                    .or_insert(route.max_concurrency);
            }
        }
        for (key, gate) in gates.iter() {
            let mut state = gate
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.enabled = limits.contains_key(key.as_str());
            if let Some(limit) = limits.get(key.as_str()) {
                state.limit = *limit;
                state.dispatch();
            } else {
                for waiter in std::mem::take(&mut state.waiters).into_values() {
                    waiter
                        .state
                        .store(REMOVED, std::sync::atomic::Ordering::Release);
                    waiter.wake.notify_one();
                }
            }
        }
        // 删除已无人持有的失效账号，避免多次导入删除后累积注册表。
        gates.retain(|key, gate| limits.contains_key(key.as_str()) || Arc::strong_count(gate) > 1);
        for (key, limit) in limits {
            gates.entry(key.to_string()).or_insert_with(|| {
                Arc::new(OfficialGate {
                    state: Mutex::new(GateState {
                        enabled: true,
                        limit,
                        ..GateState::default()
                    }),
                })
            });
        }
    }

    pub(crate) fn acquire(&self, key: &str) -> Result<OfficialAdmission> {
        let gate = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
            .ok_or(OfficialRouteUnavailable)?;
        let mut state = gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.enabled {
            return Err(OfficialRouteUnavailable.into());
        }
        if state.waiters.is_empty() && (state.limit == 0 || state.active < state.limit as usize) {
            state.active += 1;
            drop(state);
            return Ok(OfficialAdmission::Ready(OfficialPermit { gate }));
        }
        let id = state.sequence;
        state.sequence = state
            .sequence
            .checked_add(1)
            .expect("official queue sequence exhausted");
        let waiter = Arc::new(Waiter {
            state: std::sync::atomic::AtomicU8::new(WAITING),
            wake: tokio::sync::Notify::new(),
            enqueued: Instant::now(),
        });
        state.waiters.insert(id, Arc::clone(&waiter));
        drop(state);
        Ok(OfficialAdmission::Waiting(OfficialWait {
            gate,
            id,
            waiter,
        }))
    }

    pub(crate) fn status(&self) -> Value {
        let gates = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Value::Array(gates.iter().map(|(key, gate)| {
            let state = gate.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            json!({
                "accountKey": key, "maxConcurrency": state.limit,
                "running": state.active, "waiting": state.waiters.len(), "enabled": state.enabled,
                "oldestWaitMs": state.waiters.first_key_value().map(|(_, waiter)| waiter.enqueued.elapsed().as_millis() as u64),
            })
        }).collect())
    }
}

impl GateState {
    fn dispatch(&mut self) {
        while self.enabled && (self.limit == 0 || self.active < self.limit as usize) {
            let Some((_, waiter)) = self.waiters.pop_first() else {
                break;
            };
            self.active += 1;
            waiter
                .state
                .store(GRANTED, std::sync::atomic::Ordering::Release);
            waiter.wake.notify_one();
        }
    }
}

impl OfficialWait {
    async fn wait(self) -> Result<OfficialPermit> {
        loop {
            self.waiter.wake.notified().await;
            let state = self
                .gate
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match self.waiter.state.load(std::sync::atomic::Ordering::Acquire) {
                GRANTED => {
                    self.waiter
                        .state
                        .store(CLAIMED, std::sync::atomic::Ordering::Release);
                    drop(state);
                    return Ok(OfficialPermit {
                        gate: Arc::clone(&self.gate),
                    });
                }
                REMOVED => return Err(OfficialRouteUnavailable.into()),
                _ => {}
            }
        }
    }
}

impl Drop for OfficialWait {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.waiter.state.load(std::sync::atomic::Ordering::Acquire) {
            WAITING => {
                state.waiters.remove(&self.id);
            }
            GRANTED => {
                state.active -= 1;
            }
            _ => return,
        }
        self.waiter
            .state
            .store(REMOVED, std::sync::atomic::Ordering::Release);
        state.dispatch();
    }
}

impl Drop for OfficialPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
        state.dispatch();
    }
}

pub(crate) struct RequestExecutionPermit {
    _official: Option<OfficialPermit>,
    _connection: OwnedSemaphorePermit,
}

struct QueueObservation {
    started: Instant,
    probe: Option<RouteRequestLogProbe>,
}

impl Drop for QueueObservation {
    fn drop(&mut self) {
        if let Some(probe) = &self.probe {
            probe.record_concurrency_wait(self.started.elapsed());
        }
    }
}

pub(crate) async fn await_http_admission<T, F>(stream: &mut TcpStream, future: F) -> Result<T>
where
    T: Send,
    F: std::future::Future<Output = T> + Send,
{
    tokio::pin!(future);
    tokio::select! {
        biased;
        result = await_http_reset(stream) => {
            Err(anyhow::Error::new(result.unwrap_err()).context(DownstreamClosed))
        }
        result = &mut future => {
            match http_reset_error(stream) {
                Ok(None) => Ok(result),
                Ok(Some(error)) | Err(error) => {
                    Err(anyhow::Error::new(error).context(DownstreamClosed))
                }
            }
        }
    }
}

fn http_reset_error(stream: &TcpStream) -> std::io::Result<Option<std::io::Error>> {
    if let Some(error) = stream.take_error()? {
        return Ok(Some(error));
    }
    // Mio 将 Windows 的 AFD_POLL_ABORT 映射成 WRITE_CLOSED，SO_ERROR 可能仍为空。
    // FIN 仅设置 READ_CLOSED；检查写关闭不会消费流水线字节或误判写入半关闭。
    #[cfg(windows)]
    if let Some(ready) =
        futures_util::FutureExt::now_or_never(stream.ready(tokio::io::Interest::WRITABLE))
        && ready?.is_write_closed()
    {
        return Ok(Some(std::io::ErrorKind::ConnectionReset.into()));
    }
    // Windows 在 FIN 之后可能不再报告 RST 的 SO_ERROR / AFD_POLL_ABORT。
    // 直接读取 TCP 状态，CLOSE_WAIT 仍可写回响应，CLOSED 才取消等待。
    #[cfg(windows)]
    if windows_http_tcp_state(stream)? == Some(windows::Win32::Networking::WinSock::TCPSTATE_CLOSED)
    {
        return Ok(Some(std::io::ErrorKind::ConnectionReset.into()));
    }
    Ok(None)
}

#[cfg(windows)]
fn windows_http_tcp_state(
    stream: &TcpStream,
) -> std::io::Result<Option<windows::Win32::Networking::WinSock::TCPSTATE>> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{
        SIO_TCP_INFO, SOCKET, SOCKET_ERROR, TCP_INFO_v0, WSAEINVAL, WSAEOPNOTSUPP, WSAGetLastError,
        WSAIoctl,
    };

    let version = 0u32;
    let mut info = TCP_INFO_v0::default();
    let mut returned = 0u32;
    // 同步查询只写入栈上的固定大小缓冲区，不发送或读取 HTTP 字节，
    // 也不改变 Tokio/Mio 的就绪注册。SIO_TCP_INFO 不需要管理员权限。
    let result = unsafe {
        WSAIoctl(
            SOCKET(stream.as_raw_socket() as usize),
            SIO_TCP_INFO,
            Some(std::ptr::from_ref(&version).cast()),
            std::mem::size_of_val(&version) as u32,
            Some(std::ptr::from_mut(&mut info).cast()),
            std::mem::size_of_val(&info) as u32,
            &mut returned,
            None,
            None,
        )
    };
    if result == SOCKET_ERROR {
        let error = unsafe { WSAGetLastError() };
        // 旧版 Windows 或第三方 Winsock provider 不支持此查询时保留原有监测。
        if error == WSAEOPNOTSUPP || error == WSAEINVAL {
            return Ok(None);
        }
        return Err(std::io::Error::from_raw_os_error(error.0));
    }
    if returned < std::mem::size_of_val(&info.State) as u32 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(Some(info.State))
}

async fn await_http_reset(stream: &TcpStream) -> std::io::Result<()> {
    #[cfg(windows)]
    loop {
        if let Some(error) = http_reset_error(stream)? {
            return Err(error);
        }
        // 普通可写就绪会一直保留；定时重查关闭标记和 TCP 状态，不清除就绪。
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Unix 的错误就绪不受 FIN 或未消费字节影响，虚假唤醒由 WouldBlock 清除。
    #[cfg(not(windows))]
    stream
        .async_io(tokio::io::Interest::ERROR, || {
            Err::<(), _>(
                stream
                    .take_error()?
                    .unwrap_or_else(|| std::io::ErrorKind::WouldBlock.into()),
            )
        })
        .await
}

impl RouterServer {
    pub(crate) async fn admit_official<D: ResponsesDownstream + ?Sized>(
        &self,
        route: &RouteTarget,
        downstream: &mut D,
    ) -> Result<RequestExecutionPermit> {
        let admission = match route.concurrency_key.as_deref() {
            Some(key) => match self.official_concurrency.acquire(key) {
                Ok(admission) => Some(admission),
                Err(_) => return self.write_route_unavailable(downstream, route).await,
            },
            None => None,
        };
        let connection = if matches!(admission, Some(OfficialAdmission::Waiting(_))) {
            None
        } else {
            Arc::clone(&self.connection_limit).try_acquire_owned().ok()
        };
        let permit = match (admission, connection) {
            (Some(OfficialAdmission::Ready(permit)), Some(connection)) => RequestExecutionPermit {
                _official: Some(permit),
                _connection: connection,
            },
            (None, Some(connection)) => RequestExecutionPermit {
                _official: None,
                _connection: connection,
            },
            (admission, _) => {
                let observation = QueueObservation {
                    started: Instant::now(),
                    probe: downstream.request_log_probe().cloned(),
                };
                let result = tokio::time::timeout(
                    OFFICIAL_QUEUE_TIMEOUT,
                    downstream.wait_for_admission(async {
                        let permit = match admission {
                            Some(OfficialAdmission::Ready(permit)) => Some(permit),
                            Some(OfficialAdmission::Waiting(waiter)) => Some(waiter.wait().await?),
                            None => None,
                        };
                        let connection = Arc::clone(&self.connection_limit)
                            .acquire_owned()
                            .await
                            .map_err(|_| OfficialRouteUnavailable)?;
                        Ok::<_, anyhow::Error>(RequestExecutionPermit {
                            _official: permit,
                            _connection: connection,
                        })
                    }),
                )
                .await;
                let probe = observation.probe.clone();
                drop(observation);
                match result {
                    Ok(Ok(Ok(permit))) => permit,
                    Err(_) => {
                        downstream
                            .write_admission_error(
                                504,
                                "queue_wait_timeout",
                                "等待线路执行额度超时".into(),
                                Some(route),
                            )
                            .await?;
                        return Err(AdmissionEnded.into());
                    }
                    Ok(Ok(Err(_))) => return self.write_route_unavailable(downstream, route).await,
                    Ok(Err(error))
                        if error.is::<DownstreamResponseInterrupt>()
                            || error.is::<DownstreamClosed>() =>
                    {
                        if let Some(probe) = &probe {
                            probe.mark_cancelled("official_queue_cancelled");
                            probe.finish_cancelled();
                        }
                        if error.is::<DownstreamResponseInterrupt>() {
                            downstream
                                .write_admission_error(
                                    499,
                                    "queue_cancelled",
                                    "排队请求已取消".into(),
                                    Some(route),
                                )
                                .await?;
                            return Err(AdmissionEnded.into());
                        }
                        return Err(error);
                    }
                    Ok(Err(error)) => return Err(error),
                }
            }
        };
        // 只改上限不会改变上下文指纹；账号、凭据平面或地址变更必须重新发起请求。
        let valid = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .get(&route.provider_id)
            .is_some_and(|current| {
                current.concurrency_key == route.concurrency_key
                    && current.context_config == route.context_config
            });
        if !valid {
            return self.write_route_unavailable(downstream, route).await;
        }
        Ok(permit)
    }

    async fn write_route_unavailable<D: ResponsesDownstream + ?Sized>(
        &self,
        downstream: &mut D,
        route: &RouteTarget,
    ) -> Result<RequestExecutionPermit> {
        downstream
            .write_admission_error(
                409,
                "queued_route_changed",
                OfficialRouteUnavailable.to_string(),
                Some(route),
            )
            .await?;
        Err(AdmissionEnded.into())
    }
}

#[derive(Debug)]
pub(crate) struct AdmissionEnded;
impl std::fmt::Display for AdmissionEnded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("排队请求已结束")
    }
}
impl std::error::Error for AdmissionEnded {}

#[cfg(test)]
mod tests;
