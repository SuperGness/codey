//! 只使用合成会话和临时数据库，不读取用户会话或启动 Codex。
use super::*;
use std::io::BufWriter;
use std::time::Instant;

fn fixture(items: usize, text_bytes: usize) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model TEXT, model_provider TEXT, rollout_path TEXT);
        INSERT INTO threads VALUES ('saved','database/old','database','session.jsonl');",
    )
    .unwrap();
    let mut file = BufWriter::new(File::create(home.path().join("session.jsonl")).unwrap());
    writeln!(
        file,
        "{}",
        json!({"type":"session_meta","payload":{
            "id":"saved","model_provider":"initial"
        }})
    )
    .unwrap();
    let text = "x".repeat(text_bytes);
    let response = json!({"type":"response_item","payload":{
        "type":"function_call_output","call_id":"call-bench","output":text
    }})
    .to_string();
    let event = json!({"type":"event_msg","payload":{
        "type":"agent_message","message":text
    }})
    .to_string();
    for index in 0..items {
        writeln!(file, "{response}\n{event}").unwrap();
        if index % 32 == 0 {
            writeln!(
                file,
                "{}",
                json!({"type":"turn_context","payload":{
                    "model":"rollout/earlier","effort":"medium"
                }})
            )
            .unwrap();
        }
    }
    writeln!(
        file,
        "{}",
        json!({"type":"compacted","payload":{
            "replacement_history":[{"type":"compaction","encrypted_content":"opaque-bench"}],
            "resume_metadata":{"previous_turn_settings":{"model":"source/model"}}
        }})
    )
    .unwrap();
    write!(
        file,
        "{}",
        json!({"type":"event_msg","timestamp":"2026-01-01T00:00:00Z","payload":{
            "type":"thread_settings_applied","thread_id":"saved","thread_settings":{
                "model":"route/latest","model_provider_id":"latest","reasoning_effort":"xhigh"
            }
        }})
    )
    .unwrap();
    file.flush().unwrap();
    home
}

fn measure<T: PartialEq + std::fmt::Debug>(mut read: impl FnMut() -> T, expected: T) -> Value {
    let mut samples = Vec::new();
    for _ in 0..21 {
        let started = Instant::now();
        let actual = read();
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(actual, expected);
    }
    let first = samples.remove(0);
    samples.sort_by(f64::total_cmp);
    json!({"first_sample_ms":first,"warm_p50_ms":samples[9],
        "warm_p95_ms":samples[18],"warm_samples":samples.len()})
}

#[test]
#[ignore = "opt-in synthetic session metadata benchmark"]
fn session_metadata_latency() {
    for items in [32, 256, 2048] {
        let home = fixture(items, 16 * 1024);
        let bytes = std::fs::metadata(home.path().join("session.jsonl"))
            .unwrap()
            .len();
        let model = measure(
            || read(home.path(), "saved").unwrap(),
            json!({"status":"ok","threadId":"saved","model":"route/latest",
                "modelProvider":"latest","reasoningEffort":"xhigh"}),
        );
        let compaction = measure(
            || native_compaction_source(home.path(), "saved").unwrap(),
            Some("source/model".to_string()),
        );
        println!(
            "BENCH {}",
            json!({"case":"session_metadata","bytes":bytes,"message_pairs":items,
                "model":model,"compaction":compaction})
        );
    }
}

#[test]
#[ignore = "opt-in comparison of rollout read and parse strategies"]
fn session_scan_strategies() {
    for (items, text_bytes) in [(2048, 16 * 1024), (32768, 256)] {
        let home = fixture(items, text_bytes);
        let path = home.path().join("session.jsonl");
        for (capacity, reuse, prefilter) in [
            (8 * 1024, false, false),
            (8 * 1024, true, false),
            (64 * 1024, true, false),
            (256 * 1024, true, false),
            (8 * 1024, true, true),
            (64 * 1024, true, true),
        ] {
            let metrics = measure(
                || {
                    let mut reader = BufReader::with_capacity(capacity, File::open(&path).unwrap());
                    let mut line = String::new();
                    let mut selected = 0;
                    loop {
                        if reuse {
                            line.clear();
                        } else {
                            line = String::new();
                        }
                        if reader.read_line(&mut line).unwrap() == 0 {
                            break;
                        }
                        let record = if prefilter {
                            metadata_record(&line, "turn_context")
                        } else {
                            Some(serde_json::from_str::<Value>(&line).unwrap())
                        };
                        if record.as_ref().is_some_and(|record| {
                            record["type"] == "session_meta"
                                || record["type"] == "turn_context"
                                || own_settings(record, "saved")
                        }) {
                            selected += 1;
                        }
                    }
                    selected
                },
                2 + items.div_ceil(32),
            );
            println!(
                "BENCH {}",
                json!({"case":"session_scan_strategies","bytes":std::fs::metadata(&path).unwrap().len(),
                    "text_bytes":text_bytes,"capacity":capacity,"reuse":reuse,
                    "prefilter":prefilter,"metrics":metrics})
            );
        }
    }
}
