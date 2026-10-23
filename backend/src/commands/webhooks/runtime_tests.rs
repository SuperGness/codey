struct GatedWebhookServer {
    url: String,
    gate: Arc<Semaphore>,
    received: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    channel_peak: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl GatedWebhookServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let gate = Arc::new(Semaphore::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let channel_peak = Arc::new(AtomicUsize::new(0));
        let channels = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let server_gate = Arc::clone(&gate);
        let server_received = Arc::clone(&received);
        let server_active = Arc::clone(&active);
        let server_peak = Arc::clone(&peak);
        let server_channel_peak = Arc::clone(&channel_peak);
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (mut socket, _) = result.unwrap();
                        let gate = Arc::clone(&server_gate);
                        let received = Arc::clone(&server_received);
                        let active = Arc::clone(&server_active);
                        let peak = Arc::clone(&server_peak);
                        let channel_peak = Arc::clone(&server_channel_peak);
                        let channels = Arc::clone(&channels);
                        requests.spawn(async move {
                            use tokio::io::AsyncWriteExt;
                            let request = read_http_request(&mut socket).await;
                            let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_string();
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(current, Ordering::SeqCst);
                            {
                                let mut channels = channels.lock().await;
                                let count = channels.entry(path.clone()).or_default();
                                *count += 1;
                                channel_peak.fetch_max(*count, Ordering::SeqCst);
                            }
                            received.fetch_add(1, Ordering::SeqCst);
                            gate.acquire_owned().await.unwrap().forget();
                            active.fetch_sub(1, Ordering::SeqCst);
                            *channels.lock().await.get_mut(&path).unwrap() -= 1;
                            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{\"code\":0}").await;
                        });
                    }
                    _ = requests.join_next(), if !requests.is_empty() => {},
                }
            }
        });
        Self {
            url,
            gate,
            received,
            active,
            peak,
            channel_peak,
            task,
        }
    }

    fn channel(&self, id: &str) -> NotificationChannelConfig {
        NotificationChannelConfig {
            id: id.into(),
            url: format!("{}/{id}", self.url),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        }
    }
}

impl Drop for GatedWebhookServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn wait_for_count(count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while count.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("local delivery should make progress");
}

async fn stored_delivery_states(state: &AppState) -> Vec<String> {
    let path = state
        .store
        .path()
        .with_file_name("notification-outbox.sqlite3");
    // SQLite 等待写事务时仍让 HTTP 模拟服务和投递任务继续运行。
    tokio::task::spawn_blocking(move || {
        let db = rusqlite::Connection::open(path).unwrap();
        db.prepare("SELECT state FROM notification_deliveries ORDER BY state")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .await
    .unwrap()
}

async fn wait_for_delivered(state: &AppState, expected: usize) {
    wait_for_delivery_states(expected, || stored_delivery_states(state)).await;
}

async fn wait_for_delivery_states<F, Fut>(expected: usize, mut read_states: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Vec<String>>,
{
    let mut delivered = 0;
    let mut states = Vec::new();
    loop {
        // 每次确认都需要完整磁盘同步；限制停滞时间，不限制整批投递的吞吐量。
        // 只有成功数量严格增加才重新计时，最多等待 expected 次进展。
        let progress = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                states = read_states().await;
                assert_eq!(
                    states.len(),
                    expected,
                    "unexpected delivery rows: {states:?}"
                );
                let completed = states.iter().filter(|state| *state == "delivered").count();
                if completed == expected || completed > delivered {
                    break completed;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        delivered = progress.unwrap_or_else(|_| {
            panic!("delivery results stalled: delivered {delivered}/{expected}, states={states:?}")
        });
        if delivered == expected {
            break;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn delivery_wait_accepts_progress_across_slow_durable_commits() {
    let started = tokio::time::Instant::now();
    let mut completed = 0;
    wait_for_delivery_states(2, || {
        completed += 1;
        let states = if completed == 1 {
            vec!["delivered".into(), "inflight".into()]
        } else {
            vec!["delivered".into(), "delivered".into()]
        };
        async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            states
        }
    })
    .await;
    assert_eq!(started.elapsed(), Duration::from_secs(4));
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "delivery results stalled: delivered 1/2")]
async fn delivery_wait_repeated_states_do_not_extend_the_stall_deadline() {
    wait_for_delivery_states(2, || async { vec!["delivered".into(), "inflight".into()] }).await;
}

#[tokio::test]
async fn slow_http_does_not_block_enqueue_and_concurrency_is_global_across_events() {
    let directory = tempfile::tempdir().unwrap();
    let server = GatedWebhookServer::start().await;
    let channels = (0..7)
        .map(|i| server.channel(&format!("channel-{i}")))
        .collect();
    let state = channel_delivery_test_state(directory.path(), channels);
    let worker = delivery::start(Arc::clone(&state)).await.unwrap();
    let payload = json!({"sessionId":"session","turnId":"first","sessionName":"First", "confirmedByRollout":true});
    notify_webhook_completion(&state, &payload).await.unwrap();
    wait_for_count(&server.received, 4).await;
    assert_eq!(server.active.load(Ordering::SeqCst), 4);
    let second = json!({"sessionId":"session","turnId":"second","sessionName":"Second", "confirmedByRollout":true});
    let queued = tokio::time::timeout(
        Duration::from_millis(250),
        notify_webhook_completion(&state, &second),
    )
    .await
    .expect("an unrelated enqueue must not wait for HTTP")
    .unwrap();
    assert_eq!(queued["status"], "queued");
    assert_eq!(server.received.load(Ordering::SeqCst), 4);
    assert_eq!(stored_delivery_states(&state).await.len(), 14);
    server.gate.add_permits(14);
    wait_for_delivered(&state, 14).await;
    assert_eq!(server.peak.load(Ordering::SeqCst), 4);
    assert_eq!(server.channel_peak.load(Ordering::SeqCst), 1);
    state.notification_delivery_stop.send_replace(true);
    worker.await.unwrap();
}

#[tokio::test]
async fn successful_http_responses_progress_while_delivery_storage_is_locked() {
    let directory = tempfile::tempdir().unwrap();
    let server = GatedWebhookServer::start().await;
    let channels = (0..4)
        .map(|index| server.channel(&format!("channel-{index}")))
        .collect();
    let state = channel_delivery_test_state(directory.path(), channels);
    notify_webhook_completion(
        &state,
        &json!({"sessionId":"session", "turnId":"turn", "confirmedByRollout":true}),
    )
    .await
    .unwrap();
    let worker = delivery::start(Arc::clone(&state)).await.unwrap();
    wait_for_count(&server.received, 4).await;

    let path = state
        .store
        .path()
        .with_file_name("notification-outbox.sqlite3");
    let (locked_tx, locked_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let storage = tokio::task::spawn_blocking(move || {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE;").unwrap();
        locked_tx.send(()).unwrap();
        let _ = release_rx.blocking_recv();
        db.execute_batch("COMMIT;").unwrap();
    });
    locked_rx.await.unwrap();
    server.gate.add_permits(4);
    // 响应已放行，磁盘锁持续超过测试 HTTP 客户端的 2 秒超时。
    tokio::time::sleep(Duration::from_millis(2200)).await;
    drop(release_tx);
    storage.await.unwrap();
    let delivered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let states = stored_delivery_states(&state).await;
            if states.iter().all(|state| state != "inflight") {
                break states;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    state.notification_delivery_stop.send_replace(true);
    worker.await.unwrap();
    assert_eq!(delivered.unwrap(), vec!["delivered"; 4]);
}

#[tokio::test]
async fn shutdown_cancels_active_http_and_leaves_unsent_work_pending() {
    let directory = tempfile::tempdir().unwrap();
    let server = GatedWebhookServer::start().await;
    let channels = vec![server.channel("single")];
    let state = channel_delivery_test_state(directory.path(), channels.clone());
    for turn in ["first", "second"] {
        notify_webhook_completion(
            &state,
            &json!({"sessionId":"session","turnId":turn,
            "sessionName":"Shutdown", "confirmedByRollout":true}),
        )
        .await
        .unwrap();
    }
    let worker = delivery::start(Arc::clone(&state)).await.unwrap();
    wait_for_count(&server.received, 1).await;
    state.notification_delivery_stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored_delivery_states(&state).await,
        ["pending", "uncertain"]
    );
    assert_eq!(server.received.load(Ordering::SeqCst), 1);
    drop(state);
    let restarted = channel_delivery_test_state(directory.path(), channels);
    server.gate.add_permits(2);
    let worker = delivery::start(Arc::clone(&restarted)).await.unwrap();
    wait_for_count(&server.received, 2).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while stored_delivery_states(&restarted).await != ["delivered", "uncertain"] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    restarted.notification_delivery_stop.send_replace(true);
    worker.await.unwrap();
    assert_eq!(server.received.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_queue_write_does_not_reserve_waiting_or_acknowledge_terminal() {
    let directory = tempfile::tempdir().unwrap();
    let server = MockWebhookServer::start(vec!["200 OK"]).await;
    let state = channel_delivery_test_state(directory.path(), vec![server.channel("channel")]);
    fs::create_dir(directory.path().join("notification-outbox.sqlite3")).unwrap();
    let waiting = json!({"sessionId":"session","turnId":"turn","waitingId":"approval","sessionName":"Failure"});
    assert!(notify_webhook_waiting(&state, &waiting).await.is_err());
    assert!(
        state
            .persisted_waiting_notifications
            .lock()
            .await
            .is_empty()
    );
    let completed = json!({"sessionId":"session","turnId":"turn","sessionName":"Failure","confirmedByRollout":true});
    assert!(notify_webhook_completion(&state, &completed).await.is_err());
    fs::remove_dir(directory.path().join("notification-outbox.sqlite3")).unwrap();
    assert_eq!(
        notify_webhook_waiting(&state, &waiting).await.unwrap()["status"],
        "queued"
    );
    assert_eq!(
        notify_webhook_completion(&state, &completed).await.unwrap()["status"],
        "queued"
    );
    assert!(server.requests.lock().await.is_empty());
}

#[tokio::test]
async fn resolved_waiting_is_cancelled_without_suppressing_a_new_question() {
    use crate::notifications::outbox::DeliveryOutcome;

    let directory = tempfile::tempdir().unwrap();
    let server = MockWebhookServer::start(vec!["200 OK"]).await;
    let state = channel_delivery_test_state(directory.path(), vec![server.channel("waiting")]);
    for waiting_id in ["answered", "current"] {
        notify_webhook_waiting(
            &state,
            &json!({"sessionId":"session","turnId":"turn",
            "waitingId":waiting_id,"sessionName":"Waiting"}),
        )
        .await
        .unwrap();
    }
    let events = RecentSessionEvents {
        session_statuses: Arc::new(HashMap::from([(
            "session".into(),
            SessionLifecycleStatus::Waiting,
        )])),
        pending_approvals: vec![PendingApproval {
            session_id: "session".into(),
            turn_id: "turn".into(),
            waiting_id: "current".into(),
            duration_ms: 0,
        }],
        ..Default::default()
    };
    *state.notification_waiting_snapshot.write().await =
        WaitingNotificationSnapshot::from_events(&events);
    let mut results = drive_due(&state).await;
    results.extend(drive_due(&state).await);
    assert_eq!(
        results
            .iter()
            .filter(|outcome| matches!(outcome, DeliveryOutcome::Cancelled))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|outcome| matches!(outcome, DeliveryOutcome::Delivered))
            .count(),
        1
    );
    assert_eq!(server.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn watcher_refreshes_waiting_before_recovery_and_delivers_appended_failure() {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let server = MockWebhookServer::start(vec!["200 OK"]).await;
    let state = channel_delivery_test_state(directory.path(), vec![server.channel("channel")]);
    let home = notification_codex_home(&state);
    fs::create_dir_all(home.join("sessions")).unwrap();
    let rollout = home.join("sessions/rollout-session.jsonl");
    fs::write(
        &rollout,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn\"}}\n",
    )
    .unwrap();
    let db = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, archived INTEGER, updated_at INTEGER);").unwrap();
    db.execute(
        "INSERT INTO threads VALUES (?1, ?2, 0, ?3)",
        rusqlite::params![
            "session",
            rollout.to_string_lossy(),
            unix_timestamp_seconds()
        ],
    )
    .unwrap();
    drop(db);
    // 模拟重启时仍在队列中、但当前 rollout 已无对应提问的等待通知。
    notify_webhook_waiting(
        &state,
        &json!({"sessionId":"session","turnId":"turn",
        "waitingId":"answered","sessionName":"Recovery"}),
    )
    .await
    .unwrap();
    let scan = start_recent_session_scan_at(Default::default(), home);
    start_waiting_webhook_watcher(&state, scan).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while stored_delivery_states(&state).await != ["cancelled"] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("startup must reconcile waiting before sending");
    assert!(server.requests.lock().await.is_empty());

    let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    writeln!(
        file,
        "{}",
        json!({"type":"event_msg", "timestamp":chrono::Utc::now().to_rfc3339(),
        "payload":{"type":"task_complete","turn_id":"turn","error":"test failure"}})
    )
    .unwrap();
    // 保持 writer 打开，验证真实文件事件能够推动扫描和投递。
    file.flush().unwrap();
    // Windows 的目录变更通知依赖写缓存刷新；File::flush 本身不会刷新它。
    #[cfg(windows)]
    file.sync_all().unwrap();
    let appended_at = tokio::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.requests.lock().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the native write event should reach the local webhook");
    eprintln!(
        "open rollout append to local webhook request: {:?}",
        appended_at.elapsed()
    );
    let delivered = tokio::time::timeout(Duration::from_secs(5), async {
        while stored_delivery_states(&state).await != ["cancelled", "delivered"] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if delivered.is_err() {
        let snapshot = pending_approval::RecentSessionEventCache::default()
            .refresh(&notification_codex_home(&state));
        panic!(
            "appended failure was not delivered: states={:?}, snapshot={snapshot:?}, watcher_finished={:?}",
            stored_delivery_states(&state).await,
            state
                .waiting_watcher_task
                .lock()
                .await
                .as_ref()
                .map(|task| task.is_finished())
        );
    }
    stop_waiting_webhook_watcher(&state).await;
    assert_eq!(server.requests.lock().await.len(), 1);
    assert!(
        server.requests.lock().await[0]
            .to_string()
            .contains("会话失败")
    );
}

fn open_empty_wal_session_index(path: &Path) -> rusqlite::Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let database = rusqlite::Connection::open(path).unwrap();
    database.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, archived INTEGER, updated_at INTEGER, thread_source TEXT);",
    ).unwrap();
    database
}

async fn assert_native_new_session_delivery(commit_delay: Duration, two_databases: bool) {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let server = MockWebhookServer::start(vec!["200 OK"]).await;
    let state = channel_delivery_test_state(directory.path(), vec![server.channel("new-session")]);
    let home = notification_codex_home(&state);
    fs::create_dir_all(home.join("sessions")).unwrap();
    let mut databases = vec![open_empty_wal_session_index(&home.join("state_5.sqlite"))];
    if two_databases {
        databases.push(open_empty_wal_session_index(
            &home.join("sqlite/sessions.sqlite"),
        ));
    }
    let scan = start_recent_session_scan_at(Default::default(), home.clone());
    start_waiting_webhook_watcher(&state, scan).await;
    // 只是等待启动扫描结束，不是延迟断言；全量并行测试时阻塞线程池会排队。
    tokio::time::timeout(Duration::from_secs(10), async {
        while state.notification_outbox.get().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the watcher should finish its startup scan with empty indexes");
    assert!(server.requests.lock().await.is_empty());

    // 新日期目录和 rollout 均在监听启动后创建，数据库与文件 writer 始终保持打开。
    let date_directory = home.join("sessions/2026/10/19");
    fs::create_dir_all(&date_directory).unwrap();
    let mut writers = Vec::new();
    for (index, database) in databases.iter().enumerate() {
        let session_id = format!("new-root-{index}");
        let rollout = date_directory.join(format!("rollout-{session_id}.jsonl"));
        let mut writer = fs::File::create(&rollout).unwrap();
        let error = if index == 0 {
            None
        } else {
            Some("test failure")
        };
        for record in [
            json!({"type":"session_meta", "payload":{"id":session_id, "thread_source":"user"}}),
            json!({"type":"event_msg", "timestamp":chrono::Utc::now().to_rfc3339(),
                "payload":{"type":"task_started","turn_id":"new-turn"}}),
            json!({"type":"event_msg", "timestamp":chrono::Utc::now().to_rfc3339(),
                "payload":{"type":"task_complete","turn_id":"new-turn","error":error}}),
        ] {
            writeln!(writer, "{record}").unwrap();
        }
        writer.flush().unwrap();
        // Windows 需要刷新写缓存，才能及时产生目录变更通知。
        #[cfg(windows)]
        writer.sync_all().unwrap();
        writers.push(writer);
        database.execute_batch("BEGIN IMMEDIATE;").unwrap();
        database
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, 0, ?3, 'user')",
                rusqlite::params![
                    session_id,
                    rollout.to_string_lossy(),
                    unix_timestamp_seconds()
                ],
            )
            .unwrap();
    }
    // 先产生文件提示，再提交索引，覆盖第一次发现时还读不到新会话的情况。
    tokio::time::sleep(commit_delay).await;
    assert!(server.requests.lock().await.is_empty());
    let committed_at = tokio::time::Instant::now();
    for database in &databases {
        database.execute_batch("COMMIT;").unwrap();
    }
    let delivered = tokio::time::timeout(Duration::from_secs(2), async {
        while server.requests.lock().await.len() < databases.len() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let latency = committed_at.elapsed();
    if delivered.is_ok() {
        wait_for_delivered(&state, databases.len()).await;
    }
    // 失败时也回收原生监听和发送任务，然后报告实际收到的请求数。
    stop_waiting_webhook_watcher(&state).await;
    let requests = server.requests.lock().await;
    assert!(
        delivered.is_ok(),
        "new indexed sessions did not reach the local webhook within 2 s: requests={}, indexes={}, states={:?}",
        requests.len(),
        databases.len(),
        stored_delivery_states(&state).await
    );
    assert_eq!(requests.len(), databases.len());
    assert!(
        requests
            .iter()
            .any(|request| request["card"]["header"]["title"]["content"] == "Codex会话完成")
    );
    if two_databases {
        assert!(
            requests
                .iter()
                .any(|request| request["card"]["header"]["title"]["content"] == "Codex会话失败")
        );
    }
    eprintln!(
        "new session native delivery: indexes={}, commit_delay={commit_delay:?}, latency={latency:?}",
        databases.len()
    );
    drop(writers);
    drop(databases);
}

#[tokio::test]
async fn native_new_date_directory_and_open_wal_deliver_new_session() {
    assert_native_new_session_delivery(Duration::ZERO, false).await;
}

#[tokio::test]
async fn native_new_session_discovery_survives_dirty_hint_before_wal_commit() {
    assert_native_new_session_delivery(Duration::from_millis(150), false).await;
}

#[tokio::test]
async fn native_home_and_sqlite_indexes_deliver_success_and_failure_after_delayed_commits() {
    assert_native_new_session_delivery(Duration::from_millis(150), true).await;
}
