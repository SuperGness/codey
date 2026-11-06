//! 不启动 Codex、不访问外网，使用临时配置测量启动早期的状态初始化。
use super::*;

#[test]
#[ignore = "opt-in isolated startup state benchmark"]
fn startup_state_latency() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));
    store.save(&CodeyConfig::default()).unwrap();
    let mut samples = Vec::new();
    for _ in 0..21 {
        let started = std::time::Instant::now();
        let state = AppState::with_store(ConfigStore::new(store.path().to_path_buf()));
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(state);
    }
    let first = samples.remove(0);
    samples.sort_by(f64::total_cmp);
    println!(
        "BENCH {}",
        json!({"case":"startup_state", "first_sample_ms":first,
        "warm_p50_ms":samples[9], "warm_p95_ms":samples[18], "warm_samples":samples.len()})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_http_client_initialization_is_lazy_and_shared() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::with_store(ConfigStore::new(
        directory.path().join("config.json"),
    )));
    assert!(!state.http_client.initialized());
    let clients = futures_util::future::join_all((0..16).map(|_| state.http_client())).await;
    let first = clients[0].as_ref().unwrap();
    for client in &clients {
        assert!(std::ptr::eq(*first, *client.as_ref().unwrap()));
    }
    assert!(state.http_client.initialized());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in shared HTTP readiness measurement"]
async fn startup_http_readiness_latency() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::with_store(ConfigStore::new(directory.path().join("config.json")));
    let started = std::time::Instant::now();
    state.http_client().await.unwrap();
    let first = started.elapsed().as_secs_f64() * 1000.0;
    let started = std::time::Instant::now();
    state.http_client().await.unwrap();
    println!(
        "BENCH {}",
        json!({"case":"shared_http_readiness", "first_ms":first,
        "cached_ms": started.elapsed().as_secs_f64() * 1000.0})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in loopback client construction comparison"]
async fn startup_loopback_client_latency() {
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..21 {
        for variant in [round % 2, 1 - round % 2] {
            let started = std::time::Instant::now();
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none());
            if variant == 1 {
                builder = builder.tls_built_in_root_certs(false);
            }
            std::hint::black_box(builder.build().unwrap());
            samples[variant].push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    for (variant, mut samples) in samples.into_iter().enumerate() {
        let first = samples.remove(0);
        samples.sort_by(f64::total_cmp);
        println!(
            "BENCH {}",
            json!({"case":"loopback_client", "system_roots":variant == 0,
            "first_sample_ms":first, "warm_p50_ms":samples[9],
            "warm_p95_ms":samples[18], "warm_samples":samples.len()})
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in isolated local router startup measurement"]
async fn startup_local_router_latency() {
    let mut samples = Vec::new();
    for _ in 0..21 {
        let started = std::time::Instant::now();
        let router = crate::local_router::LocalRouter::start(&CodeyConfig::default())
            .await
            .unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        router.stop().await.unwrap();
    }
    let first = samples.remove(0);
    samples.sort_by(f64::total_cmp);
    println!(
        "BENCH {}",
        json!({"case":"local_router_startup", "first_sample_ms":first,
        "warm_p50_ms":samples[9], "warm_p95_ms":samples[18], "warm_samples":samples.len()})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in first CDP discovery against an isolated loopback server"]
async fn startup_cdp_discovery_latency() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        for _ in 0..21 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            assert!(request.starts_with(b"GET /json HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
                .await
                .unwrap();
        }
    });
    let mut samples = Vec::new();
    for _ in 0..21 {
        let started = std::time::Instant::now();
        let targets = codey_runtime_core::cdp::list_targets(port).await.unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        assert!(targets.is_empty());
    }
    server.await.unwrap();
    let first = samples.remove(0);
    samples.sort_by(f64::total_cmp);
    println!(
        "BENCH {}",
        json!({"case":"cdp_discovery", "first_sample_ms":first,
        "warm_p50_ms":samples[9], "warm_p95_ms":samples[18], "warm_samples":samples.len()})
    );
}
