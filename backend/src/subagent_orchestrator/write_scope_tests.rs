use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier};
use tempfile::{TempDir, tempdir};

struct Fixture {
    state: TempDir,
    workspace: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            state: tempdir().unwrap(),
            workspace: tempdir().unwrap(),
        }
    }

    fn spawn(&self, session: &str, task: &str) {
        let input = json!({"task_name": task, "agent_type": "codey_worker", "message": "Work"});
        assert_eq!(
            pre_spawn_with_workspace_and_turn(
                self.state.path(),
                "runtime",
                session,
                Some(&input),
                RootHookContext::new(self.workspace.path().to_str(), 0, 10),
            )
            .unwrap(),
            None
        );
        post_spawn(
            self.state.path(),
            "runtime",
            session,
            Some(&input),
            Some(&json!({"agent_id": task})),
            11,
        )
        .unwrap();
    }

    fn tool(&self, session: &str, task: &str, name: &str, input: Value) -> Option<String> {
        authorize_child_tool_with_workspace(
            self.state.path(),
            "runtime",
            session,
            ChildToolContext {
                agent_id: task,
                agent_type: None,
                transcript_path: None,
                tool_name: name,
                tool_input: Some(&input),
            },
            self.workspace.path().canonicalize().unwrap().to_str(),
            20,
        )
        .unwrap()
    }

    fn edit(&self, session: &str, task: &str, path: &str) -> Option<String> {
        self.tool(
            session,
            task,
            "apply_patch",
            json!(format!(
                "*** Begin Patch\n*** Add File: {path}\n+content\n*** End Patch\n"
            )),
        )
    }

    fn claims(&self, session: &str, task: &str) -> Reservation {
        LedgerStore::open(self.state.path(), session)
            .unwrap()
            .load("runtime", session, 21)
            .unwrap()
            .unwrap()
            .reservations[task]
            .clone()
    }
}

#[test]
fn different_files_can_be_edited_in_parallel_across_and_within_sessions() {
    for same_session in [false, true] {
        let f = Fixture::new();
        let other = if same_session { "a" } else { "b" };
        f.spawn("a", "writer_a");
        f.spawn(other, "writer_b");
        assert_eq!(f.edit("a", "writer_a", "a.rs"), None);
        assert_eq!(f.edit(other, "writer_b", "b.rs"), None);
        let denied = f.edit(other, "writer_b", "a.rs").unwrap();
        assert!(denied.contains("CODEY_SUBAGENT_WORKSPACE_BUSY"));
        let claims = f.claims(other, "writer_b");
        assert!(!claims.workspace_write_claimed);
        assert!(!claims.unbounded_write_claimed);
        assert_eq!(claims.file_write_claims.len(), 1);
        assert_eq!(f.edit(other, "writer_b", "c.rs"), None);
    }
}

#[test]
fn racing_writers_admit_both_for_distinct_files_and_exactly_one_for_the_same_file() {
    for same_file in [false, true] {
        for _ in 0..8 {
            let f = Arc::new(Fixture::new());
            f.spawn("a", "writer_a");
            f.spawn("b", "writer_b");
            let barrier = Arc::new(Barrier::new(2));
            let handles = [
                ("a", "writer_a", "a.rs"),
                ("b", "writer_b", if same_file { "a.rs" } else { "b.rs" }),
            ]
            .into_iter()
            .map(|(session, task, file)| {
                let f = Arc::clone(&f);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    f.edit(session, task, file)
                })
            })
            .collect::<Vec<_>>();
            let results = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                results.iter().filter(|result| result.is_none()).count(),
                if same_file { 1 } else { 2 }
            );
        }
    }
}

#[test]
fn multi_file_patch_acquires_all_resources_or_none_and_rename_claims_both_names() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "occupied.rs"), None);
    let patch = "*** Begin Patch\n*** Update File: source.rs\n*** Move to: target.rs\n@@\n-old\n+new\n*** Delete File: occupied.rs\n*** End Patch";
    assert!(
        f.tool("b", "writer_b", "apply_patch", json!(patch))
            .is_some()
    );
    assert!(f.claims("b", "writer_b").file_write_claims.is_empty());
    subagent_stopped(f.state.path(), "runtime", "a", "writer_a", 22).unwrap();
    assert_eq!(
        f.tool("b", "writer_b", "apply_patch", json!({"input": patch})),
        None
    );
    let claims = f.claims("b", "writer_b");
    assert_eq!(claims.file_write_claims.len(), 3);
    f.spawn("c", "writer_c");
    for file in ["source.rs", "target.rs", "occupied.rs"] {
        assert!(f.edit("c", "writer_c", file).is_some(), "{file}");
    }
}

#[test]
fn workspace_upgrade_waits_for_other_files_and_does_not_discard_previous_claims() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "a.rs"), None);
    assert_eq!(f.edit("b", "writer_b", "b.rs"), None);
    assert!(
        f.tool(
            "a",
            "writer_a",
            "functions.exec_command",
            json!({"cmd": "cargo test"})
        )
        .is_some()
    );
    assert!(!f.claims("a", "writer_a").workspace_write_claimed);
    assert_eq!(f.claims("a", "writer_a").file_write_claims.len(), 1);
    assert_eq!(f.edit("a", "writer_a", "c.rs"), None);
    subagent_stopped(f.state.path(), "runtime", "b", "writer_b", 22).unwrap();
    assert_eq!(
        f.tool(
            "a",
            "writer_a",
            "exec_command",
            json!({"cmd": "cargo test"})
        ),
        None
    );
    assert!(f.claims("a", "writer_a").workspace_write_claimed);
    assert_eq!(f.claims("a", "writer_a").file_write_claims.len(), 2);
    f.spawn("c", "writer_c");
    assert!(f.edit("c", "writer_c", "d.rs").is_some());
    assert_eq!(
        f.tool("c", "writer_c", "grep", json!({"pattern": "x"})),
        None
    );
}

#[test]
fn exact_literal_patch_calls_are_narrow_but_arbitrary_scripts_remain_broad() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "a.rs"), None);
    let patch = "*** Begin Patch\n*** Add File: b.rs\n+content\n*** End Patch";
    let literal = serde_json::to_string(patch).unwrap();
    for code in [
        format!("text(await tools.apply_patch({literal}));"),
        format!("// @exec: {{\"max_output_tokens\": 100}}\nawait tools.apply_patch({literal})"),
    ] {
        assert_eq!(f.tool("b", "writer_b", "functions.exec", json!(code)), None);
    }
    for code in [
        format!(
            "text(await tools.apply_patch({literal})); await tools.exec_command({{cmd:'rm a.rs'}})"
        ),
        "await tools.apply_patch(patch)".to_owned(),
        "await tools.apply_patch(`template`)".to_owned(),
    ] {
        assert!(
            f.tool("b", "writer_b", "functions.exec", json!(code))
                .is_some()
        );
    }
    assert!(!f.claims("b", "writer_b").workspace_write_claimed);
}

#[test]
fn native_single_file_replace_coordinates_paths_and_dry_run_has_no_claim() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    let a = f.workspace.path().join("a.rs");
    let b = f.workspace.path().join("b.rs");
    fs::write(&a, "old").unwrap();
    fs::write(&b, "old").unwrap();
    let args = |path: &Path, dry_run| json!({"path": path, "pattern": "old", "replacement": "new", "dry_run": dry_run});
    assert_eq!(
        f.tool(
            "a",
            "writer_a",
            "mcp__codey_fastctx__replace",
            args(&a, false)
        ),
        None
    );
    assert_eq!(
        f.tool(
            "b",
            "writer_b",
            "mcp__codey_fastctx__replace",
            args(&a, true)
        ),
        None
    );
    assert!(f.claims("b", "writer_b").file_write_claims.is_empty());
    assert_eq!(
        f.tool(
            "b",
            "writer_b",
            "mcp__codey_fastctx__replace",
            json!(args(&b, false).to_string())
        ),
        None
    );
    assert!(
        f.tool(
            "b",
            "writer_b",
            "mcp__codey_fastctx__replace",
            args(&a, false)
        )
        .is_some()
    );
    assert!(
        f.tool(
            "b",
            "writer_b",
            "mcp__codey_fastctx__replace",
            args(f.workspace.path(), false)
        )
        .is_some()
    );
    assert!(
        f.tool("b", "writer_b", "mcp__other__replace", args(&b, false))
            .is_some()
    );
}

#[test]
fn aliases_parent_replacements_and_external_targets_conflict() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    fs::create_dir(f.workspace.path().join("src")).unwrap();
    assert_eq!(f.edit("a", "writer_a", "src/file.rs"), None);
    assert!(f.edit("b", "writer_b", "src/../src/file.rs").is_some());
    assert!(f.edit("b", "writer_b", "src").is_some());
    let outside = tempdir().unwrap();
    let target = outside.path().join("external.rs");
    assert_eq!(f.edit("a", "writer_a", target.to_str().unwrap()), None);
    assert!(f.edit("b", "writer_b", target.to_str().unwrap()).is_some());
    assert_eq!(f.edit("b", "writer_b", "src/other.rs"), None);
}

#[cfg(unix)]
#[test]
fn symlinks_and_parent_components_use_filesystem_resolution_and_alias_claims() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    let outside = tempdir().unwrap();
    fs::create_dir(outside.path().join("nested")).unwrap();
    symlink(
        outside.path().join("nested"),
        f.workspace.path().join("alias"),
    )
    .unwrap();
    assert_eq!(f.edit("a", "writer_a", "alias/file.rs"), None);
    assert!(
        f.edit(
            "b",
            "writer_b",
            outside.path().join("nested/file.rs").to_str().unwrap()
        )
        .is_some()
    );
    assert!(f.edit("b", "writer_b", "alias").is_some());
    assert_eq!(f.edit("a", "writer_a", "alias/../parent.rs"), None);
    assert!(
        f.edit(
            "b",
            "writer_b",
            outside.path().join("parent.rs").to_str().unwrap()
        )
        .is_some()
    );
}

#[cfg(unix)]
#[test]
fn unresolved_symlinks_and_hard_links_use_unbounded_protection() {
    use std::os::unix::fs::symlink;
    for hard_link in [false, true] {
        let f = Fixture::new();
        f.spawn("a", "writer_a");
        f.spawn("b", "writer_b");
        let target = f.workspace.path().join("alias");
        if hard_link {
            let real = f.workspace.path().join("real");
            fs::write(&real, "content").unwrap();
            fs::hard_link(&real, &target).unwrap();
        } else {
            symlink("missing", &target).unwrap();
        }
        assert_eq!(f.edit("a", "writer_a", "alias"), None);
        assert!(f.claims("a", "writer_a").unbounded_write_claimed);
        assert!(f.edit("b", "writer_b", "different.rs").is_some());
    }
}

#[test]
fn narrow_claims_survive_interrupt_and_release_only_after_acknowledgement() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "a.rs"), None);
    let target = json!({"target": "writer_a"});
    pre_interrupt_agent(f.state.path(), "runtime", "a", Some(&target), 22).unwrap();
    assert!(f.edit("b", "writer_b", "a.rs").is_some());
    assert_eq!(f.edit("b", "writer_b", "b.rs"), None);
    assert!(f.edit("a", "writer_a", "c.rs").is_some());
    settle_interrupt_acknowledgement(
        f.state.path(),
        "runtime",
        "a",
        Some(&target),
        &InterruptAcknowledgement {
            prior_outcome: None,
            identifiers: vec![],
        },
        23,
    )
    .unwrap();
    assert_eq!(f.edit("b", "writer_b", "a.rs"), None);
}

#[test]
fn incomplete_patch_or_missing_cwd_cannot_obtain_partial_file_claims() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "a.rs"), None);
    for patch in [
        "*** Begin Patch\n*** Add File: b.rs\n+content",
        "*** Begin Patch\n*** Add File: b.rs\n+content\n*** Unknown: a.rs\n*** End Patch",
        "*** Begin Patch\n*** Update File: b.rs\n*** Move to: c.rs\n*** Move to: a.rs\n*** End Patch",
    ] {
        assert!(
            f.tool("b", "writer_b", "apply_patch", json!(patch))
                .is_some()
        );
        assert!(f.claims("b", "writer_b").file_write_claims.is_empty());
    }
    let patch = json!("*** Begin Patch\n*** Add File: b.rs\n+x\n*** End Patch");
    assert!(
        authorize_child_tool_with_context(
            f.state.path(),
            "runtime",
            "b",
            ChildToolContext {
                agent_id: "writer_b",
                agent_type: None,
                transcript_path: None,
                tool_name: "apply_patch",
                tool_input: Some(&patch)
            },
            24
        )
        .unwrap()
        .is_some()
    );
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn new_file_names_coordinate_case_insensitively() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "Name.rs"), None);
    assert!(f.edit("b", "writer_b", "name.rs").is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn new_file_names_coordinate_unicode_normalization() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(f.edit("a", "writer_a", "caf\u{e9}.rs"), None);
    assert!(f.edit("b", "writer_b", "cafe\u{301}.rs").is_some());
}

#[test]
fn relative_paths_follow_the_current_native_cwd_and_external_directories_remain_exclusive() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    let outside = tempdir().unwrap();
    let patch = json!("*** Begin Patch\n*** Add File: external.rs\n+x\n*** End Patch");
    assert_eq!(
        authorize_child_tool_with_workspace(
            f.state.path(),
            "runtime",
            "a",
            ChildToolContext {
                agent_id: "writer_a",
                agent_type: None,
                transcript_path: None,
                tool_name: "apply_patch",
                tool_input: Some(&patch)
            },
            outside.path().to_str(),
            20
        )
        .unwrap(),
        None
    );
    assert!(
        f.edit(
            "b",
            "writer_b",
            outside.path().join("external.rs").to_str().unwrap()
        )
        .is_some()
    );
    assert_eq!(f.edit("b", "writer_b", "external.rs"), None);
    let input = json!({"path": outside.path(), "pattern": "x", "replacement": "y"});
    assert!(
        f.tool("a", "writer_a", "mcp__codey_fastctx__replace", input)
            .is_some()
    );
    assert!(!f.claims("a", "writer_a").unbounded_write_claimed);
}

#[test]
fn an_existing_workspace_claim_cannot_bypass_an_external_file_claim() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    assert_eq!(
        f.tool(
            "a",
            "writer_a",
            "exec_command",
            json!({"cmd": "cargo test"})
        ),
        None
    );
    let outside = tempdir().unwrap();
    let target = outside.path().join("external.rs");
    assert_eq!(f.edit("b", "writer_b", target.to_str().unwrap()), None);
    assert!(f.edit("a", "writer_a", target.to_str().unwrap()).is_some());
    assert!(f.claims("a", "writer_a").workspace_write_claimed);
    assert!(f.claims("a", "writer_a").file_write_claims.is_empty());
}

#[test]
fn commands_with_an_explicit_external_workdir_cannot_claim_only_the_original_workspace() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    let outside = tempdir().unwrap();
    assert_eq!(
        f.edit(
            "b",
            "writer_b",
            outside.path().join("external.rs").to_str().unwrap()
        ),
        None
    );
    assert!(
        f.tool(
            "a",
            "writer_a",
            "exec_command",
            json!({"cmd": "cargo test", "workdir": outside.path()})
        )
        .is_some()
    );
    assert!(!f.claims("a", "writer_a").workspace_write_claimed);
    assert_eq!(
        f.tool(
            "a",
            "writer_a",
            "exec_command",
            json!({"cmd": "cargo test", "workdir": f.workspace.path()})
        ),
        None
    );
}

#[test]
fn ledger_size_limit_does_not_persist_unreadable_resource_records() {
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("a", "writer_b");
    let store = LedgerStore::open(f.state.path(), "a").unwrap();
    let before = fs::read(&store.ledger_path).unwrap();
    let mut ledger = store.load("runtime", "a", 20).unwrap().unwrap();
    for reservation in ledger.reservations.values_mut() {
        reservation.file_write_claims = (0..MAX_WRITE_PATHS)
            .map(|index| format!("/scope/{index}/{}", "a".repeat(4_000)))
            .collect();
    }
    assert!(store.save(&mut ledger, 21).is_err());
    assert_eq!(fs::read(&store.ledger_path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn symlink_ancestors_removed_by_parent_components_still_remain_protected() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.spawn("a", "writer_a");
    f.spawn("b", "writer_b");
    let outside = tempdir().unwrap();
    fs::create_dir(outside.path().join("nested")).unwrap();
    symlink(
        outside.path().join("nested"),
        f.workspace.path().join("alias"),
    )
    .unwrap();
    assert_eq!(f.edit("a", "writer_a", "alias/../parent.rs"), None);
    assert!(f.edit("b", "writer_b", "alias").is_some());
}
