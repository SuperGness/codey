use super::*;

#[test]
#[ignore = "仅由格式化子进程测试启动"]
fn formatter_process_child() {
    let Some(marker) = std::env::var_os("CODEY_FORMATTER_TEST_MARKER") else {
        return;
    };
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    fs::write(&marker, &input).unwrap();
    if std::env::var_os("CODEY_FORMATTER_TEST_WAIT").is_some() {
        loop {
            std::thread::sleep(Duration::from_millis(20));
            fs::write(&marker, b"running").unwrap();
        }
    }
    std::io::stdout().write_all(&input).unwrap();
    std::process::exit(0);
}

fn formatter_child_command(marker: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "conversation_git::tests::formatter_process_child",
            "--ignored",
            "--nocapture",
        ])
        .env("CODEY_FORMATTER_TEST_MARKER", marker);
    command
}

#[test]
fn formatter_process_receives_eof_and_collects_output_without_pipes() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("input");
    let input = b"fn main() {}\n".repeat(8192);
    let output = run_formatter(
        &mut formatter_child_command(&marker),
        &input,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(fs::read(marker).unwrap(), input);
    assert!(output.ends_with(&input));
}

#[test]
fn formatter_timeout_terminates_and_reaps_process() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("heartbeat");
    let mut command = formatter_child_command(&marker);
    command.env("CODEY_FORMATTER_TEST_WAIT", "1");
    let error = run_formatter(&mut command, b"input", Duration::from_millis(250)).unwrap_err();
    assert!(format!("{error:#}").contains("历史格式化校验超时"));
    let after = fs::read(&marker).ok();
    let modified = fs::metadata(&marker).and_then(|m| m.modified()).ok();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fs::read(&marker).ok(), after);
    assert_eq!(
        fs::metadata(marker).and_then(|m| m.modified()).ok(),
        modified
    );
}

#[test]
fn commit_analysis_preserves_complete_file_diffs() {
    let first = format!("diff --git a/first b/first\n{}\n", "a".repeat(20_000));
    let second = format!("diff --git a/second b/second\n{}\n", "b".repeat(20_000));
    let diff = format!("{first}{second}");
    assert_eq!(analysis_inputs(&diff).unwrap(), vec![first, second]);
    assert_eq!(analysis_inputs(&diff).unwrap().concat(), diff);
    assert!(
        analysis_inputs(&format!(
            "diff --git a/large b/large\n{}",
            "x".repeat(28_000)
        ))
        .is_err()
    );
    assert!(analysis_inputs("").is_err());
}

#[test]
fn large_file_analysis_preserves_each_complete_hunk_and_repeats_file_metadata() {
    let header =
        "diff --git a/large.rs b/large.rs\nindex abc..def 100644\n--- a/large.rs\n+++ b/large.rs\n";
    let first = format!("@@ -1 +1,2 @@\n-old\n+{}\n", "中".repeat(18_000));
    let second = format!("@@ -8 +9,2 @@\n-old\n+{}\n", "文".repeat(18_000));
    let diff = format!("{header}{first}{second}");
    let chunks = analysis_inputs(&diff).unwrap();
    assert_eq!(
        chunks,
        vec![format!("{header}{first}"), format!("{header}{second}")]
    );
    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 28_000));
    assert!(analysis_inputs(&format!("{header}@@ -1 +1 @@\n+{}", "x".repeat(28_000))).is_err());
}

#[test]
#[ignore = "仅供显式指定本地会话的只读诊断"]
fn diagnose_local_conversation_git_status() {
    let home = PathBuf::from(std::env::var("CODEY_GIT_DIAGNOSTIC_HOME").expect("指定诊断目录"));
    let session = std::env::var("CODEY_GIT_DIAGNOSTIC_SESSION").expect("指定诊断会话");
    if std::env::var_os("CODEY_GIT_DIAGNOSTIC_VALIDATE").is_none() {
        match status(&home, &session) {
            Ok(value) => eprintln!("{value}"),
            Err(error) => panic!("{error:#}"),
        }
    }
    if std::env::var_os("CODEY_GIT_DIAGNOSTIC_VALIDATE").is_some() {
        let (_, workspace, _) = source(&home, &session).unwrap();
        let root = PathBuf::from(git_text(&workspace, &["rev-parse", "--show-toplevel"]).unwrap())
            .canonicalize()
            .unwrap();
        let head = git_text(&root, &["rev-parse", "--verify", "HEAD"]).unwrap();
        let history = match history::recover(&home, &session, &root, &workspace) {
            Ok(value) => value,
            Err(error) => panic!("历史恢复失败: {error:#}"),
        };
        if let (Ok(base), Ok(target), Ok(path)) = (
            std::env::var("CODEY_GIT_DIAGNOSTIC_BASE"),
            std::env::var("CODEY_GIT_DIAGNOSTIC_TARGET"),
            std::env::var("CODEY_GIT_DIAGNOSTIC_PATH"),
        ) {
            let before = head_entry(&root, &base, &path).unwrap();
            let after = head_entry(&root, &target, &path).unwrap();
            eprintln!(
                "提交历史校验: {:?}",
                history.replay(&path, before.as_ref(), after.as_ref())
            );
            eprintln!(
                "提交反向校验: {}",
                history.changes_in_head(&path, after.as_ref())
            );
            return;
        }
        for path in history.paths() {
            let before = head_entry(&root, &head, path).unwrap();
            let after = disk_entry(&root, path).unwrap();
            if before != after {
                if std::env::var_os("CODEY_GIT_DIAGNOSTIC_RECORDS").is_some() {
                    eprintln!(
                        "记录校验 {path}: {:?}",
                        history.diagnostic_replay(path, before.as_ref())
                    );
                }
                eprintln!(
                    "历史校验 {path}: {:?}",
                    history
                        .replay(path, before.as_ref(), after.as_ref())
                        .map_err(|e| format!("{e:#}"))
                );
                eprintln!(
                    "分离校验 {path}: {:?}",
                    history
                        .isolate_changes(&root, path, before.as_ref(), after.as_ref())
                        .map(|entry| digest(&entry.bytes))
                        .map_err(|e| format!("{e:#}"))
                );
            }
        }
        match tracking::changes(&home, &session, &root, &workspace, &head) {
            Ok(changes) => eprintln!(
                "已验证文件：{:?}",
                changes
                    .iter()
                    .map(|c| (&c.path, c.after != c.disk))
                    .collect::<Vec<_>>()
            ),
            Err(error) => panic!("{error:#}"),
        }
    }
}

#[test]
#[ignore = "仅供显式指定本地会话的只读预览诊断"]
fn diagnose_local_conversation_git_snapshot() {
    let home = PathBuf::from(std::env::var("CODEY_GIT_DIAGNOSTIC_HOME").expect("指定诊断目录"));
    let session = std::env::var("CODEY_GIT_DIAGNOSTIC_SESSION").expect("指定诊断会话");
    let prepared = snapshot(&home, &session).unwrap_or_else(|error| panic!("{error:#}"));
    eprintln!(
        "完整预览校验：{:?}",
        prepared
            .changes
            .iter()
            .map(|change| (&change.path, change.after != change.disk))
            .collect::<Vec<_>>()
    );
}

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";
const CHILD: &str = "33333333-3333-4333-8333-333333333333";
const RUNTIME: &str = "conversation-git-test";

fn shared_native_repo() -> (Repo, String, String) {
    let repo = Repo::new();
    let base = format!(
        "old\n{}end\n",
        (2..15).map(|i| format!("line{i}\n")).collect::<String>()
    );
    fs::write(repo.root.join("owned.txt"), &base).unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "shared baseline"], None, None).unwrap();
    git(&repo.root, &["push"], None, None).unwrap();
    let own = base.replacen("old\n", "mine\n", 1);
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-old\n+mine\n", 1),
    );
    fs::write(repo.root.join("owned.txt"), &own).unwrap();
    repo.fastctx(repo.hook(OTHER, "mcp__codey_fastctx__replace", json!({
        "path":repo.root.join("owned.txt"), "pattern":"end", "replacement":"outside", "literal":true
    })));
    let disk = own.replace("end", "outside");
    (repo, own, disk)
}

#[test]
fn shared_file_commits_only_conversation_hunks_and_keeps_the_remainder() {
    let (repo, own, disk) = shared_native_repo();
    fs::write(repo.root.join("other.txt"), "staged\n").unwrap();
    git(&repo.root, &["add", "--", "other.txt"], None, None).unwrap();
    fs::write(repo.root.join("other.txt"), "unstaged\n").unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(prepared.public()["partialFiles"], json!(["owned.txt"]));
    assert_eq!(
        prepared.changes[0].after.as_ref().unwrap().bytes,
        own.as_bytes()
    );
    assert_eq!(
        prepared.changes[0].disk.as_ref().unwrap().bytes,
        disk.as_bytes()
    );
    assert!(!prepared.diff.contains("outside"));
    assert_eq!(
        commit_and_push(&prepared, "chore(files): 更新本对话测试", None).unwrap()["status"],
        "pushed"
    );
    assert_eq!(
        git(&repo.root, &["show", "HEAD:owned.txt"], None, None).unwrap(),
        own.as_bytes()
    );
    assert_eq!(
        fs::read(repo.root.join("owned.txt")).unwrap(),
        disk.as_bytes()
    );
    assert_eq!(
        git(&repo.root, &["show", ":owned.txt"], None, None).unwrap(),
        own.as_bytes()
    );
    assert_eq!(
        git(&repo.root, &["show", ":other.txt"], None, None).unwrap(),
        b"staged\n"
    );
    assert_eq!(
        fs::read(repo.root.join("other.txt")).unwrap(),
        b"unstaged\n"
    );
    assert!(
        status(&repo.home, SESSION).is_err(),
        "本对话已提交，不能把剩余改动显示为本对话改动"
    );
}

#[test]
fn shared_file_isolates_mixed_native_patch_and_fastctx_history() {
    let (repo, own, disk) = shared_native_repo();
    repo.history(
        SESSION,
        None,
        "apply_patch",
        json!(
            "*** Begin Patch\n*** Update File: owned.txt\n@@\n-mine\n+intermediate\n*** End Patch"
        ),
        json!("Success. Updated the following files:\nM owned.txt"),
        2,
    );
    repo.history(SESSION, Some("mcp__codey_fastctx"), "replace",
        json!({"path":repo.root.join("owned.txt"), "pattern":"intermediate", "replacement":"final own", "literal":true}),
        json!(format!("{}: 1 replacement\n\n(Complete: 1 replacement in 1 file.)", repo.root.join("owned.txt").display())), 3);
    let selected = own.replacen("mine", "final own", 1);
    let complete = disk.replacen("mine", "final own", 1);
    fs::write(repo.root.join("owned.txt"), &complete).unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(prepared.public()["partialFiles"], json!(["owned.txt"]));
    assert_eq!(
        prepared.changes[0].after.as_ref().unwrap().bytes,
        selected.as_bytes()
    );
    assert_eq!(
        prepared.changes[0].disk.as_ref().unwrap().bytes,
        complete.as_bytes()
    );
    assert!(!prepared.diff.contains("outside"));
    assert_eq!(
        commit_and_push(&prepared, "fix(test): 更新本对话测试", None).unwrap()["status"],
        "pushed"
    );
    assert_eq!(
        git(&repo.root, &["show", "HEAD:owned.txt"], None, None).unwrap(),
        selected.as_bytes()
    );
    assert_eq!(
        fs::read(repo.root.join("owned.txt")).unwrap(),
        complete.as_bytes()
    );
    assert!(
        status(&repo.home, SESSION).is_err(),
        "提交后只剩其他对话改动"
    );
}

#[test]
fn resumed_segments_restore_prior_edits_and_reject_mismatched_identity() {
    for scenario in ["valid", "workspace", "ordinal", "identity"] {
        let repo = Repo::new();
        repo.old_patch(SESSION, "owned.txt", "old", "middle", 1);
        repo.old_patch(SESSION, "owned.txt", "middle", "new", 2);
        let original = repo
            .home
            .join("sessions")
            .join(format!("rollout-{SESSION}.jsonl"));
        let mut records: Vec<Value> = fs::read_to_string(&original)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        records[0]["ordinal"] = json!(0);
        records[0]["payload"]["timestamp"] = json!("2026-10-08T00:00:00.000Z");
        let mut restored = records[0].clone();
        restored["ordinal"] = json!(104);
        restored["payload"]["timestamp"] = json!("2026-10-08T00:00:01.750Z");
        match scenario {
            "workspace" => restored["payload"]["cwd"] = json!(repo.remote),
            "ordinal" => {
                restored.as_object_mut().unwrap().remove("ordinal");
            }
            "identity" => restored["payload"]["id"] = json!(OTHER),
            _ => {}
        }
        let resumed = repo
            .home
            .join("sessions")
            .join(format!("rollout-{SESSION}_{CHILD}.jsonl"));
        let encode = |items: &[Value]| {
            items
                .iter()
                .map(|item| format!("{item}\n"))
                .collect::<String>()
        };
        fs::write(&original, encode(&records[..3])).unwrap();
        fs::write(&resumed, format!("{restored}\n{}", encode(&records[3..]))).unwrap();
        rusqlite::Connection::open(repo.home.join("state_5.sqlite"))
            .unwrap()
            .execute(
                "UPDATE threads SET rollout_path=?1 WHERE id=?2",
                [resumed.to_str().unwrap(), SESSION],
            )
            .unwrap();
        if scenario == "valid" {
            let prepared = snapshot(&repo.home, SESSION).unwrap();
            assert_eq!(prepared.changes[0].after.as_ref().unwrap().bytes, b"new\n");
            let late = json!({"ordinal":104,"type":"response_item","payload":{"type":"custom_tool_call","call_id":"old-tail","name":"apply_patch","input":"invalid patch"}});
            repo.append_record(SESSION, &late);
            assert!(snapshot(&repo.home, SESSION).is_ok());
        } else {
            assert!(snapshot(&repo.home, SESSION).is_err(), "{scenario}");
        }
    }
}

#[test]
fn sequential_patch_and_command_receipts_recover_only_patch_files() {
    let repo = Repo::new();
    let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch";
    let code = format!(
        "text(await tools.apply_patch({}));\ntext(await tools.exec_command({{cmd:\"node --test test.mjs\",yield_time_ms:1000}}));",
        serde_json::to_string(patch).unwrap()
    );
    let output = json!([{"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"input_text","text":"{}"},{"type":"input_text","text":"{\"exit_code\":0}"}]);
    repo.history(SESSION, None, "exec", json!(code), output, 1);
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    fs::write(repo.root.join("other.txt"), "another conversation\n").unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(prepared.public()["files"], json!(["owned.txt"]));
    assert!(!prepared.diff.contains("another conversation"));
}

#[test]
fn history_cache_checks_content_even_when_file_size_and_time_are_preserved() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    assert!(snapshot(&repo.home, SESSION).is_ok());
    let path = repo
        .home
        .join("sessions")
        .join(format!("rollout-{SESSION}.jsonl"));
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let original = fs::read_to_string(&path).unwrap();
    let changed = original.replace("+new", "+bad");
    assert_ne!(changed, original);
    assert_eq!(changed.len(), original.len());
    fs::write(&path, changed).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let result = snapshot(&repo.home, SESSION);
    assert!(result.is_err(), "缓存修改后的结果: {result:?}");
}

#[test]
fn shared_fastctx_history_requires_reversible_edits_and_matching_receipts() {
    for scenario in [
        "regex",
        "count",
        "case-insensitive",
        "collision",
        "changed",
        "incomplete",
    ] {
        let (repo, _, disk) = shared_native_repo();
        let mut args = json!({"path":repo.root.join("owned.txt"), "pattern":"mine", "replacement":"final own", "literal":true});
        let mut count = 1;
        match scenario {
            "regex" => args["literal"] = json!(false),
            "count" => count = 2,
            "case-insensitive" => args["case_insensitive"] = json!(true),
            "collision" => args["replacement"] = json!("line8"),
            _ => {}
        }
        repo.history(
            SESSION,
            Some("mcp__codey_fastctx"),
            "replace",
            args,
            json!(format!(
                "{}: {count} replacements\n\n(Complete: {count} replacements in 1 file.)",
                repo.root.join("owned.txt").display()
            )),
            2,
        );
        let complete = match scenario {
            "collision" => disk.replace("mine", "line8"),
            "changed" => disk.replace("mine", "different edit"),
            _ => disk.replace("mine", "final own"),
        };
        fs::write(repo.root.join("owned.txt"), &complete).unwrap();
        if scenario == "incomplete" {
            let hook = repo.hook(SESSION, "mcp__codey_fastctx__replace", json!({"path":repo.root.join("owned.txt"), "pattern":"line8", "replacement":"pending", "literal":true}));
            repo.observe(&hook).unwrap();
        }
        let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
        let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
        assert!(snapshot(&repo.home, SESSION).is_err(), "{scenario}");
        assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
        assert_eq!(
            fs::read(repo.root.join("owned.txt")).unwrap(),
            complete.as_bytes()
        );
    }
}

#[test]
fn shared_file_refuses_overlap_reverted_hunks_and_staged_contents() {
    for scenario in ["overlap", "reverted", "staged"] {
        let (repo, _, disk) = shared_native_repo();
        match scenario {
            "overlap" => fs::write(
                repo.root.join("owned.txt"),
                disk.replacen("mine", "different", 1),
            )
            .unwrap(),
            "reverted" => {
                fs::write(repo.root.join("owned.txt"), disk.replacen("mine", "old", 1)).unwrap()
            }
            "staged" => {
                git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
            }
            _ => unreachable!(),
        }
        let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
        let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
        let before = fs::read(repo.root.join("owned.txt")).unwrap();
        assert!(snapshot(&repo.home, SESSION).is_err(), "{scenario}");
        assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
        assert_eq!(fs::read(repo.root.join("owned.txt")).unwrap(), before);
    }
}

#[test]
fn committed_native_history_survives_unique_context_line_shifts() {
    let (repo, own, _) = shared_native_repo();
    let committed = format!("prefix\n{own}");
    fs::write(repo.root.join("owned.txt"), &committed).unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "commit shifted content"],
        None,
        None,
    )
    .unwrap();
    fs::write(
        repo.root.join("owned.txt"),
        committed.replace("end", "outside"),
    )
    .unwrap();
    let history = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
    let head = head_entry(&repo.root, "HEAD", "owned.txt").unwrap();
    assert!(history.changes_in_head("owned.txt", head.as_ref()));
    assert!(tracking::dirty_paths(&repo.home, SESSION, &repo.root, &repo.root, "HEAD").is_err());
    fs::write(repo.root.join("owned.txt"), format!("mine\n{committed}")).unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "duplicate context"],
        None,
        None,
    )
    .unwrap();
    let head = head_entry(&repo.root, "HEAD", "owned.txt").unwrap();
    assert!(!history.changes_in_head("owned.txt", head.as_ref()));
}

#[test]
fn external_native_artifact_does_not_block_repository_history() {
    let (repo, _, _) = shared_native_repo();
    repo.append_record(
        SESSION,
        &repo.native_change(
            SESSION,
            "/tmp/codey-preview-artifact.patch",
            "@@ -1 +1 @@\n-old\n+new\n",
            2,
        ),
    );
    let history = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
    assert_eq!(
        history.paths().cloned().collect::<Vec<_>>(),
        vec!["owned.txt"]
    );
}

#[test]
fn complete_history_resolves_repeated_context_but_never_isolates_it() {
    let repo = Repo::new();
    let base = "anchor\nsame\ngap\nsame\nend\n";
    fs::write(repo.root.join("owned.txt"), base).unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "repeated baseline"],
        None,
        None,
    )
    .unwrap();
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -2 +2 @@\n-same\n+mine\n", 1),
    );
    // 空原生 diff 不能打断此前成功编辑，也不能凭空产生其他文件归属。
    repo.append_record(SESSION, &repo.native_change(SESSION, "owned.txt", "", 2));
    let own = "anchor\nmine\ngap\nsame\nend\n";
    fs::write(repo.root.join("owned.txt"), own).unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(
        prepared.changes[0].after.as_ref().unwrap().bytes,
        own.as_bytes()
    );
    fs::write(repo.root.join("owned.txt"), own.replace("end", "outside")).unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
    assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
    assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
    assert!(
        fs::read_to_string(repo.root.join("owned.txt"))
            .unwrap()
            .contains("outside")
    );
}

#[test]
fn invalid_own_summary_requires_complete_history_proof_even_when_expected_matches_disk() {
    for scenario in ["own", "other", "missing", "unrecorded"] {
        let repo = Repo::new();
        let base = "anchor\nsame\ngap\nsame\nend\n";
        fs::write(repo.root.join("owned.txt"), base).unwrap();
        git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
        git(
            &repo.root,
            &["commit", "-m", "repeated baseline"],
            None,
            None,
        )
        .unwrap();
        if scenario != "missing" {
            repo.append_record(
                SESSION,
                &repo.native_change(SESSION, "owned.txt", "@@ -2 +2 @@\n-same\n+mine\n", 1),
            );
        }
        let own = "anchor\nmine\ngap\nsame\nend\n";
        let disk = if scenario == "unrecorded" {
            own.replace("end", "outside")
        } else {
            own.to_string()
        };
        fs::write(repo.root.join("owned.txt"), &disk).unwrap();
        let directory = repo.home.join("codey-conversation-git-v2");
        fs::create_dir_all(&directory).unwrap();
        // 模拟 Hook 已记录最终摘要，但执行前后校验未能确认归属的情况。
        fs::write(
            directory.join(format!(
                "{}.json",
                digest(repo.root.as_os_str().as_encoded_bytes())
            )),
            serde_json::to_vec(&json!({"files":{"owned.txt":{
                "sessions": if scenario == "other" { vec![SESSION, OTHER] } else { vec![SESSION] },
                "baseline":{"mode":"100644","hash":digest(base.as_bytes())},
                "expected":{"mode":"100644","hash":digest(disk.as_bytes())},
                "valid":false
            }},"pending":{}}))
            .unwrap(),
        )
        .unwrap();
        let prepared = snapshot(&repo.home, SESSION);
        if scenario == "own" {
            let prepared = prepared.unwrap();
            assert_eq!(
                prepared.changes[0].after.as_ref().unwrap().bytes,
                own.as_bytes()
            );
            assert_eq!(prepared.changes[0].after, prepared.changes[0].disk);
        } else {
            assert!(prepared.is_err(), "{scenario} 不得通过完整归属校验");
        }
        assert_eq!(
            fs::read(repo.root.join("owned.txt")).unwrap(),
            disk.as_bytes()
        );
    }
}

#[test]
fn repeated_context_search_refuses_overflow_instead_of_truncating_candidates() {
    let repo = Repo::new();
    let base = "same\n".repeat(17);
    fs::write(repo.root.join("owned.txt"), &base).unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "many candidates"], None, None).unwrap();
    // 记录坐标落在文件之外，只能退回按内容定位；17 个相同行超过候选上限时必须整体拒绝。
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -40 +40 @@\n-same\n+mine\n", 1),
    );
    fs::write(
        repo.root.join("owned.txt"),
        base.replacen("same", "mine", 1),
    )
    .unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
fn full_file_history_can_verify_regex_receipts_without_weakening_partial_isolation() {
    for count in [1, 2] {
        let repo = Repo::new();
        repo.history(
            SESSION,
            Some("mcp__codey_fastctx"),
            "replace",
            json!({"path":repo.root.join("owned.txt"), "pattern":"o(l)d", "replacement":"n${1}ew"}),
            json!(format!(
                "{}: {count} replacement\n\n(Complete: {count} replacement in 1 file.)",
                repo.root.join("owned.txt").display()
            )),
            1,
        );
        fs::write(repo.root.join("owned.txt"), "nlew\n").unwrap();
        assert_eq!(snapshot(&repo.home, SESSION).is_ok(), count == 1);
        fs::write(repo.root.join("owned.txt"), "nlew\noutside\n").unwrap();
        assert!(snapshot(&repo.home, SESSION).is_err());
        assert_eq!(
            git(&repo.root, &["show", "HEAD:owned.txt"], None, None).unwrap(),
            b"old\n"
        );
        assert_eq!(
            fs::read(repo.root.join("owned.txt")).unwrap(),
            b"nlew\noutside\n"
        );
    }
}

#[test]
fn binary_patch_marker_inside_text_does_not_block_commit_analysis() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "GIT binary patch", 1);
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert!(
        prepared
            .diff
            .lines()
            .any(|line| line == "+GIT binary patch")
    );
}

#[test]
fn fixed_multiple_patch_wrapper_keeps_following_formatter_record() {
    let repo = Repo::new();
    fs::write(repo.root.join("owned.rs"), "fn main(){let value=1;}\n").unwrap();
    git(&repo.root, &["add", "--", "owned.rs"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "rust baseline"], None, None).unwrap();
    repo.append_record(
        SESSION,
        &repo.native_change(
            SESSION,
            "owned.rs",
            "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
            1,
        ),
    );
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "other.txt", "@@ -1 +1 @@\n-other\n+updated\n", 2),
    );
    let first = "*** Begin Patch\n*** Update File: owned.rs\n@@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n*** End Patch";
    let second = "*** Begin Patch\n*** Update File: other.txt\n@@\n-other\n+updated\n*** End Patch";
    let code = format!(
        "text(await tools.apply_patch({}));\ntext(await tools.apply_patch({}));\ntext(await tools.exec_command({{cmd:\"rustfmt --edition 2024 owned.rs\",workdir:{}}}));",
        json!(first),
        json!(second),
        json!(repo.root)
    );
    repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:00.500Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"two-patches-fmt","input":code}}));
    repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:03.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"two-patches-fmt","output":[{"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":"{}"},{"type":"text","text":"{}"},{"type":"text","text":"{\"exit_code\":0}"}]}}));
    let formatted = format_rust(&repo.root, "2024", b"fn main(){let value=2;}\n").unwrap();
    fs::write(repo.root.join("owned.rs"), &formatted).unwrap();
    fs::write(repo.root.join("other.txt"), "updated\n").unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(prepared.changes.len(), 2);
    assert_eq!(
        prepared
            .changes
            .iter()
            .find(|change| change.path == "owned.rs")
            .unwrap()
            .after
            .as_ref()
            .unwrap()
            .bytes,
        formatted
    );
}

#[test]
fn formatter_after_interleaved_checks_and_patch_keeps_ordered_receipts() {
    for outcome in ["success", "failed", "missing", "script"] {
        let repo = Repo::new();
        let base = "fn main(){let value=1;}\n";
        let edited = "fn main(){let value=2;}\n";
        fs::write(repo.root.join("owned.rs"), base).unwrap();
        git(&repo.root, &["add", "--", "owned.rs"], None, None).unwrap();
        git(&repo.root, &["commit", "-m", "baseline"], None, None).unwrap();
        repo.append_record(
            SESSION,
            &repo.native_change(
                SESSION,
                "owned.rs",
                "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
                1,
            ),
        );
        let patch = "*** Begin Patch\n*** Update File: owned.rs\n@@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n*** End Patch";
        let mut code = format!(
            "text(await tools.exec_command({}));\ntext(await tools.apply_patch({}));\ntext(await tools.exec_command({}));",
            json!({"cmd":"git status --short"}),
            json!(patch),
            json!({"cmd":"rustfmt --edition 2024 owned.rs"})
        );
        if outcome == "script" {
            code.push_str("\nunknownFunction();");
        }
        let mut content = vec![
            json!({"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"}),
            json!({"type":"text","text":"{\"exit_code\":0}"}),
            json!({"type":"text","text":"{}"}),
            json!({"type":"text","text":json!({"exit_code":if outcome == "failed" {1} else {0}}).to_string()}),
        ];
        if outcome == "missing" {
            content.remove(2);
        }
        repo.history(
            SESSION,
            None,
            "exec",
            json!(code),
            json!({"content":content}),
            2,
        );
        let formatted = format_rust(&repo.root, "2024", edited.as_bytes()).unwrap();
        fs::write(repo.root.join("owned.rs"), &formatted).unwrap();
        // 过时的其他会话摘要只能由本会话完整重放的证明替代。
        let directory = repo.home.join("codey-conversation-git-v2");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(format!("{}.json", digest(repo.root.as_os_str().as_encoded_bytes()))),
            serde_json::to_vec(&json!({"files":{"owned.rs":{
                "sessions":[OTHER], "baseline":{"mode":"100644","hash":digest(b"obsolete baseline")},
                "expected":{"mode":"100644","hash":digest(b"obsolete result")}, "valid":false
            }},"pending":{}})).unwrap()).unwrap();
        let prepared = snapshot(&repo.home, SESSION);
        assert_eq!(prepared.is_ok(), outcome == "success", "{outcome}");
        if let Ok(prepared) = prepared {
            assert_eq!(prepared.changes[0].after.as_ref().unwrap().bytes, formatted);
        }
        assert_eq!(fs::read(repo.root.join("owned.rs")).unwrap(), formatted);
    }
}

#[test]
fn native_history_replays_unique_context_after_head_line_shift() {
    let repo = Repo::new();
    fs::write(repo.root.join("owned.txt"), "prefix\nold\n").unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "shift baseline"], None, None).unwrap();
    git(&repo.root, &["push"], None, None).unwrap();
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-old\n+mine\n", 1),
    );
    fs::write(repo.root.join("owned.txt"), "prefix\nmine\n").unwrap();
    let snapshot = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(
        snapshot.changes[0].after.as_ref().unwrap().bytes,
        b"prefix\nmine\n"
    );
    fs::write(repo.root.join("owned.txt"), "old\nold\n").unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "duplicate baseline"],
        None,
        None,
    )
    .unwrap();
    fs::write(repo.root.join("owned.txt"), "old\nmine\n").unwrap();
    let before = fs::read(repo.root.join("owned.txt")).unwrap();
    let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    assert!(super::snapshot(&repo.home, SESSION).is_ok());
    assert_eq!(fs::read(repo.root.join("owned.txt")).unwrap(), before);
    assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
    assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
}

#[test]
fn history_replays_successful_explicit_formatter_after_committed_add() {
    for outcome in ["success", "failed", "shell", "running"] {
        let repo = Repo::new();
        let base = "fn main(){let value=1;}\n";
        fs::write(repo.root.join("owned.rs"), base).unwrap();
        git(&repo.root, &["add", "--", "owned.rs"], None, None).unwrap();
        git(&repo.root, &["commit", "-m", "Rust baseline"], None, None).unwrap();
        git(&repo.root, &["push"], None, None).unwrap();
        let path = repo.root.join("owned.rs").to_string_lossy().to_string();
        let mut add = repo.native_change(SESSION, "owned.rs", "", 1);
        add["payload"]["item"]["changes"][&path] = json!({"type":"add", "content":base});
        repo.append_record(SESSION, &add);
        repo.append_record(
            SESSION,
            &repo.native_change(
                SESSION,
                "owned.rs",
                "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
                2,
            ),
        );
        let command = if outcome == "shell" {
            "rustfmt --edition 2024 owned.rs; touch other.txt"
        } else {
            "rustfmt --edition 2024 owned.rs"
        };
        let code = format!(
            "text(await tools.exec_command({{cmd:{},workdir:{}}}));\ntext(await tools.write_stdin({{session_id:100,chars:\"\"}}));",
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&repo.root).unwrap()
        );
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:03.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"formatter", "input":code}}));
        let receipt = if outcome == "running" {
            json!({"session_id":100})
        } else {
            json!({"exit_code":if outcome == "failed" {1} else {0}})
        };
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:03.200Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"formatter","output":[{"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":receipt.to_string()},{"type":"text","text":"{\"exit_code\":0}"}]}}));
        let formatted = format_rust(&repo.root, "2024", b"fn main(){let value=2;}\n").unwrap();
        fs::write(repo.root.join("owned.rs"), &formatted).unwrap();
        fs::write(repo.root.join("other.txt"), "outside\n").unwrap();
        let result = snapshot(&repo.home, SESSION);
        if outcome == "success" {
            let prepared = result.unwrap();
            assert_eq!(prepared.changes.len(), 1);
            assert_eq!(prepared.changes[0].path, "owned.rs");
            assert_eq!(prepared.changes[0].after.as_ref().unwrap().bytes, formatted);
            commit_and_push(&prepared, "fix: 更新测试数值并格式化", None).unwrap();
            assert_eq!(fs::read(repo.root.join("other.txt")).unwrap(), b"outside\n");
            assert_eq!(
                git(&repo.root, &["show", "HEAD:owned.rs"], None, None).unwrap(),
                formatted
            );
        } else {
            assert!(result.is_err(), "{outcome}");
            assert_eq!(fs::read(repo.root.join("owned.rs")).unwrap(), formatted);
            assert_eq!(
                git(&repo.root, &["show", "HEAD:owned.rs"], None, None).unwrap(),
                base.as_bytes()
            );
        }
    }
}

#[test]
fn wrapped_rustfmt_after_readonly_checks_recovers_format() {
    let repo = Repo::new();
    let base = "fn main(){let value=1;}\n";
    fs::write(repo.root.join("owned.rs"), base).unwrap();
    git(&repo.root, &["add", "--", "owned.rs"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "Rust baseline"], None, None).unwrap();
    git(&repo.root, &["push"], None, None).unwrap();
    repo.append_record(
        SESSION,
        &repo.native_change(
            SESSION,
            "owned.rs",
            "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
            1,
        ),
    );
    let command = format!(
        "cd {} && git diff --stat && rustfmt --edition 2024 owned.rs 2>&1 | head -5; git diff --stat",
        repo.root.display()
    );
    repo.append_record(
        SESSION,
        &json!({"timestamp":"2026-10-08T00:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"wrapped-fmt","arguments":json!({"cmd":command}).to_string()}}),
    );
    repo.append_record(
        SESSION,
        &json!({"timestamp":"2026-10-08T00:00:03.200Z","type":"response_item","payload":{"type":"function_call_output","call_id":"wrapped-fmt","output":"Chunk ID: aaa\nWall time: 0.1 seconds\nProcess exited with code 0\nOriginal token count: 0\nOutput:\n"}}),
    );
    let formatted = format_rust(&repo.root, "2024", b"fn main(){let value=2;}\n").unwrap();
    fs::write(repo.root.join("owned.rs"), &formatted).unwrap();
    let recovered = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
    let before = head_entry(&repo.root, "HEAD", "owned.rs").unwrap();
    let after = disk_entry(&repo.root, "owned.rs").unwrap();
    assert!(
        recovered
            .replay("owned.rs", before.as_ref(), after.as_ref())
            .is_ok()
    );
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().changes[0]
            .after
            .as_ref()
            .unwrap()
            .bytes,
        formatted
    );
}

#[test]
fn historical_cargo_format_tracks_only_prior_owned_files_and_resolves_polls() {
    for outcome in ["success", "failed", "running", "unsafe"] {
        let repo = Repo::new();
        fs::create_dir_all(repo.root.join("backend/src")).unwrap();
        fs::write(
            repo.root.join("Cargo.toml"),
            "[workspace.package]\nedition = '2024'\n",
        )
        .unwrap();
        fs::write(
            repo.root.join("backend/Cargo.toml"),
            "[package]\nname = 'codey'\nedition.workspace = true\n",
        )
        .unwrap();
        let path = "backend/src/owned.rs";
        fs::write(repo.root.join(path), "fn main(){let value=1;}\n").unwrap();
        git(&repo.root, &["add", "--", path], None, None).unwrap();
        git(&repo.root, &["commit", "-m", "baseline"], None, None).unwrap();
        repo.append_record(
            SESSION,
            &repo.native_change(
                SESSION,
                path,
                "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
                1,
            ),
        );
        let command = if outcome == "unsafe" {
            "cargo fmt -p codey; true"
        } else {
            "cargo fmt -p codey && cargo test -p codey --lib 2>&1 | tail -6"
        };
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"fmt","arguments":json!({"cmd":command,"workdir":repo.root}).to_string()}}));
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:02.100Z","type":"response_item","payload":{"type":"function_call_output","call_id":"fmt","output":"Chunk ID: aaa\nWall time: 30 seconds\nProcess running with session ID 10\nOriginal token count: 0\nOutput:\n"}}));
        if outcome != "running" {
            repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","call_id":"poll","arguments":"{\"session_id\":10,\"chars\":\"\"}"}}));
            let code = if outcome == "failed" { 1 } else { 0 };
            repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:03.100Z","type":"response_item","payload":{"type":"function_call_output","call_id":"poll","output":format!("Chunk ID: bbb\nWall time: 1 seconds\nProcess exited with code {code}\nOriginal token count: 0\nOutput:\n")}}));
        }
        let formatted = format_rust(&repo.root, "2024", b"fn main(){let value=2;}\n").unwrap();
        fs::write(repo.root.join(path), &formatted).unwrap();
        let other = "backend/src/other.rs";
        fs::write(repo.root.join(other), "fn other(){}\n").unwrap();
        let recovered = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
        assert!(!recovered.paths().any(|path| path == other));
        let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
        let before = head_entry(&repo.root, &head, path).unwrap();
        let after = disk_entry(&repo.root, path).unwrap();
        assert_eq!(
            recovered
                .replay(path, before.as_ref(), after.as_ref())
                .is_ok(),
            outcome == "success",
            "{outcome}"
        );
        assert_eq!(fs::read(repo.root.join(other)).unwrap(), b"fn other(){}\n");
    }
}

#[test]
fn wrapped_cargo_all_format_recovers_committed_edits_without_claiming_other_files() {
    for outcome in [
        "success",
        "immediate",
        "failed",
        "running",
        "unsafe",
        "extra",
        "tty",
        "shell",
        "receipt",
    ] {
        let repo = Repo::new();
        fs::create_dir_all(repo.root.join("backend/src")).unwrap();
        fs::write(
            repo.root.join("Cargo.toml"),
            "[workspace.package]\nedition = '2024'\n",
        )
        .unwrap();
        fs::write(
            repo.root.join("backend/Cargo.toml"),
            "[package]\nname = 'codey'\nedition.workspace = true\n",
        )
        .unwrap();
        let path = "backend/src/owned.rs";
        let own = "fn main(){let value=2;}\n";
        fs::write(repo.root.join(path), own).unwrap();
        git(&repo.root, &["add", "--", path], None, None).unwrap();
        git(
            &repo.root,
            &["commit", "-m", "already committed"],
            None,
            None,
        )
        .unwrap();
        repo.append_record(
            SESSION,
            &repo.native_change(
                SESSION,
                path,
                "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
                1,
            ),
        );
        let other = "backend/src/other.rs";
        fs::write(repo.root.join(other), own).unwrap();
        repo.session(OTHER, None);
        repo.append_record(
            OTHER,
            &repo.native_change(
                OTHER,
                other,
                "@@ -1 +1 @@\n-fn main(){let value=1;}\n+fn main(){let value=2;}\n",
                1,
            ),
        );
        let format_args = match outcome {
            "unsafe" => json!({"cmd":"cargo fmt --all; true"}),
            "tty" => json!({"cmd":"cargo fmt --all","tty":true}),
            "shell" => json!({"cmd":"cargo fmt --all","shell":"/bin/sh"}),
            _ => json!({"cmd":"cargo fmt --all"}),
        };
        let mut code = format!(
            "text(await tools.exec_command({format_args}));\ntext(await tools.exec_command({}));\ntext(await tools.exec_command({}));\ntext(await tools.exec_command({}));",
            json!({"cmd":"cargo fmt --all -- --check"}),
            json!({"cmd":"git diff --check"}),
            json!({"cmd":"git diff --stat"})
        );
        if outcome == "extra" {
            code.push_str("\ntext(await tools.exec_command({cmd:\"rm owned.rs\"}));");
        }
        let envelope =
            json!({"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"});
        let block = |value: Value| json!({"type":"text","text":value.to_string()});
        let initial = if outcome == "immediate" {
            json!({"exit_code":0})
        } else {
            json!({"session_id":10})
        };
        let mut content = vec![
            envelope.clone(),
            block(initial),
            block(json!({"exit_code":0})),
            block(json!({"exit_code":0})),
            block(json!({"exit_code":0})),
        ];
        if outcome == "receipt" {
            content.pop();
        }
        repo.history(
            SESSION,
            None,
            "exec",
            json!(code),
            json!({"content":content}),
            2,
        );
        if outcome != "running" && outcome != "immediate" {
            let code = format!(
                "text(await tools.write_stdin({}));\ntext(await tools.exec_command({}));\ntext(await tools.exec_command({}));",
                json!({"session_id":10,"chars":""}),
                json!({"cmd":"cargo fmt --all -- --check"}),
                json!({"cmd":"git diff --check"})
            );
            repo.history(SESSION, None, "exec", json!(code), json!({"content":[envelope,block(json!({"exit_code":if outcome == "failed" {1} else {0}})),block(json!({"exit_code":0})),block(json!({"exit_code":0}))]}), 3);
        }
        let formatted = format_rust(&repo.root, "2024", own.as_bytes()).unwrap();
        fs::write(repo.root.join(path), &formatted).unwrap();
        fs::write(repo.root.join(other), &formatted).unwrap();
        let recovered = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
        assert!(!recovered.paths().any(|path| path == other));
        let before = head_entry(&repo.root, "HEAD", path).unwrap();
        let after = disk_entry(&repo.root, path).unwrap();
        assert_eq!(
            recovered
                .replay(path, before.as_ref(), after.as_ref())
                .is_ok(),
            matches!(outcome, "success" | "immediate"),
            "{outcome}"
        );
        if matches!(outcome, "success" | "immediate") {
            assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
            assert_eq!(snapshot(&repo.home, SESSION).unwrap().changes.len(), 1);
        }
        assert_eq!(fs::read(repo.root.join(other)).unwrap(), formatted);
    }
}

#[test]
fn committed_other_session_claim_does_not_block_proven_later_history() {
    let repo = Repo::new();
    repo.fastctx(repo.hook(OTHER, "mcp__codey_fastctx__replace", json!({"path":repo.root.join("owned.txt"), "pattern":"old", "replacement":"outside", "literal":true})));
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "other conversation"],
        None,
        None,
    )
    .unwrap();
    fs::write(repo.root.join("owned.txt"), "prefix\noutside\n").unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "later committed prefix"],
        None,
        None,
    )
    .unwrap();
    git(&repo.root, &["push"], None, None).unwrap();
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -2 +2 @@\n-outside\n+mine\n", 1),
    );
    fs::write(repo.root.join("owned.txt"), "prefix\nmine\n").unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(
        prepared.changes[0].after.as_ref().unwrap().bytes,
        b"prefix\nmine\n"
    );
    commit_and_push(&prepared, "fix: 更新本对话文本", None).unwrap();
    assert_eq!(
        git(&repo.root, &["show", "HEAD:owned.txt"], None, None).unwrap(),
        b"prefix\nmine\n"
    );
}

#[test]
fn equivalent_history_suffixes_have_one_commit_content() {
    let repo = Repo::new();
    let base = format_rust(&repo.root, "2024", b"fn main(){let value=1;}\n").unwrap();
    fs::write(repo.root.join("owned.rs"), &base).unwrap();
    git(&repo.root, &["add", "--", "owned.rs"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "formatted baseline"],
        None,
        None,
    )
    .unwrap();
    git(&repo.root, &["push"], None, None).unwrap();
    repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:01.000Z","type":"response_item","payload":{"type":"function_call", "name":"exec_command", "namespace":"functions", "call_id":"noop-format", "arguments":json!({"cmd":"rustfmt --edition 2024 owned.rs"}).to_string()}}));
    repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:01.100Z","type":"response_item","payload":{"type":"function_call_output", "call_id":"noop-format", "output":"{\"exit_code\":0}"}}));
    repo.append_record(
        SESSION,
        &repo.native_change(
            SESSION,
            "owned.rs",
            "@@ -1,3 +1,3 @@\n fn main() {\n-    let value = 1;\n+    let value = 2;\n }\n",
            2,
        ),
    );
    let disk = String::from_utf8(base)
        .unwrap()
        .replace("value = 1", "value = 2");
    fs::write(repo.root.join("owned.rs"), &disk).unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(
        prepared.changes[0].after.as_ref().unwrap().bytes,
        disk.as_bytes()
    );
    let history = history::recover(&repo.home, SESSION, &repo.root, &repo.root).unwrap();
    assert_eq!(
        history
            .isolate_changes(
                &repo.root,
                "owned.rs",
                prepared.changes[0].before.as_ref(),
                prepared.changes[0].disk.as_ref()
            )
            .unwrap(),
        prepared.changes[0].after.clone().unwrap()
    );
}

#[test]
fn successful_patch_keeps_full_context_when_native_receipt_is_shortened() {
    for fixed_wrapper in [true, false] {
        let repo = Repo::new();
        fs::write(
            repo.root.join("owned.txt"),
            "anchor-a\nold\nanchor-b\nold\nend\n",
        )
        .unwrap();
        git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
        git(
            &repo.root,
            &["commit", "-m", "duplicate short context"],
            None,
            None,
        )
        .unwrap();
        git(&repo.root, &["push"], None, None).unwrap();
        let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n anchor-a\n-old\n+mine\n anchor-b\n*** End Patch";
        let code = format!(
            "text(await tools.apply_patch({}));",
            serde_json::to_string(patch).unwrap()
        );
        let code = if fixed_wrapper {
            code
        } else {
            format!("if (true) {{ {code} }}")
        };
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:01.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"context-patch","input":code}}));
        repo.append_record(
            SESSION,
            &repo.native_change(SESSION, "owned.txt", "@@ -2 +2 @@\n-old\n+mine\n", 1),
        );
        repo.append_record(SESSION, &json!({"timestamp":"2026-10-08T00:00:01.500Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"context-patch","output":[{"type":"text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":"{}"}]}}));
        fs::write(
            repo.root.join("owned.txt"),
            "anchor-a\nmine\nanchor-b\nold\noutside\n",
        )
        .unwrap();
        let result = snapshot(&repo.home, SESSION);
        if fixed_wrapper {
            let prepared = result.unwrap();
            assert_ne!(prepared.changes[0].after, prepared.changes[0].disk);
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn committed_native_history_does_not_hide_new_live_edits() {
    for keep_remainder in [false, true] {
        let (repo, own, _) = shared_native_repo();
        let prepared = snapshot(&repo.home, SESSION).unwrap();
        commit_and_push(&prepared, "chore(files): 更新测试", None).unwrap();
        if !keep_remainder {
            fs::write(repo.root.join("owned.txt"), &own).unwrap();
        }
        repo.fastctx(repo.hook(SESSION, "mcp__codey_fastctx__replace", json!({
            "path":repo.root.join("owned.txt"), "pattern":"line8", "replacement":"new own edit", "literal":true
        })));
        assert!(status(&repo.home, SESSION).is_ok(), "新编辑必须显示入口");
        if keep_remainder {
            assert!(
                snapshot(&repo.home, SESSION).is_err(),
                "未证实的混合新编辑仍应停止"
            );
        } else {
            let next = snapshot(&repo.home, SESSION).unwrap();
            assert!(next.diff.contains("new own edit"));
            assert_eq!(next.changes[0].after, next.changes[0].disk);
        }
    }
}

#[test]
fn shared_file_binds_unselected_disk_changes_and_rejects_ambiguous_history() {
    let (repo, _, disk) = shared_native_repo();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    fs::write(
        repo.root.join("owned.txt"),
        disk.replace("outside", "later user edit"),
    )
    .unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    assert!(commit_and_push(&prepared, "chore(files): 更新测试", None).is_err());
    assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);

    let (repo, _, disk) = shared_native_repo();
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -15 +15 @@\n-end\n+own end\n", 2),
    );
    fs::write(
        repo.root.join("owned.txt"),
        disk.replace("outside", "own end")
            .replace("line8", "outside"),
    )
    .unwrap();
    let error = snapshot(&repo.home, SESSION).unwrap_err();
    assert!(format!("{error:#}").contains("不能唯一对应当前 HEAD"));
}

#[test]
fn batched_head_entries_preserve_bytes_paths_modes_and_absence() {
    let repo = Repo::new();
    let paths = vec![
        "owned.txt".to_string(),
        "missing.txt".to_string(),
        "space file".to_string(),
        "binary".to_string(),
        "empty".to_string(),
        "nested/file".to_string(),
    ];
    #[cfg(unix)]
    let paths = {
        let mut paths = paths;
        paths.extend(["tab\tfile".into(), "line\nfile".into(), "[literal]*".into()]);
        paths
    };
    fs::create_dir(repo.root.join("nested")).unwrap();
    for path in &paths[2..] {
        fs::write(repo.root.join(path), b"\0\xff\nbody\n\0").unwrap();
    }
    fs::write(repo.root.join("empty"), b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(repo.root.join("binary"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "batch entries"], None, None).unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    fs::remove_file(repo.root.join("owned.txt")).unwrap();
    fs::write(repo.root.join("missing.txt"), b"new file").unwrap();
    let batched = head_entries(&repo.root, &head, &paths).unwrap();
    for path in &paths {
        assert_eq!(batched[path], head_entry(&repo.root, &head, path).unwrap());
    }
    assert!(batched["missing.txt"].is_none());
    assert_ne!(
        batched["owned.txt"],
        disk_entry(&repo.root, "owned.txt").unwrap()
    );
    assert!(head_entries(&repo.root, &head, &[]).unwrap().is_empty());
}

#[test]
fn batched_head_entries_split_content_and_enforce_file_limit() {
    let repo = Repo::new();
    let paths = vec!["large-a".to_string(), "large-b".to_string()];
    for (index, path) in paths.iter().enumerate() {
        fs::write(repo.root.join(path), vec![index as u8; MAX_BYTES as usize]).unwrap();
    }
    fs::write(repo.root.join("oversized"), vec![0; MAX_BYTES as usize + 1]).unwrap();
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "batch limits"], None, None).unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    let batched = head_entries(&repo.root, &head, &paths).unwrap();
    for path in &paths {
        assert_eq!(batched[path], head_entry(&repo.root, &head, path).unwrap());
    }
    assert!(head_entries(&repo.root, &head, &["owned.txt".into(), "oversized".into()]).is_err());
    assert!(head_entries(&repo.root, &head, &vec!["owned.txt".into(); MAX_FILES + 1]).is_err());
}

#[cfg(unix)]
#[test]
fn batched_head_entries_reject_symlinks_and_directories() {
    let repo = Repo::new();
    std::os::unix::fs::symlink("owned.txt", repo.root.join("link")).unwrap();
    fs::create_dir(repo.root.join("directory")).unwrap();
    fs::write(repo.root.join("directory/file"), b"content").unwrap();
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "unsupported entries"],
        None,
        None,
    )
    .unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    for path in ["link", "directory"] {
        assert!(head_entries(&repo.root, &head, &["owned.txt".into(), path.into()]).is_err());
    }
}

#[test]
fn display_batched_comparison_finds_later_changes_and_reverted_files() {
    let repo = Repo::new();
    let paths: Vec<_> = (0..9).map(|i| format!("batch-{i}.txt")).collect();
    for path in &paths {
        fs::write(repo.root.join(path), "old\n").unwrap();
    }
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "batch baseline"], None, None).unwrap();
    for path in &paths {
        repo.edit(SESSION, path, "old", "new");
        if path != paths.last().unwrap() {
            fs::write(repo.root.join(path), "old\n").unwrap();
        }
    }
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    fs::write(repo.root.join(paths.last().unwrap()), "old\n").unwrap();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        false
    );
}

#[test]
fn display_batched_comparison_handles_large_reverted_candidates() {
    let repo = Repo::new();
    let paths: Vec<_> = (0..9).map(|i| format!("large-{i}.txt")).collect();
    let baseline = format!("old\n{}", "x".repeat(MAX_BYTES as usize - 4));
    for path in &paths {
        fs::write(repo.root.join(path), &baseline).unwrap();
    }
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "large baseline"], None, None).unwrap();
    for path in &paths {
        repo.edit(SESSION, path, "old", "new");
        fs::write(repo.root.join(path), &baseline).unwrap();
    }
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        false
    );
    fs::write(
        repo.root.join(paths.last().unwrap()),
        baseline.replacen("old", "new", 1),
    )
    .unwrap();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
}

#[cfg(unix)]
#[test]
fn display_batch_failure_preserves_early_valid_change() {
    let repo = Repo::new();
    fs::write(repo.root.join("zz-later.txt"), "old\n").unwrap();
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "later baseline"], None, None).unwrap();
    repo.edit(SESSION, "owned.txt", "old", "new");
    repo.edit(SESSION, "zz-later.txt", "old", "new");
    fs::remove_file(repo.root.join("zz-later.txt")).unwrap();
    std::os::unix::fs::symlink("owned.txt", repo.root.join("zz-later.txt")).unwrap();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[cfg(unix)]
#[test]
fn display_batch_failure_still_rejects_invalid_first_path() {
    let repo = Repo::new();
    fs::write(repo.root.join("aa-first.txt"), "old\n").unwrap();
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "first baseline"], None, None).unwrap();
    repo.edit(SESSION, "owned.txt", "old", "new");
    repo.edit(SESSION, "aa-first.txt", "old", "new");
    fs::remove_file(repo.root.join("aa-first.txt")).unwrap();
    std::os::unix::fs::symlink("owned.txt", repo.root.join("aa-first.txt")).unwrap();
    assert!(display_status(&repo.home, SESSION).is_err());
}

#[test]
#[ignore = "显式运行多文件 HEAD 读取耗时比较"]
fn benchmark_batched_head_comparison() {
    let repo = Repo::new();
    let paths: Vec<_> = (0..64).map(|i| format!("compare-{i}.txt")).collect();
    for path in &paths {
        fs::write(repo.root.join(path), "baseline\n".repeat(1024)).unwrap();
    }
    git(&repo.root, &["add", "--all"], None, None).unwrap();
    git(
        &repo.root,
        &["commit", "-m", "comparison benchmark"],
        None,
        None,
    )
    .unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    let start = Instant::now();
    let individual: BTreeMap<_, _> = paths
        .iter()
        .map(|path| (path.clone(), head_entry(&repo.root, &head, path).unwrap()))
        .collect();
    let individual_time = start.elapsed();
    let start = Instant::now();
    let mut batched = BTreeMap::new();
    for chunk in paths.chunks(8) {
        batched.extend(head_entries(&repo.root, &head, chunk).unwrap());
    }
    let batched_time = start.elapsed();
    assert_eq!(individual, batched);
    eprintln!("64 文件 HEAD 读取：逐项 {individual_time:?}，每批 8 个 {batched_time:?}");
}

struct Repo {
    directory: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let root = base.join("work");
        let remote = base.join("remote.git");
        let home = base.join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&remote).unwrap();
        fs::create_dir_all(home.join("sessions")).unwrap();
        git(&remote, &["init", "--bare"], None, None).unwrap();
        git(&root, &["init", "-b", "main"], None, None).unwrap();
        git(&root, &["config", "user.name", "Git Test"], None, None).unwrap();
        git(
            &root,
            &["config", "user.email", "git-test@example.invalid"],
            None,
            None,
        )
        .unwrap();
        git(&root, &["config", "commit.gpgsign", "false"], None, None).unwrap();
        fs::write(root.join("owned.txt"), "old\n").unwrap();
        fs::write(root.join("other.txt"), "other\n").unwrap();
        git(&root, &["add", "--", "owned.txt", "other.txt"], None, None).unwrap();
        git(&root, &["commit", "-m", "initial"], None, None).unwrap();
        // 使用相对路径，避免 Windows 规范化路径的前缀被 Git 解析为 SSH 地址。
        git(
            &root,
            &["remote", "add", "origin", "../remote.git"],
            None,
            None,
        )
        .unwrap();
        git(&root, &["push", "-u", "origin", "main"], None, None).unwrap();
        rusqlite::Connection::open(home.join("state_5.sqlite"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT, rollout_path TEXT)",
            )
            .unwrap();
        let repo = Self {
            directory,
            root,
            remote,
            home,
        };
        repo.session(SESSION, None);
        repo.session(OTHER, None);
        repo
    }

    fn session(&self, id: &str, parent: Option<&str>) -> PathBuf {
        let path = self
            .home
            .join("sessions")
            .join(format!("rollout-{id}.jsonl"));
        let mut meta = json!({"id": id, "cwd": self.root});
        if let Some(parent) = parent {
            meta["parent_thread_id"] = json!(parent);
            meta["agent_path"] = json!("/root/editor");
            meta["agent_role"] = json!("codey_worker");
        }
        fs::write(
            &path,
            format!("{}\n", json!({"type":"session_meta", "payload":meta})),
        )
        .unwrap();
        rusqlite::Connection::open(self.home.join("state_5.sqlite"))
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO threads VALUES (?1, ?2, ?3)",
                [id, self.root.to_str().unwrap(), path.to_str().unwrap()],
            )
            .unwrap();
        path
    }

    fn hook(&self, session: &str, name: &str, input: Value) -> Value {
        json!({"hook_event_name":"PreToolUse", "session_id":session, "agent_id":"root",
            "turn_id":uuid::Uuid::new_v4().to_string(), "tool_name":name, "tool_input":input,
            "transcript_path": self.home.join("sessions").join(format!("rollout-{session}.jsonl")), "cwd":self.root})
    }

    fn observe(&self, hook: &Value) -> Result<()> {
        tracking::observe_with_runtime(&self.home, hook, Some(RUNTIME))
    }

    fn finish(&self, mut hook: Value, writes: &[(&str, Option<&str>)], output: Value) {
        for (file, content) in writes {
            if let Some(content) = content {
                fs::write(self.root.join(file), content).unwrap();
            } else {
                fs::remove_file(self.root.join(file)).unwrap();
            }
        }
        hook["hook_event_name"] = json!("PostToolUse");
        hook["tool_response"] = output;
        self.observe(&hook).unwrap();
    }

    fn edit(&self, session: &str, file: &str, before: &str, after: &str) {
        let patch = format!(
            "*** Begin Patch\n*** Update File: {file}\n@@\n-{before}\n+{after}\n*** End Patch"
        );
        let hook = self.hook(session, "apply_patch", json!(patch));
        self.observe(&hook).unwrap();
        self.finish(
            hook,
            &[(file, Some(&format!("{after}\n")))],
            json!(format!("Success. Updated the following files:\nM {file}")),
        );
    }

    fn snapshot(&self) -> Snapshot {
        self.edit(SESSION, "owned.txt", "old", "new");
        snapshot(&self.home, SESSION).unwrap()
    }

    fn history(
        &self,
        session: &str,
        namespace: Option<&str>,
        name: &str,
        args: Value,
        response: Value,
        second: u32,
    ) {
        let path = self
            .home
            .join("sessions")
            .join(format!("rollout-{session}.jsonl"));
        let id = uuid::Uuid::new_v4().to_string();
        let mut call = json!({"type":"function_call", "name":name, "call_id":id, "arguments":args.to_string()});
        if let Some(namespace) = namespace {
            call["namespace"] = json!(namespace);
        }
        if name == "apply_patch" {
            call["type"] = json!("custom_tool_call");
            call["input"] = args;
        }
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{}", json!({"timestamp": format!("2026-10-08T00:00:{second:02}.000Z"), "ordinal":100 + second * 2, "type":"response_item", "payload":call})).unwrap();
        writeln!(file, "{}", json!({"timestamp": format!("2026-10-08T00:00:{second:02}.500Z"), "ordinal":101 + second * 2, "type":"response_item", "payload":{"type":"function_call_output", "call_id":id, "output":response}})).unwrap();
    }

    fn old_patch(&self, session: &str, file: &str, before: &str, after: &str, second: u32) {
        self.history(session, None, "apply_patch",
            json!(format!("*** Begin Patch\n*** Update File: {file}\n@@\n-{before}\n+{after}\n*** End Patch")),
            json!([{"type":"text", "text":"Wall time: 0.01 seconds"}, {"type":"text", "text":format!("Success. Updated the following files:\nM {file}")}]), second);
        fs::write(self.root.join(file), format!("{after}\n")).unwrap();
    }

    fn native_change(&self, session: &str, file: &str, diff: &str, second: u32) -> Value {
        let time =
            chrono::DateTime::parse_from_rfc3339(&format!("2026-10-08T00:00:{second:02}.100Z"))
                .unwrap()
                .timestamp_millis();
        json!({"timestamp":format!("2026-10-08T00:00:{second:02}.200Z"), "ordinal": 100 + second * 2,
        "type":"event_msg", "payload":{"type":"item_completed", "thread_id":session,
        "started_at_ms":time, "completed_at_ms":time + 50, "item":{
            "type":"FileChange", "id":uuid::Uuid::new_v4().to_string(), "status":"completed",
            "stdout":format!("Success. Updated the following files:\nM {}\n",self.root.join(file).display()),
            "changes":{self.root.join(file).to_string_lossy().to_string():{
                "type":"update", "move_path":null, "unified_diff":diff}}
        }}})
    }

    fn append_record(&self, session: &str, record: &Value) {
        let path = self
            .home
            .join("sessions")
            .join(format!("rollout-{session}.jsonl"));
        writeln!(
            OpenOptions::new().append(true).open(path).unwrap(),
            "{record}"
        )
        .unwrap();
    }

    fn fastctx(&self, hook: Value) {
        self.observe(&hook).unwrap();
        let request = serde_json::from_value(hook["tool_input"].clone()).unwrap();
        let response = fastctx::edit::ReplaceService::new().replace(request);
        let content: Vec<Value> = response
            .content
            .into_iter()
            .map(|block| match block {
                fastctx::ToolContent::Text(text) => json!({"type":"text", "text":text}),
                _ => panic!("unexpected image"),
            })
            .collect();
        self.finish(
            hook,
            &[],
            json!({"content": content, "isError": response.is_error}),
        );
    }

    fn child(&self) -> Value {
        use crate::subagent_orchestrator::*;
        let transcript = self.session(CHILD, Some(SESSION));
        let state = self.home.join(crate::subagent_gate::STATE_DIRECTORY);
        let spawn = json!({"task_name":"editor", "agent_type":"codey_worker", "message":"edit"});
        assert!(
            pre_spawn_with_workspace_and_turn(
                &state,
                RUNTIME,
                SESSION,
                Some(&spawn),
                RootHookContext::new(self.root.to_str(), 0, 10)
            )
            .unwrap()
            .is_none()
        );
        post_spawn(
            &state,
            RUNTIME,
            SESSION,
            Some(&spawn),
            Some(&json!({"agent_id":CHILD})),
            11,
        )
        .unwrap();
        let mut hook = self.hook(SESSION, "mcp__codey_fastctx__replace", json!({
            "path": self.root.join("other.txt"), "pattern":"other", "replacement":"child", "literal":true
        }));
        hook["agent_id"] = json!(CHILD);
        hook["agent_type"] = json!("codey_worker");
        hook["agent_transcript_path"] = json!(transcript);
        hook
    }
}

#[test]
fn old_conversation_shows_button_and_commits_only_exact_historical_changes() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    fs::write(repo.root.join("other.txt"), "staged\n").unwrap();
    git(&repo.root, &["add", "--", "other.txt"], None, None).unwrap();
    fs::write(repo.root.join("other.txt"), "staged plus user edit\n").unwrap();
    let status = status(&repo.home, SESSION).unwrap();
    assert_eq!(status["visible"], true);
    assert_eq!(status["files"], json!(["owned.txt"]));
    let snapshot = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(snapshot.public()["files"], json!(["owned.txt"]));
    assert_eq!(
        commit_and_push(&snapshot, "chore(files): 更新历史对话文件", None).unwrap()["status"],
        "pushed"
    );
    assert_eq!(
        git(&repo.root, &["show", ":other.txt"], None, None).unwrap(),
        b"staged\n"
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("other.txt")).unwrap(),
        "staged plus user edit\n"
    );
    assert!(super::status(&repo.home, SESSION).is_err());
}

#[test]
fn native_edit_events_complete_aggregate_history_and_preserve_other_changes() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "middle", 1);
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-old\n+middle\n", 1),
    );
    let code = "text(await tools.apply_patch(\"*** Begin Patch\\n*** Update File: owned.txt\\n@@\\n-middle\\n+new\\n*** End Patch\")); text(await tools.exec_command({cmd:\"pnpm check\"}));";
    repo.history(
        SESSION,
        None,
        "exec",
        json!(code),
        json!([
            {"type":"input_text","text":"Script completed\nOutput:\n"},
            {"type":"input_text","text":"{}"},
            {"type":"input_text","text":"{\"exit_code\":0}"}
        ]),
        2,
    );
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-middle\n+new\n", 2),
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    fs::write(repo.root.join("other.txt"), "user staged\n").unwrap();
    git(&repo.root, &["add", "--", "other.txt"], None, None).unwrap();
    fs::write(repo.root.join("other.txt"), "user unstaged\n").unwrap();
    let snapshot = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(snapshot.public()["files"], json!(["owned.txt"]));
    assert_eq!(
        commit_and_push(&snapshot, "chore(files): 更新当前对话文件", None).unwrap()["status"],
        "pushed"
    );
    assert_eq!(
        git(&repo.root, &["show", ":other.txt"], None, None).unwrap(),
        b"user staged\n"
    );
    assert_eq!(
        fs::read(repo.root.join("other.txt")).unwrap(),
        b"user unstaged\n"
    );
}

#[test]
fn native_edit_events_reject_invalid_identity_receipt_and_changed_content() {
    for scenario in [
        "identity",
        "receipt",
        "failed",
        "duplicate",
        "content",
        "inherited",
    ] {
        let repo = Repo::new();
        let mut record = repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-old\n+new\n", 1);
        match scenario {
            "identity" => record["payload"]["thread_id"] = json!(OTHER),
            "receipt" => {
                record["payload"]["item"]["stdout"] =
                    json!("Success. Updated the following files:\nM other.txt")
            }
            "failed" => record["payload"]["item"]["status"] = json!("failed"),
            "inherited" => {
                let path = repo
                    .home
                    .join("sessions")
                    .join(format!("rollout-{SESSION}.jsonl"));
                fs::write(
                    path,
                    format!(
                        "{}\n",
                        json!({"type":"session_meta", "payload":{
                            "id":SESSION,"cwd":repo.root,"subagent_history_start_ordinal":999
                        }})
                    ),
                )
                .unwrap();
            }
            _ => {}
        }
        repo.append_record(SESSION, &record);
        if scenario == "duplicate" {
            repo.append_record(SESSION, &record);
        }
        fs::write(
            repo.root.join("owned.txt"),
            if scenario == "content" {
                "new\nuser edit\n"
            } else {
                "new\n"
            },
        )
        .unwrap();
        assert!(snapshot(&repo.home, SESSION).is_err(), "{scenario}");
    }
}

#[test]
fn native_unified_patch_uses_exact_coordinates_and_reverses_insertions() {
    let input = "marker\nfirst\nmarker\nlast\n";
    let diff = "@@ -3,2 +3,3 @@\n marker\n+inserted\n last\n";
    let after = history::apply_native_patch(input, diff, false).unwrap();
    assert_eq!(after, "marker\nfirst\nmarker\ninserted\nlast\n");
    assert_eq!(
        history::apply_native_patch(&after, diff, true).unwrap(),
        input
    );
    let insertion = "@@ -0,0 +1 @@\n+start\n";
    assert_eq!(
        history::apply_native_patch("", insertion, false).unwrap(),
        "start\n"
    );
    assert_eq!(
        history::apply_native_patch("start\n", insertion, true).unwrap(),
        ""
    );
    for malformed in [
        "@@ -3,9 +3,3 @@\n marker\n+inserted\n last\n",
        "@@ -2,2 +3,3 @@\n marker\n+inserted\n last\n",
        "@@ -3,2 +8,3 @@\n marker\n+inserted\n last\n",
    ] {
        assert!(history::apply_native_patch(input, malformed, false).is_err());
    }
}

#[test]
fn native_edit_history_keeps_other_conversation_ownership_veto() {
    let repo = Repo::new();
    repo.append_record(
        SESSION,
        &repo.native_change(SESSION, "owned.txt", "@@ -1 +1 @@\n-old\n+new\n", 1),
    );
    let hook = repo.hook(
        OTHER,
        "mcp__codey_fastctx__replace",
        json!({
            "path":repo.root.join("owned.txt"), "pattern":"old", "replacement":"new", "literal":true
        }),
    );
    repo.fastctx(hook);
    let error = snapshot(&repo.home, SESSION).unwrap_err().to_string();
    assert!(error.contains("含其他对话的编辑记录"), "{error}");
}

#[test]
fn native_child_edits_require_parent_binding() {
    let repo = Repo::new();
    repo.session(CHILD, Some(SESSION));
    repo.append_record(
        CHILD,
        &repo.native_change(CHILD, "other.txt", "@@ -1 +1 @@\n-other\n+child\n", 2),
    );
    fs::write(repo.root.join("other.txt"), "child\n").unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
    repo.history(
        SESSION,
        Some("agents"),
        "spawn_agent",
        json!({"task_name":"editor", "agent_type":"codey_worker"}),
        json!({"task_name":"/root/editor"}).to_string().into(),
        1,
    );
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().public()["files"],
        json!(["other.txt"])
    );
}

#[test]
fn auxiliary_catalog_database_does_not_hide_historical_changes() {
    let repo = Repo::new();
    fs::create_dir(repo.home.join("sqlite")).unwrap();
    rusqlite::Connection::open(repo.home.join("sqlite/catalog.db"))
        .unwrap()
        .execute_batch("CREATE TABLE local_thread_catalog (thread_id TEXT NOT NULL)")
        .unwrap();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["owned.txt"])
    );
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().public()["files"],
        json!(["owned.txt"])
    );
    assert!(status(&repo.home, OTHER).is_err());
}

#[test]
fn display_status_uses_live_tracking_without_waiting_for_full_history() {
    let repo = Repo::new();
    repo.edit(SESSION, "owned.txt", "old", "new");
    // 模拟生成中的历史末行尚未保存完整：已完成的执行记录足以展示入口。
    let path = repo
        .home
        .join("sessions")
        .join(format!("rollout-{SESSION}.jsonl"));
    writeln!(
        OpenOptions::new().append(true).open(path).unwrap(),
        "{{unfinished"
    )
    .unwrap();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    assert!(
        snapshot(&repo.home, SESSION).is_err(),
        "预览仍须完整核验历史"
    );
    assert_eq!(display_status(&repo.home, OTHER).unwrap()["visible"], false);
    fs::write(repo.root.join("owned.txt"), "old\n").unwrap();
    assert!(
        display_status(&repo.home, SESSION).is_err(),
        "已撤销的执行记录不能单独显示入口"
    );
}

#[test]
fn display_fast_path_does_not_authorize_mixed_conversation_files() {
    let repo = Repo::new();
    repo.edit(SESSION, "owned.txt", "old", "mine");
    repo.edit(OTHER, "owned.txt", "mine", "mixed");
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    assert!(
        snapshot(&repo.home, SESSION).is_err(),
        "入口可见不能替代跨会话归属校验"
    );
}

#[test]
fn display_history_cache_tracks_appended_records_and_commit_state() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    fs::write(repo.root.join("owned.txt"), "old\n").unwrap();
    repo.old_patch(SESSION, "other.txt", "other", "next", 2);
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().public()["files"],
        json!(["other.txt"])
    );
    git(&repo.root, &["add", "--", "other.txt"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "save"], None, None).unwrap();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        false,
        "历史缓存不能跳过当前 HEAD 与文件检查"
    );
}

#[test]
fn display_cache_cannot_replace_preview_content_verification() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    let path = repo
        .home
        .join("sessions")
        .join(format!("rollout-{SESSION}.jsonl"));
    let metadata = fs::metadata(&path).unwrap();
    let original = fs::read_to_string(&path).unwrap();
    // 同长度改写并恢复修改时间，展示缓存仍可命中，提交证明必须重读内容。
    let rewritten = original.replace("-old\\n+new", "-bad\\n+new");
    assert_ne!(original, rewritten);
    assert_eq!(original.len(), rewritten.len());
    fs::write(&path, rewritten).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
#[ignore = "显式运行合成大会话的只读状态耗时比较"]
fn benchmark_large_conversation_display_status() {
    let repo = Repo::new();
    let path = repo
        .home
        .join("sessions")
        .join(format!("rollout-{SESSION}.jsonl"));
    let record =
        json!({"type":"response_item", "payload":{"type":"message", "content":"x".repeat(16_384)}})
            .to_string();
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    for _ in 0..2_048 {
        writeln!(file, "{record}").unwrap();
    }
    drop(file);
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    let start = std::time::Instant::now();
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    let cold = start.elapsed();
    let start = std::time::Instant::now();
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    let strict = start.elapsed();
    let start = std::time::Instant::now();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    let cached = start.elapsed();
    repo.edit(SESSION, "other.txt", "other", "live");
    let start = std::time::Instant::now();
    assert_eq!(
        display_status(&repo.home, SESSION).unwrap()["visible"],
        true
    );
    eprintln!(
        "32 MiB history: cold={cold:?}, full-check={strict:?}, display-cache={cached:?}, live-display={:?}",
        start.elapsed()
    );
}

#[test]
fn rollout_input_text_receipts_recover_only_reported_files() {
    let repo = Repo::new();
    repo.history(SESSION, Some("mcp__codey_fastctx"), "replace",
        json!({"path":repo.root.join("owned.txt"), "pattern":"old", "replacement":"new", "literal":true}),
        json!([{"type":"input_text", "text":format!("{}: 1 replacement\n\n(Complete: 1 replacement in 1 file.)", repo.root.join("owned.txt").display())}]), 1);
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    fs::write(repo.root.join("other.txt"), "user edit\n").unwrap();
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["owned.txt"])
    );
    assert!(snapshot(&repo.home, SESSION).is_ok());
    assert!(
        tracking::output_text(
            &json!({"isError":true,"content":[{"type":"input_text","text":"success"}]})
        )
        .is_none()
    );
    assert!(tracking::output_text(&json!([{"type":"image","text":"success"}])).is_none());
}

#[test]
fn old_fastctx_directory_replace_uses_actual_receipt_files() {
    let repo = Repo::new();
    let args = json!({"path":repo.root, "pattern":"old", "replacement":"new", "literal":true});
    let result =
        fastctx::edit::ReplaceService::new().replace(serde_json::from_value(args.clone()).unwrap());
    let output: Vec<_> = result
        .content
        .into_iter()
        .map(|block| match block {
            fastctx::ToolContent::Text(text) => json!({"type":"text", "text":text}),
            _ => panic!("expected text"),
        })
        .collect();
    repo.history(
        SESSION,
        Some("mcp__codey_fastctx"),
        "replace",
        args,
        json!(output),
        1,
    );
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["owned.txt"])
    );
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().public()["files"],
        json!(["owned.txt"])
    );
}

#[test]
fn old_child_edits_require_matching_parent_spawn_receipt_and_ignore_inherited_history() {
    let repo = Repo::new();
    repo.session(CHILD, Some(SESSION));
    repo.old_patch(CHILD, "other.txt", "other", "child", 2);
    assert!(status(&repo.home, SESSION).is_err());
    repo.history(
        SESSION,
        Some("agents"),
        "spawn_agent",
        json!({"task_name":"editor", "agent_type":"codey_worker"}),
        json!({"task_name":"/root/editor"}).to_string().into(),
        1,
    );
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["other.txt"])
    );
    assert!(snapshot(&repo.home, SESSION).is_ok());
    let connection = rusqlite::Connection::open(repo.home.join("state_5.sqlite")).unwrap();
    connection
        .execute_batch("ALTER TABLE threads ADD COLUMN source TEXT NOT NULL DEFAULT ''; ")
        .unwrap();
    connection
        .execute(
            "UPDATE threads SET source=?1 WHERE id=?2",
            [
                json!({"subagent":{"thread_spawn":{"parent_thread_id":SESSION}}}).to_string(),
                CHILD.into(),
            ],
        )
        .unwrap();
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["other.txt"])
    );

    connection
        .execute(
            "UPDATE threads SET cwd=?1 WHERE id=?2",
            [repo.directory.path().to_str().unwrap(), CHILD],
        )
        .unwrap();
    assert!(status(&repo.home, SESSION).is_err());
    connection
        .execute(
            "UPDATE threads SET cwd=?1 WHERE id=?2",
            [repo.root.to_str().unwrap(), CHILD],
        )
        .unwrap();
    let path = repo
        .home
        .join("sessions")
        .join(format!("rollout-{CHILD}.jsonl"));
    let mut lines: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines[0]["payload"]["forked_from_id"] = json!(SESSION);
    lines[0]["payload"]["subagent_history_start_ordinal"] = json!(999);
    fs::write(
        path,
        lines
            .into_iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    assert!(status(&repo.home, SESSION).is_err());
}

#[test]
fn old_conversation_refuses_later_user_changes_and_other_conversation_claims() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    fs::write(repo.root.join("owned.txt"), "new\nuser edit\n").unwrap();
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("历史编辑")
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    repo.edit(OTHER, "owned.txt", "new", "other conversation");
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("含其他对话的编辑记录")
    );
}

#[test]
fn old_conversation_refuses_incomplete_receipts_regex_and_unrecorded_new_file_content() {
    let repo = Repo::new();
    repo.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch"),
        json!("Error: patch failed"),
        1,
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    assert!(status(&repo.home, SESSION).is_err());
    repo.history(
        SESSION,
        Some("mcp__codey_fastctx"),
        "replace",
        json!({"path":repo.root.join("owned.txt"), "pattern":".*old.*", "replacement":"new"}),
        json!(format!(
            "{}: 1 replacement\n\n(Complete: 1 replacement in 1 file.)",
            repo.root.join("owned.txt").display()
        )),
        2,
    );
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    fs::write(repo.root.join("owned.txt"), "new\noutside\n").unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
    let other = Repo::new();
    other.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: created.txt\n+created\n*** End Patch"),
        json!("Success. Updated the following files:\nA created.txt"),
        1,
    );
    fs::write(other.root.join("created.txt"), "created\nunrecorded\n").unwrap();
    assert_eq!(
        status(&other.home, SESSION).unwrap()["files"],
        json!(["created.txt"])
    );
    assert!(snapshot(&other.home, SESSION).is_err());
}

#[test]
fn historical_new_file_creation_and_followup_edits_commit_only_owned_content() {
    for native in [false, true] {
        let repo = Repo::new();
        if native {
            let mut creation = repo.native_change(SESSION, "created.txt", "", 1);
            let path = repo.root.join("created.txt").to_string_lossy().to_string();
            creation["payload"]["item"]["changes"][&path] =
                json!({"type":"add", "content":"created\n"});
            repo.append_record(SESSION, &creation);
        } else {
            repo.history(
                SESSION,
                None,
                "apply_patch",
                json!("*** Begin Patch\n*** Add File: created.txt\n+created\n*** End Patch"),
                json!("Success. Updated the following files:\nA created.txt"),
                1,
            );
        }
        repo.old_patch(SESSION, "created.txt", "created", "updated", 2);
        fs::write(repo.root.join("other.txt"), "outside\n").unwrap();
        let prepared = snapshot(&repo.home, SESSION).unwrap();
        assert_eq!(prepared.changes.len(), 1);
        assert_eq!(prepared.changes[0].path, "created.txt");
        assert!(prepared.changes[0].before.is_none());
        commit_and_push(&prepared, "feat: 增加当前对话文件", None).unwrap();
        assert_eq!(
            git(&repo.root, &["show", "HEAD:created.txt"], None, None).unwrap(),
            b"updated\n"
        );
        assert_eq!(fs::read(repo.root.join("other.txt")).unwrap(), b"outside\n");
    }
}

#[test]
fn historical_tracked_deletion_is_supported_but_mixed_ownership_is_rejected() {
    for native in [false, true] {
        let repo = Repo::new();
        if native {
            let mut deletion = repo.native_change(SESSION, "owned.txt", "", 1);
            let path = repo.root.join("owned.txt").to_string_lossy().to_string();
            deletion["payload"]["item"]["changes"][&path] = json!({"type":"delete"});
            repo.append_record(SESSION, &deletion);
        } else {
            repo.history(
                SESSION,
                None,
                "apply_patch",
                json!("*** Begin Patch\n*** Delete File: owned.txt\n*** End Patch"),
                json!("Success. Updated the following files:\nD owned.txt"),
                1,
            );
        }
        fs::remove_file(repo.root.join("owned.txt")).unwrap();
        let prepared = snapshot(&repo.home, SESSION).unwrap();
        assert!(prepared.changes[0].after.is_none());
        commit_and_push(&prepared, "chore: 删除当前对话文件", None).unwrap();
        assert!(
            head_entry(
                &repo.root,
                &git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
                "owned.txt"
            )
            .unwrap()
            .is_none()
        );
    }
    let mixed = Repo::new();
    mixed.fastctx(mixed.hook(OTHER, "mcp__codey_fastctx__replace", json!({"path":mixed.root.join("owned.txt"), "pattern":"old", "replacement":"outside", "literal":true})));
    mixed.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Delete File: owned.txt\n*** End Patch"),
        json!("Success. Updated the following files:\nD owned.txt"),
        1,
    );
    fs::remove_file(mixed.root.join("owned.txt")).unwrap();
    assert!(
        snapshot(&mixed.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("其他对话")
    );
}

#[test]
fn historical_creation_rejects_another_sessions_stale_claim_even_if_content_matches() {
    let repo = Repo::new();
    repo.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: created.txt\n+created\n*** End Patch"),
        json!("Success. Updated the following files:\nA created.txt"),
        1,
    );
    fs::write(repo.root.join("created.txt"), "created\n").unwrap();
    repo.fastctx(repo.hook(OTHER, "mcp__codey_fastctx__replace", json!({"path":repo.root.join("created.txt"), "pattern":"created", "replacement":"outside", "literal":true})));
    fs::write(repo.root.join("created.txt"), "created\n").unwrap();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("其他对话")
    );
    assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
    assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
    assert_eq!(
        fs::read(repo.root.join("created.txt")).unwrap(),
        b"created\n"
    );
}

#[test]
fn committed_historical_creation_does_not_claim_later_unrecorded_changes() {
    let repo = Repo::new();
    repo.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: created.txt\n+created\n*** End Patch"),
        json!("Success. Updated the following files:\nA created.txt"),
        1,
    );
    fs::write(repo.root.join("created.txt"), "created\n").unwrap();
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    commit_and_push(&prepared, "feat: 增加文件", None).unwrap();
    fs::write(repo.root.join("created.txt"), "outside\n").unwrap();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 2);
    let prepared = snapshot(&repo.home, SESSION).unwrap();
    assert_eq!(prepared.changes.len(), 1);
    assert_eq!(prepared.changes[0].path, "owned.txt");
    commit_and_push(&prepared, "fix: 修改当前对话文件", None).unwrap();
    assert_eq!(
        git(&repo.root, &["show", "HEAD:created.txt"], None, None).unwrap(),
        b"created\n"
    );
    assert_eq!(
        fs::read(repo.root.join("created.txt")).unwrap(),
        b"outside\n"
    );
}

#[test]
fn historical_add_cannot_overwrite_a_tracked_baseline() {
    let repo = Repo::new();
    repo.history(
        SESSION,
        None,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: owned.txt\n+replacement\n*** End Patch"),
        json!("Success. Updated the following files:\nA owned.txt"),
        1,
    );
    fs::write(repo.root.join("owned.txt"), "replacement\n").unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
    assert_eq!(
        git(&repo.root, &["show", "HEAD:owned.txt"], None, None).unwrap(),
        b"old\n"
    );
    assert_eq!(
        fs::read(repo.root.join("owned.txt")).unwrap(),
        b"replacement\n"
    );
}

#[test]
fn old_conversation_can_replay_only_edits_after_the_current_committed_baseline() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "middle", 1);
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    git(&repo.root, &["commit", "-m", "middle"], None, None).unwrap();
    repo.old_patch(SESSION, "owned.txt", "middle", "new", 2);
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().changes[0]
            .before
            .as_ref()
            .unwrap()
            .bytes,
        b"middle\n"
    );
}

#[test]
fn historical_and_live_single_patch_aggregate_accept_native_completion_envelope() {
    let repo = Repo::new();
    let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch";
    let code = format!("text(await tools.apply_patch({}));", json!(patch));
    let response = json!([
        {"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
        {"type":"input_text", "text":"{}"}
    ]);
    repo.history(
        SESSION,
        Some("functions"),
        "exec",
        json!(code),
        response.clone(),
        1,
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    assert_eq!(
        status(&repo.home, SESSION).unwrap()["files"],
        json!(["owned.txt"])
    );
    assert!(snapshot(&repo.home, SESSION).is_ok());

    let live = Repo::new();
    let hook = live.hook(SESSION, "functions.exec", json!({"input":code}));
    live.observe(&hook).unwrap();
    live.finish(hook, &[("owned.txt", Some("new\n"))], response);
    assert!(snapshot(&live.home, SESSION).is_ok());
    let failed = Repo::new();
    failed.history(
        SESSION,
        Some("functions"),
        "exec",
        json!(code),
        json!("Script failed\nWall time 0.1 seconds\nOutput:\n{}"),
        1,
    );
    fs::write(failed.root.join("owned.txt"), "new\n").unwrap();
    assert!(status(&failed.home, SESSION).is_err());
}

#[test]
fn unfinished_historical_edit_blocks_submission_of_earlier_completed_edits() {
    let repo = Repo::new();
    repo.old_patch(SESSION, "owned.txt", "old", "new", 1);
    let mut file = OpenOptions::new()
        .append(true)
        .open(
            repo.home
                .join("sessions")
                .join(format!("rollout-{SESSION}.jsonl")),
        )
        .unwrap();
    writeln!(file, "{}", json!({"timestamp":"2026-10-08T00:00:02Z", "type":"response_item", "payload":{
        "type":"custom_tool_call", "name":"apply_patch", "call_id":"unfinished", "input":"*** Begin Patch\n*** Update File: other.txt\n@@\n-other\n+new\n*** End Patch"
    }})).unwrap();
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("缺少工具回执")
    );
}

#[test]
fn overlapping_historical_parent_and_child_edits_are_rejected() {
    let repo = Repo::new();
    repo.session(CHILD, Some(SESSION));
    repo.history(
        SESSION,
        Some("agents"),
        "spawn_agent",
        json!({"task_name":"editor", "agent_type":"codey_worker"}),
        json!({"agent_id":CHILD}).to_string().into(),
        1,
    );
    repo.old_patch(SESSION, "owned.txt", "old", "middle", 2);
    repo.old_patch(CHILD, "owned.txt", "middle", "new", 2);
    assert_eq!(status(&repo.home, SESSION).unwrap()["visible"], true);
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("编辑顺序不明确")
    );
}

#[test]
fn commit_isolates_owned_files_and_preserves_other_staged_and_unstaged_changes() {
    let repo = Repo::new();
    fs::write(repo.root.join("other.txt"), "staged\n").unwrap();
    git(&repo.root, &["add", "--", "other.txt"], None, None).unwrap();
    fs::write(repo.root.join("other.txt"), "staged plus unstaged\n").unwrap();
    fs::write(repo.root.join("untracked.txt"), "untouched\n").unwrap();
    let staged = git(&repo.root, &["show", ":other.txt"], None, None).unwrap();
    let snapshot = repo.snapshot();
    assert_eq!(snapshot.public()["files"], json!(["owned.txt"]));
    assert!(!snapshot.diff.contains("other.txt"));
    let outcome = commit_and_push(&snapshot, "chore(files): 更新当前对话文件内容", None).unwrap();
    assert_eq!(outcome["status"], "pushed");
    assert_eq!(
        git_text(&repo.root, &["log", "-1", "--format=%B"]).unwrap(),
        "chore(files): 更新当前对话文件内容"
    );
    assert_eq!(
        git_text(
            &repo.root,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        )
        .unwrap(),
        "owned.txt"
    );
    assert_eq!(
        git(&repo.root, &["show", ":other.txt"], None, None).unwrap(),
        staged
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("other.txt")).unwrap(),
        "staged plus unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("untracked.txt")).unwrap(),
        "untouched\n"
    );
    assert_eq!(
        git_text(&repo.remote, &["rev-parse", "refs/heads/main"]).unwrap(),
        outcome["commit"].as_str().unwrap()
    );
}

#[test]
fn rejects_mixed_file_content_staging_and_stale_previews() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    fs::write(repo.root.join("owned.txt"), "new\nexternal\n").unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("归属")
    );
    assert!(
        commit_and_push(&prepared, "chore(files): 更新文件", None)
            .unwrap_err()
            .to_string()
            .contains("已变化")
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    git(&repo.root, &["add", "--", "owned.txt"], None, None).unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("暂存")
    );
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
}

#[test]
fn no_edits_reverted_committed_failed_or_non_git_have_no_eligible_files() {
    let repo = Repo::new();
    assert!(status(&repo.home, SESSION).is_err());
    let prepared = repo.snapshot();
    fs::write(repo.root.join("owned.txt"), "old\n").unwrap();
    assert!(status(&repo.home, SESSION).is_err());
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    commit_and_push(&prepared, "chore(files): 更新文件", None).unwrap();
    assert!(status(&repo.home, SESSION).is_err());
    let plain = repo.directory.path().join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(build_snapshot(&repo.home, SESSION, &plain).is_err());
    let hook = repo.hook(
        SESSION,
        "apply_patch",
        json!("*** Begin Patch\n*** Delete File: other.txt\n*** End Patch"),
    );
    repo.observe(&hook).unwrap();
    repo.finish(hook, &[("other.txt", None)], json!("Error: edit failed"));
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
fn rejects_conflict_detached_head_index_lock_and_unpublished_commits() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    fs::write(repo.root.join(".git/MERGE_HEAD"), &prepared.head).unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("合并")
    );
    fs::remove_file(repo.root.join(".git/MERGE_HEAD")).unwrap();
    fs::write(repo.root.join(".git/index.lock"), "busy").unwrap();
    assert!(
        commit_and_push(&prepared, "chore(files): 更新文件", None)
            .unwrap_err()
            .to_string()
            .contains("暂存区正被")
    );
    fs::remove_file(repo.root.join(".git/index.lock")).unwrap();
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "unpublished"],
        None,
        None,
    )
    .unwrap();
    let newer = snapshot(&repo.home, SESSION).unwrap();
    assert!(
        commit_and_push(&newer, "chore(files): 更新文件", None)
            .unwrap_err()
            .to_string()
            .contains("不同步")
    );
    git(&repo.root, &["checkout", "--detach"], None, None).unwrap();
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
fn remote_advance_stops_before_creating_commit() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let tree = git_text(&repo.root, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let remote_commit = git(
        &repo.root,
        &[
            "commit-tree",
            &tree,
            "-p",
            &prepared.head,
            "-m",
            "remote advance",
        ],
        None,
        None,
    )
    .unwrap();
    let remote_commit = std::str::from_utf8(&remote_commit).unwrap().trim();
    git(
        &repo.root,
        &[
            "push",
            "origin",
            &format!("{remote_commit}:refs/heads/main"),
        ],
        None,
        None,
    )
    .unwrap();
    assert!(
        commit_and_push(&prepared, "chore(files): 更新文件", None)
            .unwrap_err()
            .to_string()
            .contains("不同步")
    );
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
}

#[cfg(unix)]
#[test]
fn remote_rejection_keeps_commit_and_worktree_without_rollback() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let hook = repo.remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'rejected by policy' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let outcome = commit_and_push(&prepared, "chore(files): 更新文件", None).unwrap();
    assert_eq!(outcome["status"], "committed");
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        outcome["commit"].as_str().unwrap()
    );
    assert_eq!(
        git_text(&repo.remote, &["rev-parse", "refs/heads/main"]).unwrap(),
        prepared.head
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("owned.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn tracks_add_delete_but_rejects_ignored_and_outside_paths() {
    let repo = Repo::new();
    let hook = repo.hook(SESSION, "apply_patch", json!("*** Begin Patch\n*** Add File: added.txt\n+created\n*** Delete File: owned.txt\n*** End Patch"));
    repo.observe(&hook).unwrap();
    repo.finish(
        hook,
        &[("added.txt", Some("created\n")), ("owned.txt", None)],
        json!("Success. Updated the following files:\nA added.txt\nD owned.txt"),
    );
    assert_eq!(snapshot(&repo.home, SESSION).unwrap().changes.len(), 2);
    fs::write(repo.root.join(".git/info/exclude"), "added.txt\n").unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("忽略")
    );
    for path in ["../outside.txt", ".git/config", "/outside.txt"] {
        assert!(relative_path(&repo.root, &repo.root, path).is_err());
    }
}

#[test]
fn existing_dirty_files_and_overwritten_untracked_files_cannot_be_claimed() {
    let repo = Repo::new();
    fs::write(repo.root.join("owned.txt"), "user\n").unwrap();
    repo.edit(SESSION, "owned.txt", "user", "new");
    assert!(snapshot(&repo.home, SESSION).is_err());
    let repo = Repo::new();
    fs::write(repo.root.join("added.txt"), "user\n").unwrap();
    let hook = repo.hook(
        SESSION,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: added.txt\n+new\n*** End Patch"),
    );
    repo.observe(&hook).unwrap();
    repo.finish(
        hook,
        &[("added.txt", Some("new\n"))],
        json!("Success. Updated the following files:\nA added.txt"),
    );
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
fn directory_fastctx_claims_only_reported_actual_edits_and_keeps_unmodified_claims() {
    let repo = Repo::new();
    repo.edit(OTHER, "other.txt", "other", "someone");
    let hook = repo.hook(
        SESSION,
        "mcp__codey_fastctx__replace",
        json!({
            "path":repo.root, "glob":["owned.txt"], "pattern":"o", "replacement":"O", "literal":true
        }),
    );
    repo.fastctx(hook);
    assert_eq!(
        snapshot(&repo.home, SESSION).unwrap().public()["files"],
        json!(["owned.txt"])
    );
    assert_eq!(
        snapshot(&repo.home, OTHER).unwrap().public()["files"],
        json!(["other.txt"])
    );
}

#[test]
fn no_op_and_dry_run_do_not_claim_files_and_incomplete_or_overlapping_calls_stop_submission() {
    let repo = Repo::new();
    let args = json!({"path":repo.root.join("owned.txt"), "pattern":"old", "replacement":"old", "literal":true});
    let hook = repo.hook(SESSION, "mcp__codey_fastctx__replace", args.clone());
    repo.observe(&hook).unwrap();
    repo.finish(
        hook,
        &[],
        json!(format!(
            "{}: 1 replacement\n\n(Complete: 1 replacement in 1 file.)",
            repo.root.join("owned.txt").display()
        )),
    );
    assert!(status(&repo.home, SESSION).is_err());
    let mut dry = repo.hook(SESSION, "mcp__codey_fastctx__replace", args);
    dry["tool_input"]["dry_run"] = json!(true);
    repo.observe(&dry).unwrap();
    repo.snapshot();
    let first = repo.hook(
        SESSION,
        "apply_patch",
        json!("*** Begin Patch\n*** Update File: owned.txt\n@@\n-new\n+next\n*** End Patch"),
    );
    let mut second = first.clone();
    second["turn_id"] = json!("another-turn");
    repo.observe(&first).unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("未完成")
    );
    repo.observe(&second).unwrap();
    repo.finish(
        first,
        &[("owned.txt", Some("next\n"))],
        json!("Success. Updated the following files:\nM owned.txt"),
    );
    repo.finish(
        second,
        &[],
        json!("Success. Updated the following files:\nM owned.txt"),
    );
    assert!(snapshot(&repo.home, SESSION).is_err());
}

#[test]
fn parent_and_child_edits_are_combined_for_both_hook_session_conventions() {
    for hook_uses_child_session in [false, true] {
        let repo = Repo::new();
        repo.snapshot();
        let mut hook = repo.child();
        if hook_uses_child_session {
            hook["session_id"] = json!(CHILD);
        }
        repo.fastctx(hook);
        let prepared = snapshot(&repo.home, SESSION).unwrap();
        assert_eq!(
            prepared.public()["files"],
            json!(["other.txt", "owned.txt"])
        );
        assert!(status(&repo.home, CHILD).is_err());
        assert_eq!(
            commit_and_push(
                &prepared,
                "chore(files): 更新主代理和子代理修改的文件",
                None
            )
            .unwrap()["status"],
            "pushed"
        );
    }
}

#[test]
fn unknown_wrong_parent_fenced_and_other_workspace_children_are_rejected() {
    use crate::subagent_orchestrator::pre_interrupt_agent;
    let repo = Repo::new();
    let hook = repo.child();
    let mut wrong = hook.clone();
    wrong["session_id"] = json!(OTHER);
    assert!(repo.observe(&wrong).is_err());
    wrong = hook.clone();
    wrong["agent_type"] = json!("codey_quick_scan");
    assert!(repo.observe(&wrong).is_err());
    wrong = hook.clone();
    wrong["agent_id"] = json!(OTHER);
    assert!(repo.observe(&wrong).is_err());
    let separate = repo.directory.path().join("elsewhere");
    fs::create_dir(&separate).unwrap();
    wrong = hook.clone();
    wrong["cwd"] = json!(separate);
    assert!(repo.observe(&wrong).is_err());
    pre_interrupt_agent(
        &repo.home.join(crate::subagent_gate::STATE_DIRECTORY),
        RUNTIME,
        SESSION,
        Some(&json!({"target":CHILD})),
        12,
    )
    .unwrap();
    assert!(repo.observe(&hook).is_err());
    assert!(status(&repo.home, SESSION).is_err());
}

#[test]
fn scoped_exec_patch_is_supported_but_arbitrary_scripts_do_not_claim_edits() {
    let repo = Repo::new();
    let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch";
    let hook = repo.hook(
        SESSION,
        "functions.exec",
        json!(format!("text(await tools.apply_patch({}));", json!(patch))),
    );
    repo.observe(&hook).unwrap();
    repo.finish(
        hook,
        &[("owned.txt", Some("new\n"))],
        json!("Success. Updated the following files:\nM owned.txt"),
    );
    assert_eq!(snapshot(&repo.home, SESSION).unwrap().changes.len(), 1);
    let script = repo.hook(
        OTHER,
        "functions.exec",
        json!("await tools.exec_command({cmd:'sed arbitrary-edit'});"),
    );
    repo.observe(&script).unwrap();
    assert!(status(&repo.home, OTHER).is_err());
}

#[test]
fn preview_tokens_are_bound_to_session_model_state_and_single_use() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let preview = save_preview(
        prepared.clone(),
        "chore(files): 更新文件".into(),
        "model".into(),
    )
    .unwrap();
    let token = preview["token"].as_str().unwrap();
    assert!(execute(&repo.home, OTHER, token, "model", true).is_err());
    assert!(execute(&repo.home, SESSION, token, "model", true).is_err());
    let preview = save_preview(
        prepared.clone(),
        "chore(files): 更新文件".into(),
        "model".into(),
    )
    .unwrap();
    assert!(
        execute(
            &repo.home,
            SESSION,
            preview["token"].as_str().unwrap(),
            "changed-model",
            true
        )
        .is_err()
    );
    let preview = save_preview(
        prepared.clone(),
        "chore(files): 更新文件".into(),
        "model".into(),
    )
    .unwrap();
    let token = preview["token"].as_str().unwrap();
    PREVIEWS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .get_mut(token)
        .unwrap()
        .created = Instant::now() - PREVIEW_LIFETIME;
    assert!(execute(&repo.home, SESSION, token, "model", true).is_err());
    let preview = save_preview(
        prepared.clone(),
        "chore(files): 更新文件".into(),
        "model".into(),
    )
    .unwrap();
    fs::write(repo.root.join("owned.txt"), "changed\n").unwrap();
    assert!(
        execute(
            &repo.home,
            SESSION,
            preview["token"].as_str().unwrap(),
            "model",
            true
        )
        .is_err()
    );
    fs::write(repo.root.join("owned.txt"), "new\n").unwrap();
    let preview = save_preview(prepared, "chore(files): 更新文件".into(), "model".into()).unwrap();
    let token = preview["token"].as_str().unwrap();
    assert_eq!(
        execute(&repo.home, SESSION, token, "model", true).unwrap()["status"],
        "pushed"
    );
    assert!(execute(&repo.home, SESSION, token, "model", true).is_err());
}

#[test]
fn execute_supports_commit_only_without_push() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let preview = save_preview(
        prepared,
        "chore(files): 仅提交不推送".into(),
        "model".into(),
    )
    .unwrap();
    let token = preview["token"].as_str().unwrap();
    let result = execute(&repo.home, SESSION, token, "model", false).unwrap();
    assert_eq!(result["status"], "committed");
    assert_eq!(result["message"], "当前对话文件已提交到本地仓库");
    assert!(result["commit"].as_str().is_some());
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(head, result["commit"].as_str().unwrap());
    // 远端分支应当仍停留在 initial commit，没有被推送
    let remote_head = git_text(&repo.remote, &["rev-parse", "HEAD"]).unwrap();
    assert_ne!(head, remote_head);
}

#[cfg(unix)]
#[test]
fn commit_hooks_are_not_bypassed_and_signing_errors_leave_head_untouched() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let hook = repo.root.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        commit_and_push(&prepared, "chore(files): 更新文件", None)
            .unwrap_err()
            .to_string()
            .contains("Git 钩子")
    );
    fs::remove_file(hook).unwrap();
    git(
        &repo.root,
        &["config", "commit.gpgsign", "true"],
        None,
        None,
    )
    .unwrap();
    git(
        &repo.root,
        &["config", "gpg.program", "/missing-codey-test-gpg"],
        None,
        None,
    )
    .unwrap();
    assert!(commit_and_push(&prepared, "chore(files): 更新文件", None).is_err());
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
}

#[test]
fn validates_chinese_messages_and_persistent_configuration() {
    assert_eq!(
        validate_message(" fix(conversation-git): 修复文件范围校验 \n").unwrap(),
        "fix(conversation-git): 修复文件范围校验"
    );
    for message in [
        "",
        "Update files",
        "```修复文件```",
        "修复\0文件",
        "修复文件范围校验",
        "fix(): 修复范围校验",
        "unknown(git): 修复范围校验",
        "Fix(git): 修复范围校验",
        "fix(提交): 修复范围校验",
        "fix(git-): 修复范围校验",
        "fix(git//scope): 修复范围校验",
        "fix(git scope): 修复范围校验",
        "fix(git)：修复范围校验",
        "fix(git):修复范围校验",
        "fix(git):  修复范围校验",
        "fix(git): Update files\n\n修复范围校验",
        "fix(git): 修复范围校验\n直接附加正文",
    ] {
        assert!(validate_message(message).is_err());
    }
    for kind in [
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore",
        "revert",
    ] {
        let message = format!("{kind}(local-router): 更新路由处理");
        assert_eq!(validate_message(&message).unwrap(), message);
        let message = format!("{kind}: 更新路由处理");
        assert_eq!(validate_message(&message).unwrap(), message);
    }
    let body = "feat(api/v2)!: 调整请求参数结构\n\n- 移除旧参数并使用新字段";
    assert_eq!(validate_message(body).unwrap(), body);
    assert!(validate_message(&format!("fix({}): 修复范围校验", "a".repeat(41))).is_err());
    assert!(validate_message(&format!("fix(git): {}", "修".repeat(100))).is_err());
    let config: crate::config::CodeyConfig = serde_json::from_str("{}").unwrap();
    assert!(!config.conversation_git.enabled);
    assert!(
        ConversationGitConfig {
            enabled: true,
            model: String::new()
        }
        .validate()
        .is_err()
    );
    let configured = ConversationGitConfig {
        enabled: true,
        model: "route/model".into(),
    };
    assert_eq!(
        serde_json::from_str::<ConversationGitConfig>(&serde_json::to_string(&configured).unwrap())
            .unwrap(),
        configured
    );
    let home = tempfile::tempdir().unwrap();
    let store = crate::config::ConfigStore::new(home.path().join("config.json"));
    let mut saved = config;
    saved.conversation_git = configured.clone();
    store.save(&saved).unwrap();
    assert_eq!(store.load().unwrap().conversation_git, configured);
}

#[test]
fn malformed_commit_messages_cannot_create_previews_or_change_git_state() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let head = git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap();
    let index = fs::read(index_path(&repo.root).unwrap()).unwrap();
    let disk = fs::read(repo.root.join("owned.txt")).unwrap();
    assert!(save_preview(prepared.clone(), "更新文件".into(), "model".into()).is_err());
    assert!(commit_and_push(&prepared, "修复文件", None).is_err());
    assert_eq!(git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(), head);
    assert_eq!(fs::read(index_path(&repo.root).unwrap()).unwrap(), index);
    assert_eq!(fs::read(repo.root.join("owned.txt")).unwrap(), disk);
}

#[tokio::test]
async fn model_receives_full_diff_and_conventional_chinese_message_rules() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(split) = bytes.windows(4).position(|slice| slice == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..split]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= split + 4 + length {
                    break;
                }
            }
            assert!(n > 0);
        }
        let request = String::from_utf8(bytes).unwrap();
        assert!(request.contains("diff --git a/owned.txt b/owned.txt"));
        assert!(request.contains("diff --git a/owned_extra.txt b/owned_extra.txt"));
        assert!(!request.contains("other.txt"));
        let split = request.find("\r\n\r\n").unwrap();
        let payload: Value = serde_json::from_str(&request[split + 4..]).unwrap();
        assert!(
            payload["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| { item["role"] == "system" && item["content"] == MESSAGE_INSTRUCTION })
        );
        assert!(MESSAGE_INSTRUCTION.contains("正文可选"));
        assert!(MESSAGE_INSTRUCTION.contains("任何改动都允许只写标题"));
        assert!(MESSAGE_INSTRUCTION.contains("不编造影响、测试结果或性能收益"));
        let body =
            json!({"choices":[{"message":{"role":"assistant", "content":"chore(files): 更新当前对话文件内容"}}]})
                .to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let config = crate::config::PromptOptimizationConfig {
        base_url: format!("http://{address}/v1"),
        api_key: "test".into(),
        model: "model".into(),
        instruction: MESSAGE_INSTRUCTION.into(),
        upstream_protocol: crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into(),
        ..Default::default()
    };
    let result = crate::prompt_optimization::optimize_prompt_resolved(
        &reqwest::Client::new(),
        &crate::prompt_optimization::ResolvedPromptOptimizationConfig::from_custom(&config),
        "diff --git a/owned.txt b/owned.txt\n-old\n+new\ndiff --git a/owned_extra.txt b/owned_extra.txt\n-old\n+new\n",
    )
    .await
    .unwrap();
    assert_eq!(
        validate_message(&result).unwrap(),
        "chore(files): 更新当前对话文件内容"
    );
    server.await.unwrap();
}

#[test]
fn commit_body_is_optional_and_model_details_are_preserved() {
    let title = "fix: 更新文件内容";
    let scoped = "fix(files): 更新文件内容\n\n- 调整文件内容，保留原有接口。";
    let unscoped = "fix: 更新文件内容\n\n- 调整文件内容，保留原有接口。";
    assert_eq!(validate_message(title).unwrap(), title);
    assert_eq!(validate_message(&format!("{title}\n\n  ")).unwrap(), title);
    assert_eq!(validate_message(scoped).unwrap(), scoped);
    assert_eq!(validate_message(unscoped).unwrap(), unscoped);
    assert!(validate_message("fix: 更新内容\n缺少空行").is_err());
}

#[tokio::test]
async fn unavailable_model_does_not_change_git_state() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let config = crate::config::PromptOptimizationConfig {
        base_url: format!("http://{address}/v1"),
        api_key: "test".into(),
        model: "unavailable".into(),
        ..Default::default()
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert!(
        crate::prompt_optimization::optimize_prompt_resolved(
            &client,
            &crate::prompt_optimization::ResolvedPromptOptimizationConfig::from_custom(&config),
            &prepared.diff
        )
        .await
        .is_err()
    );
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
    assert_eq!(
        digest(&fs::read(index_path(&repo.root).unwrap()).unwrap()),
        prepared.index_hash
    );
}

#[test]
fn remote_target_changes_after_preview_are_rejected() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let mut target = push_target(&prepared).unwrap();
    target.1 = "refs/heads/different".into();
    assert!(
        commit_and_push(&prepared, "chore(files): 更新文件", Some(&target))
            .unwrap_err()
            .to_string()
            .contains("推送目标已变化")
    );
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
}

#[test]
fn final_tree_rejects_extra_files_or_different_content() {
    let repo = Repo::new();
    let prepared = repo.snapshot();
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index");
    git(
        &repo.root,
        &["read-tree", &prepared.head],
        Some(&index),
        None,
    )
    .unwrap();
    stage(&index, &repo.root, &prepared.changes).unwrap();
    let extra = Change {
        path: "other.txt".into(),
        before: None,
        after: Some(Entry {
            mode: "100644".into(),
            bytes: b"unexpected\n".to_vec(),
        }),
        disk: None,
    };
    stage(&index, &repo.root, &[extra]).unwrap();
    let tree =
        String::from_utf8(git(&repo.root, &["write-tree"], Some(&index), None).unwrap()).unwrap();
    assert!(verify_tree(&repo.root, &prepared.head, tree.trim(), &prepared.changes).is_err());
    git(
        &repo.root,
        &["read-tree", &prepared.head],
        Some(&index),
        None,
    )
    .unwrap();
    let mut changed = prepared.changes.clone();
    changed[0].after.as_mut().unwrap().bytes = b"unapproved\n".to_vec();
    stage(&index, &repo.root, &changed).unwrap();
    let tree =
        String::from_utf8(git(&repo.root, &["write-tree"], Some(&index), None).unwrap()).unwrap();
    assert!(verify_tree(&repo.root, &prepared.head, tree.trim(), &prepared.changes).is_err());
    assert_eq!(
        git_text(&repo.root, &["rev-parse", "HEAD"]).unwrap(),
        prepared.head
    );
}

#[cfg(unix)]
#[test]
fn preview_stops_before_index_hooks_can_run() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    repo.snapshot();
    let hook = repo.root.join(".git/hooks/post-index-change");
    fs::write(&hook, "#!/bin/sh\nprintf modified > hook-ran\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        snapshot(&repo.home, SESSION)
            .unwrap_err()
            .to_string()
            .contains("Git 钩子")
    );
    assert!(!repo.root.join("hook-ran").exists());
}

#[test]
fn embedded_repositories_are_not_part_of_the_parent_conversation_scope() {
    let repo = Repo::new();
    let nested = repo.root.join("nested");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-b", "main"], None, None).unwrap();
    let hook = repo.hook(
        SESSION,
        "apply_patch",
        json!("*** Begin Patch\n*** Add File: nested/new.txt\n+foreign workspace\n*** End Patch"),
    );
    assert!(
        repo.observe(&hook)
            .unwrap_err()
            .to_string()
            .contains("嵌套 Git 工作区")
    );
    assert!(status(&repo.home, SESSION).is_err());
}
