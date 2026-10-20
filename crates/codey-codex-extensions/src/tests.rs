use super::*;
use std::{fs, io::Write};

fn cached_skill(f: &Fixture, relative: &str) -> PathBuf {
    let directory = f.home.join("plugins/cache").join(relative);
    fs::create_dir_all(&directory).unwrap();
    fs::copy(f.source.join("SKILL.md"), directory.join("SKILL.md")).unwrap();
    directory
}

fn cache_list(f: &Fixture) -> Value {
    f.service
        .dispatch(json!({"action":"list_skill_cache"}))
        .unwrap()
}

#[test]
fn skill_cache_is_separate_global_and_read_only() {
    let f = Fixture::new("");
    cached_skill(&f, "vendor/plugin/1/skills/demo");
    assert!(f.list()["skills"].as_array().unwrap().is_empty());
    let cache = cache_list(&f);
    let entry = &cache["skills"][0];
    assert_eq!(entry["readOnly"], true);
    assert_eq!(entry["canRemove"], true);
    assert_eq!(entry["canEdit"], false);
    assert_eq!(entry["canToggle"], false);
    let scoped = f.service.dispatch(json!({"action":"list_skill_cache","scope":{"kind":"project","projectPath":"/nonexistent-project"}})).unwrap();
    assert_eq!(scoped, cache);
    let read = f
        .service
        .dispatch(json!({"action":"read_skill_cache","id":entry["id"]}))
        .unwrap();
    assert_eq!(read["readOnly"], true);
    assert_eq!(
        read["content"],
        fs::read_to_string(f.source.join("SKILL.md")).unwrap()
    );
    assert_eq!(read["revision"], cache["revision"]);
    for action in ["save_skill", "set_skill_enabled", "uninstall_skill"] {
        assert!(
            f.mutate(
                json!({"action":action,"id":entry["id"],"enabled":false,"content":"anything"})
            )
            .is_err()
        );
    }
}

#[test]
fn skill_cache_delete_is_isolated_and_audited() {
    let f = Fixture::new("# preserve config\n");
    let directory = cached_skill(&f, "vendor/plugin/1/skills/demo");
    let neighbor = cached_skill(&f, "vendor/plugin/1/skills/neighbor");
    fs::create_dir_all(directory.join("resources/empty")).unwrap();
    fs::write(directory.join("resources/asset.txt"), "cached asset").unwrap();
    let parent_resource = directory
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugin.json");
    fs::write(&parent_resource, "plugin metadata").unwrap();
    let cache = cache_list(&f);
    let result = f.service.dispatch(json!({"action":"remove_skill_cache","id":skills::id(&directory.join("SKILL.md")),"revision":cache["revision"],"confirmed":true})).unwrap();
    assert!(!directory.exists());
    assert!(neighbor.join("SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(parent_resource).unwrap(),
        "plugin metadata"
    );
    assert_eq!(
        fs::read_to_string(f.home.join("config.toml")).unwrap(),
        "# preserve config\n"
    );
    assert!(!f.service.registry_path().exists());
    assert_eq!(result["cache"], cache_list(&f));
    assert_eq!(result["cache"]["skills"].as_array().unwrap().len(), 1);
    let audit = fs::read_to_string(f.data.join("audit.jsonl")).unwrap();
    let record: Value = serde_json::from_str(audit.lines().last().unwrap()).unwrap();
    assert_eq!(record["operation"], "remove_skill_cache");
    assert_eq!(record["targetCount"], 2);
    assert_eq!(record["state"], "committed");
}

#[test]
fn skill_cache_delete_requires_confirmation_and_current_resource_revision() {
    let f = Fixture::new("");
    let directory = cached_skill(&f, "vendor/plugin/1/skills/demo");
    let cache = cache_list(&f);
    let id = cache["skills"][0]["id"].clone();
    for confirmed in [Value::Null, json!(false)] {
        assert!(f.service.dispatch(json!({"action":"remove_skill_cache","id":id,"revision":cache["revision"],"confirmed":confirmed})).unwrap_err().to_string().contains("确认"));
    }
    assert!(!f.data.exists());
    let regular_revision = f.list()["revision"].clone();
    fs::write(directory.join("asset.txt"), "new asset").unwrap();
    assert_ne!(cache_list(&f)["revision"], cache["revision"]);
    assert_eq!(f.list()["revision"], regular_revision);
    assert!(f.service.dispatch(json!({"action":"remove_skill_cache","id":id,"revision":cache["revision"],"confirmed":true})).unwrap_err().to_string().contains("缓存已变化"));
    let latest = cache_list(&f);
    fs::write(directory.join("asset.txt"), "changed asset").unwrap();
    assert_ne!(cache_list(&f)["revision"], latest["revision"]);
    assert!(directory.join("SKILL.md").exists());
}

#[test]
fn skill_cache_rejects_paths_cache_roots_and_nested_skill_parents() {
    let f = Fixture::new("");
    let cache_root = cached_skill(&f, "");
    let plugin = cached_skill(&f, "vendor/plugin/1");
    let parent = cached_skill(&f, "vendor/plugin/1/skills/parent");
    cached_skill(&f, "vendor/plugin/1/skills/parent/child");
    let cache = cache_list(&f);
    for directory in [&cache_root, &plugin, &parent] {
        let id = skills::id(&directory.join("SKILL.md"));
        let entry = cache["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap();
        assert_eq!(entry["canRemove"], false);
        assert!(f.service.dispatch(json!({"action":"remove_skill_cache","id":id,"revision":cache["revision"],"confirmed":true})).is_err());
    }
    for id in [
        f.source.to_string_lossy().into_owned(),
        skills::id(&f.source.join("SKILL.md")),
        "../outside".to_owned(),
    ] {
        assert!(f.service.dispatch(json!({"action":"remove_skill_cache","id":id,"revision":cache["revision"],"confirmed":true,"sourcePath":f.source})).is_err());
    }
    assert!(f.source.join("SKILL.md").exists());
    assert!(parent.join("child/SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn skill_cache_rejects_linked_directories_and_resources() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("");
    let directory = cached_skill(&f, "vendor/plugin/1/skills/demo");
    let initial = cache_list(&f);
    symlink(&f.source, directory.join("linked-assets")).unwrap();
    let cache = cache_list(&f);
    assert_eq!(cache["skills"][0]["canRemove"], false);
    assert!(f.service.dispatch(json!({"action":"remove_skill_cache","id":initial["skills"][0]["id"],"revision":initial["revision"],"confirmed":true})).is_err());
    symlink(&f.source, directory.parent().unwrap().join("linked-skill")).unwrap();
    assert_eq!(cache_list(&f)["skills"].as_array().unwrap().len(), 1);
    assert!(f.source.join("SKILL.md").exists());
    let root_link = Fixture::new("");
    fs::create_dir_all(root_link.home.join("plugins")).unwrap();
    symlink(&f.source, root_link.home.join("plugins/cache")).unwrap();
    let blocked = cache_list(&root_link);
    assert!(blocked["skills"].as_array().unwrap().is_empty());
    assert!(blocked["warnings"].as_array().unwrap().len() > 1);
}

#[test]
fn creation_does_not_overwrite_and_enabling_requires_confirmation() {
    let f = Fixture::new("[mcp_servers.test]\ncommand='node'\nenabled=false\n");
    let original = fs::read(f.home.join("config.toml")).unwrap();
    assert!(f.mutate(json!({"action":"save_mcp","id":"test","configJson":{"command":"replacement"},"createOnly":true})).unwrap_err().to_string().contains("已存在"));
    assert!(
        f.mutate(json!({"action":"set_mcp_enabled","id":"test","enabled":true,"confirmed":false}))
            .unwrap_err()
            .to_string()
            .contains("确认")
    );
    // 编辑时省略 enabled 默认启用，不能绕过显式确认。
    assert!(
        f.mutate(
            json!({"action":"save_mcp","id":"test","configJson":{"command":"node"},"confirmed":false})
        )
        .unwrap_err()
        .to_string()
        .contains("确认")
    );
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), original);
    assert!(f.mutate(json!({"action":"save_mcp","id":"new","configJson":{"command":"node"},"createOnly":true,"confirmed":false})).is_err());
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), original);
    f.mutate(
        json!({"action":"save_mcp","id":"new","configJson":{"command":"node"},"createOnly":true}),
    )
    .unwrap();
    assert_eq!(
        f.service.mcp_configuration(&Scope::User, "new").unwrap()["enabled"],
        true
    );
}

#[test]
fn json_stdio_import_preserves_strings_and_starts_enabled() {
    let f = Fixture::new("# keep\nmodel='existing'\n");
    let config = json!({
        "type": "stdio",
        "command": "C:\\Program Files\\node.exe",
        "args": ["quoted \"argument\"", "line\nnext", "中文", "[mcp_servers.injected]"],
        "env": {"TOKEN": "private-\"quoted\"\\token\nnext"},
        "cwd": "C:\\MCP tools",
        "enabled": false,
        "required": true,
        "tool_timeout_sec": 45,
        "custom": {"nested": ["future", "value"]}
    });
    let result = f
        .mutate(json!({"action":"save_mcp","id":"local","configJson":config,"createOnly":true}))
        .unwrap();
    assert_eq!(result["applyStatus"], "reload-required");
    let stored = f.service.mcp_configuration(&Scope::User, "local").unwrap();
    let mut expected = config;
    expected.as_object_mut().unwrap().remove("type");
    expected["enabled"] = json!(true);
    assert_eq!(stored, expected);
    assert_eq!(f.list()["mcps"].as_array().unwrap().len(), 1);
    let displayed = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"local"}))
        .unwrap();
    assert!(!displayed.to_string().contains("private-"));
    assert!(result.get("snapshotId").is_none());
    assert!(!f.data.join("snapshots").exists());
    assert!(
        !fs::read_to_string(f.data.join("audit.jsonl"))
            .unwrap()
            .contains("private-")
    );
}

#[test]
fn json_http_import_normalizes_transport_and_headers() {
    for transport in ["http", "streamable-http", "streamableHttp"] {
        let f = Fixture::new("");
        f.mutate(json!({"action":"save_mcp","id":"remote","configJson":{
            "type":transport,"url":"https://example.invalid/mcp","headers":{"Authorization":"Bearer private-token","X-Custom":"quoted \"value\""},"bearer_token_env_var":"MCP_TOKEN","env_http_headers":{"X-Env":"MCP_CUSTOM"},"enabled_tools":["lookup"]
        }})).unwrap();
        let stored = f.service.mcp_configuration(&Scope::User, "remote").unwrap();
        assert!(stored.get("type").is_none());
        assert!(stored.get("headers").is_none());
        assert_eq!(
            stored["http_headers"]["Authorization"],
            "Bearer private-token"
        );
        assert_eq!(stored["http_headers"]["X-Custom"], "quoted \"value\"");
        assert_eq!(stored["bearer_token_env_var"], "MCP_TOKEN");
        assert_eq!(stored["enabled"], true);
        assert!(
            !f.service
                .dispatch(json!({"action":"get_mcp","id":"remote"}))
                .unwrap()
                .to_string()
                .contains("private-token")
        );
    }
}

#[test]
fn json_create_read_edit_and_export_reimport_roundtrip() {
    let f = Fixture::new("");
    f.mutate(
        json!({"action":"save_mcp","id":"original","createOnly":true,"configJson":{
            "command":"node","args":["server.js"],"env_vars":["MCP_TOKEN"]
        }}),
    )
    .unwrap();
    let read = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"original"}))
        .unwrap();
    assert_eq!(read["id"], "original");
    assert_eq!(read["revision"], f.list()["revision"]);
    assert!(read["configJson"].is_object());
    assert!(read.get("configToml").is_none());
    let mut config = read["configJson"].clone();
    config["args"] = json!(["updated.js"]);
    f.service.dispatch(json!({"action":"save_mcp","id":"original","revision":read["revision"],"configJson":config})).unwrap();
    let edited = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"original"}))
        .unwrap();
    assert_eq!(edited["configJson"]["args"][0], "updated.js");
    let export =
        serde_json::to_vec(&json!({"mcpServers":{"original":edited["configJson"]}})).unwrap();
    let import: Value = serde_json::from_slice(&export).unwrap();
    f.mutate(json!({"action":"save_mcp","id":"copy","createOnly":true,"configJson":import["mcpServers"]["original"]})).unwrap();
    assert_eq!(
        f.service
            .mcp_configuration(&Scope::User, "original")
            .unwrap(),
        f.service.mcp_configuration(&Scope::User, "copy").unwrap()
    );
}

#[test]
fn json_import_rejects_invalid_or_ambiguous_input_without_disclosure() {
    let f = Fixture::new("# original\n");
    for config in [
        json!("private-secret"),
        json!([{"command":"private-secret"}]),
        json!({"command":"node","args":"private-secret"}),
        json!({"type":"stdio","url":"https://example.invalid/private-secret"}),
        json!({"type":"http","command":"private-secret"}),
        json!({"type":"sse","url":"https://example.invalid/private-secret"}),
        json!({"type":"private-secret","command":"node"}),
        json!({"type":42,"command":"node"}),
        json!({"url":"https://example.invalid","headers":{"Authorization":"private-secret"},"http_headers":{"Authorization":"private-secret"}}),
        json!({"command":"node","headers":{"Authorization":"private-secret"}}),
        json!({"command":"node","env":{"TOKEN":null}}),
        json!({"command":"node","future":u64::MAX}),
        json!({"command":"node","enabled":"private-secret"}),
        json!({"url":"https://example.invalid","bearer_token":"private-secret"}),
        json!({"mcpServers":{"local":{"command":"private-secret"}}}),
        json!({"command":"node","env":{"TOKEN":mcp::MASK}}),
    ] {
        let error = f
            .mutate(json!({"action":"save_mcp","id":"test","configJson":config}))
            .unwrap_err();
        assert!(!format!("{error:#}").contains("private-secret"));
    }
    for request in [
        json!({"action":"save_mcp","id":"test","configJson":{"command":"node"},"configToml":"command='node'"}),
        json!({"action":"save_mcp","id":"test","configJson":null,"configToml":"command='node'"}),
        json!({"action":"save_mcp","id":"test"}),
        json!({"action":"save_mcp","id":"test","configToml":{}}),
        json!({"action":"save_mcp","id":"test","configToml":"command='node'"}),
    ] {
        assert!(f.mutate(request).is_err());
    }
    assert_eq!(
        fs::read_to_string(f.home.join("config.toml")).unwrap(),
        "# original\n"
    );
    assert!(!f.data.join("snapshots").exists());
}

#[test]
fn json_import_bounds_size_depth_and_node_count_before_locking() {
    let f = Fixture::new("");
    let mut nested = json!("leaf");
    for _ in 0..33 {
        nested = json!({"nested":nested});
    }
    for config in [
        json!({"command":"node","custom":nested}),
        json!({"command":"node","custom":vec![true;10000]}),
        json!({"command":"node","env":{"TOKEN":"x".repeat(fsutil::MAX_FILE as usize + 1)}}),
        json!({"command":"node","env":{"TOKEN":"\n".repeat(fsutil::MAX_FILE as usize / 2)}}),
    ] {
        let error = f
            .service
            .dispatch(json!({"action":"save_mcp","id":"test","configJson":config}))
            .unwrap_err();
        assert!(error.to_string().contains("限制"));
    }
    assert!(!f.data.exists());
    assert!(!f.home.join("config.toml.lock").exists());
}

#[test]
fn toml_management_payloads_are_rejected_before_locking() {
    let f = Fixture::new("# original\n");
    for request in [
        json!({"action":"save_mcp","id":"test","configToml":"command='node'"}),
        json!({"action":"save_mcp","id":"test","configToml":null,"configJson":{"command":"node"}}),
    ] {
        let error = f.service.dispatch(request).unwrap_err();
        assert!(error.to_string().contains("不支持 configToml"));
    }
    assert_eq!(
        fs::read_to_string(f.home.join("config.toml")).unwrap(),
        "# original\n"
    );
    assert!(!f.data.exists());
    assert!(!f.home.join("config.toml.lock").exists());
}

#[test]
fn json_import_respects_existing_identity_revision_and_credential_protection() {
    let f = Fixture::new(
        "[mcp_servers.test]\ncommand='node'\nenabled=false\nfuture='keep'\n[mcp_servers.test.env]\nTOKEN='private-token'\n",
    );
    let before = fs::read(f.home.join("config.toml")).unwrap();
    assert!(f.mutate(json!({"action":"save_mcp","id":"test","configJson":{"command":"changed"},"createOnly":true})).unwrap_err().to_string().contains("已存在"));
    assert!(f.mutate(json!({"action":"save_mcp","id":"test","configJson":{"command":"changed"},"confirmed":false})).unwrap_err().to_string().contains("确认"));
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), before);
    let revision = f.list()["revision"].clone();
    f.mutate(json!({"action":"save_mcp","id":"test","configJson":{"command":"changed","enabled":false,"env":mcp::MASK}})).unwrap();
    let stored = f.service.mcp_configuration(&Scope::User, "test").unwrap();
    assert_eq!(stored["env"]["TOKEN"], "private-token");
    assert_eq!(stored["future"], "keep");
    let changed = fs::read(f.home.join("config.toml")).unwrap();
    assert!(f.service.dispatch(json!({"action":"save_mcp","id":"next","configJson":{"command":"node"},"createOnly":true,"revision":revision})).unwrap_err().to_string().contains("配置已变化"));
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), changed);
}

struct Fixture {
    _temp: tempfile::TempDir,
    service: ExtensionService,
    home: PathBuf,
    data: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new(config: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home/.codex");
        let data = root.join("data");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), config).unwrap();
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: demo\ndescription: A test skill\n---\n# Test\n",
        )
        .unwrap();
        let service = ExtensionService::new(home.clone(), data.clone(), root.join("home"));
        Self {
            _temp: temp,
            service,
            home,
            data,
            source,
        }
    }
    fn list(&self) -> Value {
        self.service.dispatch(json!({"action":"list"})).unwrap()
    }
    fn mutate(&self, mut v: Value) -> Result<Value> {
        v["revision"] = self.list()["revision"].clone();
        if v.get("confirmed").is_none() {
            v["confirmed"] = json!(true);
        }
        self.service.dispatch(v)
    }
}

#[test]
fn reads_have_no_side_effects_and_mask_secrets() {
    let f = Fixture::new(
        "[mcp_servers.test]\ncommand = 'node'\n[mcp_servers.test.env]\nTOKEN = 'private-token'\n",
    );
    let initial = fs::read(f.home.join("config.toml")).unwrap();
    f.list();
    let result = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap();
    assert_eq!(result["configJson"]["env"], mcp::MASK);
    assert!(result.get("configToml").is_none());
    assert!(!result.to_string().contains("private-token"));
    assert!(!f.data.exists());
    assert!(!f.home.join("config.toml.lock").exists());
    assert_eq!(initial, fs::read(f.home.join("config.toml")).unwrap());
}

#[test]
fn import_starts_disabled_and_preserves_existing_inline_rules() {
    let original = "# Keep existing rules\nskills = { config = [{path = '/existing/SKILL.md', enabled = false}] }\n";
    let f = Fixture::new(original);
    let installed = f
        .mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let entry = &installed["inventory"]["skills"][0];
    assert_eq!(entry["enabled"], false);
    let text = fs::read_to_string(f.home.join("config.toml")).unwrap();
    assert!(text.contains("/existing/SKILL.md"));
    assert!(text.contains("# Keep existing rules"));
    assert!(installed.get("snapshotId").is_none());
    assert!(!f.data.join("snapshots").exists());
    assert!(f.home.join("skills/demo/SKILL.md").exists());
}

#[test]
fn preserves_unrelated_config_unknown_fields_and_secrets() {
    let f = Fixture::new(
        "# user comment\nmodel = 'keep-model'\n[mcp_servers.test]\ncommand = 'node'\nfuture_option = 'keep'\n[mcp_servers.test.env]\nTOKEN = 'private-token'\n[other]\n# other comment\nvalue = 42\n",
    );
    let mut body = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap()["configJson"]
        .clone();
    body["command"] = json!("node-next");
    let result = f
        .mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
        .unwrap();
    let config = fs::read_to_string(f.home.join("config.toml")).unwrap();
    for expected in [
        "# user comment",
        "keep-model",
        "# other comment",
        "value = 42",
        "future_option = 'keep'",
        "private-token",
        "node-next",
    ] {
        assert!(config.contains(expected), "missing {expected}");
    }
    assert!(result.get("snapshotId").is_none());
    assert!(!f.data.join("snapshots").exists());
    assert!(
        !fs::read_to_string(f.data.join("audit.jsonl"))
            .unwrap()
            .contains("private-token")
    );
}

#[test]
fn inline_tables_roundtrip() {
    let f = Fixture::new(
        "# inline root comment\nmcp_servers = { test = { command = 'node', env = { TOKEN = 'value' }, custom = 1 } }\n",
    );
    let body = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap()["configJson"]
        .clone();
    f.mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
        .unwrap();
    assert_eq!(
        f.service.mcp_configuration(&Scope::User, "test").unwrap()["env"]["TOKEN"],
        "value"
    );
}

#[test]
fn skill_toggle_preserves_rule_shape_unknown_fields_and_duplicates() {
    for text in [
        "# root comment\nskills = { config = [{path='/test/SKILL.md', enabled=false, future='one'}, {path='/test/SKILL.md', enabled=false, future='two'}] }\n",
        "[skills]\n[[skills.config]]\npath='/test/SKILL.md'\nenabled=false\nfuture='one'\n[[skills.config]]\npath='/test/SKILL.md'\nenabled=false\nfuture='two'\n",
    ] {
        let mut doc = mcp::parse(Some(text.as_bytes())).unwrap();
        skill_config::set_enabled(&mut doc, "/test/SKILL.md", true).unwrap();
        let output = doc.to_string();
        let parsed = mcp::parse(Some(output.as_bytes())).unwrap();
        assert!(skill_config::disabled(&parsed).unwrap().0.is_empty());
        assert!(output.contains("one") && output.contains("two"));
        assert_eq!(output.matches("/test/SKILL.md").count(), 2);
        if text.starts_with('#') {
            assert!(output.contains("# root comment"));
        }
    }
}

#[test]
fn legacy_bearer_token_can_only_be_preserved_or_removed() {
    let f = Fixture::new(
        "[mcp_servers.test]\nurl='https://example.invalid/mcp'\nbearer_token='private-token'\n",
    );
    let mut body = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap()["configJson"]
        .clone();
    assert!(!body.to_string().contains("private-token"));
    body["tool_timeout_sec"] = json!(20);
    f.mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
        .unwrap();
    assert_eq!(
        f.service.mcp_configuration(&Scope::User, "test").unwrap()["bearer_token"],
        "private-token"
    );
    body["bearer_token"] = json!("replacement-token");
    assert!(
        f.mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
            .is_err()
    );
}

#[test]
fn oversized_generated_file_is_rejected_before_write() {
    let f = Fixture::new("# original\n");
    let path = f.home.join("config.toml");
    let content = vec![b'x'; fsutil::MAX_FILE as usize + 1];
    assert!(Change::new(path.clone(), Some(content.clone())).is_err());
    assert!(fsutil::apply(&path, &Some(content), None).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "# original\n");
    assert!(!f.data.exists());
}

#[cfg(unix)]
#[test]
fn imported_executable_modes_are_preserved_and_external_changes_block_uninstall() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("");
    let script = f.source.join("run.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let installed = f
        .mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let target = f.home.join("skills/demo/run.sh");
    assert_eq!(fsutil::file_mode(&target).unwrap(), Some(0o700));
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        f.mutate(
            json!({"action":"uninstall_skill","id":installed["inventory"]["skills"][0]["id"]})
        )
        .is_err()
    );
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    f.mutate(json!({"action":"uninstall_skill","id":installed["inventory"]["skills"][0]["id"]}))
        .unwrap();
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn linked_ancestor_directories_do_not_block_managed_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    // macOS 的 /tmp、/var、/etc 都是链接，祖先目录带链接是系统常态。
    let real = root.join("real-root");
    let link = root.join("linked-root");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    fs::write(real.join("config.json"), "{}").unwrap();

    let file = link.join("config.json");
    fsutil::safe_path(&file).unwrap();
    assert_eq!(
        fsutil::read(&file).unwrap().as_deref(),
        Some(b"{}".as_slice())
    );

    // 目标自身仍然必须是普通文件，不能是链接。
    let outside = root.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    let linked_file = real.join("linked.json");
    std::os::unix::fs::symlink(&outside, &linked_file).unwrap();
    assert!(fsutil::safe_path(&linked_file).is_err());
    assert!(fsutil::read(&linked_file).is_err());
}

#[cfg(unix)]
#[test]
fn linked_content_inside_managed_skill_keeps_list_and_uninstall_available() {
    let f = Fixture::new("");
    let installed = f
        .mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let id = installed["inventory"]["skills"][0]["id"].clone();
    let directory = f.home.join("skills/demo");
    let outside = f.home.join("outside.md");
    fs::write(&outside, "external\n").unwrap();
    std::os::unix::fs::symlink(&outside, directory.join("linked.md")).unwrap();

    // 目录里一个链接不能让整页失效，也不能取消卸载入口。
    let inventory = f.list();
    let entry = inventory["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap();
    assert_eq!(entry["canEdit"], false);
    assert_eq!(entry["canToggle"], false);
    assert_eq!(entry["canRemove"], true);
    assert!(
        inventory["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("无法完整校验"))
    );

    f.mutate(json!({"action":"uninstall_skill","id":id}))
        .unwrap();
    assert!(!directory.join("SKILL.md").exists());
    assert!(outside.exists());
    assert!(f.list()["skills"].as_array().unwrap().is_empty());
}

#[test]
fn malformed_and_stale_configs_are_not_overwritten() {
    let f = Fixture::new("[mcp_servers.test]\ncommand = 'node'\n");
    let revision = f.list()["revision"].clone();
    fs::write(f.home.join("config.toml"), "[bad").unwrap();
    assert!(
        f.service
            .dispatch(
                json!({"action":"set_mcp_enabled","id":"test","enabled":false,"revision":revision})
            )
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.home.join("config.toml")).unwrap(),
        "[bad"
    );
    fs::write(
        f.home.join("config.toml"),
        "[mcp_servers.test]\ncommand='changed'\n",
    )
    .unwrap();
    assert!(
        f.service
            .dispatch(
                json!({"action":"set_mcp_enabled","id":"test","enabled":false,"revision":revision})
            )
            .is_err()
    );
}

#[test]
fn rejects_invalid_mcp_and_new_secret_placeholder() {
    let f = Fixture::new("");
    for body in [
        json!({"command":"node","url":"https://example.invalid"}),
        json!({"command":"node","args":1}),
        json!({"url":"https://example.invalid","bearer_token":"secret"}),
        json!({"command":"node","enabled":"false"}),
        json!({"command":"node","env":{"TOKEN":mcp::MASK}}),
        json!({"command":"node","profiles":{"a":{"model":"x"}}}),
    ] {
        assert!(
            f.mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
                .is_err(),
            "accepted {body}"
        );
    }
    assert_eq!(fs::read_to_string(f.home.join("config.toml")).unwrap(), "");
}

#[test]
fn removed_history_actions_are_rejected_without_side_effects() {
    let f = Fixture::new("[mcp_servers.test]\ncommand='node'\n");
    let before = fs::read(f.home.join("config.toml")).unwrap();
    for action in ["list_snapshots", "rollback"] {
        assert!(
            f.service
                .dispatch(json!({"action":action,"confirmed":true,"revision":f.list()["revision"]}))
                .unwrap_err()
                .to_string()
                .contains("不支持")
        );
    }
    assert_eq!(before, fs::read(f.home.join("config.toml")).unwrap());
    assert!(!f.data.exists());
    assert!(!f.home.join("config.toml.lock").exists());
}

#[test]
fn unchanged_updates_have_no_history_and_concurrent_writes_require_fresh_revision() {
    let f = Fixture::new("[mcp_servers.test]\ncommand='node'\nenabled=false\n");
    f.mutate(json!({"action":"set_mcp_enabled","id":"test","enabled":false}))
        .unwrap();
    let unchanged = f
        .mutate(json!({"action":"set_mcp_enabled","id":"test","enabled":false}))
        .unwrap();
    assert_eq!(unchanged["applyStatus"], "unchanged");
    assert!(unchanged.get("snapshotId").is_none());
    let revision = f.list()["revision"].clone();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = ["first", "second"].into_iter().map(|id| {
            let service = &f.service;
            let barrier = &barrier;
            let revision = &revision;
            scope.spawn(move || {
                barrier.wait();
                service.dispatch(json!({"action":"save_mcp","id":id,"configJson":{"command":"node"},"createOnly":true,"revision":revision,"confirmed":true}))
            })
        }).collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .into_iter()
            .find_map(Result::err)
            .unwrap()
            .to_string()
            .contains("配置已变化")
    );
    assert_eq!(f.list()["mcps"].as_array().unwrap().len(), 2);
    assert!(!f.data.join("snapshots").exists());
}

#[test]
fn managed_skill_install_toggle_edit_uninstall() {
    let f = Fixture::new("");
    fs::create_dir(f.source.join("scripts")).unwrap();
    fs::write(f.source.join("scripts/run.sh"), "echo hi\n").unwrap();
    let installed = f
        .mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let entry = &installed["inventory"]["skills"][0];
    let id = entry["id"].clone();
    assert_eq!(entry["ownership"], "managed");
    assert!(f.source.join("SKILL.md").exists());
    f.mutate(json!({"action":"set_skill_enabled","id":id,"enabled":false}))
        .unwrap();
    assert_eq!(f.list()["skills"][0]["enabled"], false);
    let text = "---\nname: demo\ndescription: Updated description\n---\nChanged\n";
    f.mutate(json!({"action":"save_skill","id":id,"content":text}))
        .unwrap();
    let removed = f
        .mutate(json!({"action":"uninstall_skill","id":id}))
        .unwrap();
    assert!(
        removed["inventory"]["skills"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!f.home.join("skills/demo").exists());
    assert!(removed.get("snapshotId").is_none());
    assert!(!f.data.join("snapshots").exists());
}

#[test]
fn external_skills_can_be_removed_but_modified_managed_skills_are_preserved() {
    let f = Fixture::new("");
    let manual = f.home.join("skills/manual");
    fs::create_dir_all(&manual).unwrap();
    fs::copy(f.source.join("SKILL.md"), manual.join("SKILL.md")).unwrap();
    let id = f.list()["skills"][0]["id"].clone();
    assert!(
        f.mutate(json!({"action":"save_skill","id":id,"content":"anything"}))
            .is_err()
    );
    f.mutate(json!({"action":"set_skill_enabled","id":id,"enabled":false}))
        .unwrap();
    f.mutate(json!({"action":"uninstall_skill","id":id}))
        .unwrap();
    assert!(!manual.exists());
    f.mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let id = f.list()["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["ownership"] == "managed")
        .unwrap()["id"]
        .clone();
    fs::write(f.home.join("skills/demo/extra"), "external").unwrap();
    assert!(
        f.mutate(json!({"action":"uninstall_skill","id":id}))
            .is_err()
    );
}

#[test]
fn invalid_external_skills_can_be_removed_from_each_supported_root() {
    for (project_scope, agents_root) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let f = Fixture::new("model='keep'\n");
        let project = f.source.parent().unwrap().join("project");
        fs::create_dir(&project).unwrap();
        let scope = if project_scope {
            json!({"kind":"project","projectPath":project})
        } else {
            json!({"kind":"user"})
        };
        let root = if project_scope {
            project.join(if agents_root {
                ".agents/skills"
            } else {
                ".codex/skills"
            })
        } else if agents_root {
            f.service.user_home.join(".agents/skills")
        } else {
            f.home.join("skills")
        };
        let directory = root.join("broken");
        fs::create_dir_all(directory.join("scripts")).unwrap();
        fs::write(directory.join("SKILL.md"), "---\nname: [broken\n---\n").unwrap();
        fs::write(directory.join("scripts/run.sh"), "echo example").unwrap();
        let neighbor = root.join("neighbor");
        fs::create_dir(&neighbor).unwrap();
        fs::copy(f.source.join("SKILL.md"), neighbor.join("SKILL.md")).unwrap();
        let inventory = f
            .service
            .dispatch(json!({"action":"list","scope":scope}))
            .unwrap();
        let id = skills::id(&directory.join("SKILL.md"));
        let entry = inventory["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap();
        assert_eq!(entry["configurationStatus"], "invalid");
        assert_eq!(entry["canRemove"], true);
        assert_eq!(entry["canEdit"], false);
        let mut request = json!({"action":"uninstall_skill","scope":scope,"id":id,"revision":inventory["revision"]});
        assert!(
            f.service
                .dispatch(request.clone())
                .unwrap_err()
                .to_string()
                .contains("确认")
        );
        assert!(directory.join("SKILL.md").exists());
        request["confirmed"] = json!(true);
        let removed = f.service.dispatch(request).unwrap();
        assert!(!directory.exists());
        assert!(root.exists() && neighbor.join("SKILL.md").exists());
        assert_eq!(removed["inventory"]["skills"].as_array().unwrap().len(), 1);
        assert_eq!(
            fs::read_to_string(f.home.join("config.toml")).unwrap(),
            "model='keep'\n"
        );
        assert!(!f.service.registry_path().exists());
    }
}

#[test]
fn external_skill_removal_rejects_changed_resources_and_protected_targets() {
    let f = Fixture::new("");
    let root = f.home.join("skills");
    let directory = root.join("broken");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), "broken").unwrap();
    fs::write(directory.join("asset.txt"), "original").unwrap();
    let inventory = f.list();
    let id = skills::id(&directory.join("SKILL.md"));
    fs::write(directory.join("asset.txt"), "changed").unwrap();
    assert!(f.service.dispatch(json!({"action":"uninstall_skill","id":id,"revision":inventory["revision"],"confirmed":true})).unwrap_err().to_string().contains("配置已变化"));
    assert!(directory.join("SKILL.md").exists());

    for target in [
        root.clone(),
        root.join(".system/protected"),
        directory.join("nested"),
    ] {
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("SKILL.md"), "broken").unwrap();
    }
    for target in [&root, &root.join(".system/protected"), &directory] {
        let id = skills::id(&target.join("SKILL.md"));
        let inventory = f.list();
        let entry = inventory["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap();
        assert_eq!(entry["canRemove"], false);
        assert!(
            f.mutate(json!({"action":"uninstall_skill","id":id}))
                .is_err()
        );
        assert!(target.join("SKILL.md").exists());
    }
    assert!(f.mutate(json!({"action":"uninstall_skill","id":skills::id(&f.source.join("SKILL.md")),"sourcePath":f.source})).is_err());
    assert!(f.source.join("SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn external_skill_removal_rejects_linked_content() {
    let f = Fixture::new("");
    let directory = f.home.join("skills/broken");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), "broken").unwrap();
    std::os::unix::fs::symlink(&f.source, directory.join("linked")).unwrap();
    let inventory = f.list();
    assert_eq!(inventory["skills"][0]["canRemove"], false);
    assert!(
        f.mutate(json!({"action":"uninstall_skill","id":inventory["skills"][0]["id"]}))
            .is_err()
    );
    assert!(directory.join("SKILL.md").exists());
    assert!(f.source.join("SKILL.md").exists());
}

#[test]
fn skill_identity_uses_path_and_project_scope_is_separate() {
    let f = Fixture::new("");
    for name in ["a", "b"] {
        let dir = f.home.join("skills").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::copy(f.source.join("SKILL.md"), dir.join("SKILL.md")).unwrap();
    }
    let list = f.list();
    assert_ne!(list["skills"][0]["id"], list["skills"][1]["id"]);
    let project = f.source.parent().unwrap().join("project");
    fs::create_dir(&project).unwrap();
    let scope = json!({"kind":"project","projectPath":project});
    let inv = f
        .service
        .dispatch(json!({"action":"list","scope":scope}))
        .unwrap();
    assert!(inv["skills"].as_array().unwrap().is_empty());
    assert!(f.service.dispatch(json!({"action":"set_skill_enabled","scope":scope,"id":list["skills"][0]["id"],"revision":inv["revision"],"enabled":false})).is_err());
    assert!(
        f.service
            .dispatch(
                json!({"action":"list","scope":{"kind":"project","projectPath":"../relative"}})
            )
            .is_err()
    );
}

fn archive(path: &Path, entries: &[(&str, &[u8])]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn zip_rejects_traversal_ambiguous_and_large_content() {
    let f = Fixture::new("");
    let zip = f.source.parent().unwrap().join("skill.zip");
    let content = fs::read(f.source.join("SKILL.md")).unwrap();
    archive(&zip, &[("../escape", b"bad"), ("SKILL.md", &content)]);
    assert!(skills::load_source(&zip).is_err());
    archive(&zip, &[("a/SKILL.md", &content), ("b/SKILL.md", &content)]);
    assert!(skills::load_source(&zip).is_err());
    archive(
        &zip,
        &[
            ("SKILL.md", &content),
            ("big", &vec![0; fsutil::MAX_FILE as usize + 1]),
        ],
    );
    assert!(skills::load_source(&zip).is_err());
    archive(
        &zip,
        &[
            ("wrapper/SKILL.md", &content),
            ("wrapper/resource", b"resource"),
        ],
    );
    assert_eq!(skills::load_source(&zip).unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_links_without_hiding_other_skills() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("");
    symlink(f.home.join("config.toml"), f.source.join("link")).unwrap();
    assert!(skills::load_source(&f.source).is_err());
    fs::remove_file(f.source.join("link")).unwrap();
    let root = f.home.join("skills");
    fs::create_dir_all(root.join("safe")).unwrap();
    fs::copy(f.source.join("SKILL.md"), root.join("safe/SKILL.md")).unwrap();
    symlink(&f.source, root.join("link")).unwrap();
    assert_eq!(f.list()["skills"].as_array().unwrap().len(), 1);
    assert!(f.list()["warnings"].as_array().unwrap().len() > 1);
    let actual = f.home.join("real.toml");
    fs::rename(f.home.join("config.toml"), &actual).unwrap();
    symlink(actual, f.home.join("config.toml")).unwrap();
    assert!(f.service.dispatch(json!({"action":"list"})).is_err());
}

#[test]
fn zip_symlink_is_rejected_and_yaml_is_bounded() {
    let f = Fixture::new("");
    let path = f.source.parent().unwrap().join("link.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    zip.add_symlink(
        "linked",
        "../../outside",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.finish().unwrap();
    assert!(skills::load_source(&path).is_err());
    for yaml in [
        "---\nname: &anchor demo\ndescription: *anchor\n---",
        "---\nname: demo\ndescription: !tag value\n---",
    ] {
        assert!(skills::metadata(yaml.as_bytes()).is_err());
    }
    let large = format!("---\nname: demo\ndescription: {}\n---", "a".repeat(32768));
    assert!(skills::metadata(large.as_bytes()).is_err());
}

#[test]
fn project_skill_toggle_uses_user_path_rule_and_preserves_project_config() {
    let f = Fixture::new("model='keep'\n");
    let project = f.source.parent().unwrap().join("project");
    fs::create_dir(&project).unwrap();
    let scope = json!({"kind":"project","projectPath":project});
    let inv = f
        .service
        .dispatch(json!({"action":"list","scope":scope}))
        .unwrap();
    let installed=f.service.dispatch(json!({"action":"install_skill","scope":scope,"revision":inv["revision"],"sourcePath":f.source})).unwrap();
    let inv = &installed["inventory"];
    f.service.dispatch(json!({"action":"set_skill_enabled","scope":scope,"revision":inv["revision"],"id":inv["skills"][0]["id"],"enabled":false})).unwrap();
    assert!(!project.join(".codex/config.toml").exists());
    let config = fs::read_to_string(f.home.join("config.toml")).unwrap();
    assert!(config.contains("model='keep'"));
    assert!(
        config.contains(".agents/skills/demo/SKILL.md")
            || config.contains(r".agents\skills\demo\SKILL.md")
    );
    assert!(config.contains("enabled = false"));
}

#[test]
fn name_rules_remain_unchanged_and_block_ambiguous_toggles() {
    let f = Fixture::new("");
    f.mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let config = f.home.join("config.toml");
    fs::write(
        &config,
        format!(
            "{}\n[[skills.config]]\nname='demo'\nenabled=false\n",
            fs::read_to_string(&config).unwrap()
        ),
    )
    .unwrap();
    let entry = f.list()["skills"][0].clone();
    assert_eq!(entry["readOnly"], true);
    assert_eq!(
        f.service
            .dispatch(json!({"action":"read_skill","id":entry["id"]}))
            .unwrap()["readOnly"],
        true
    );
    assert!(
        f.mutate(json!({"action":"uninstall_skill","id":entry["id"]}))
            .is_err()
    );
    assert!(
        f.mutate(json!({"action":"set_skill_enabled","id":entry["id"],"enabled":true}))
            .is_err()
    );
    assert!(
        fs::read_to_string(f.home.join("config.toml"))
            .unwrap()
            .contains("name='demo'")
    );
    let conflict = Fixture::new("[[skills.config]]\nname='demo'\nenabled=true\n");
    assert!(
        conflict
            .mutate(json!({"action":"install_skill","sourcePath":conflict.source}))
            .is_err()
    );
    assert!(!conflict.home.join("skills/demo").exists());
}

#[test]
fn recursive_credentials_and_urls_roundtrip_without_disclosure() {
    let f = Fixture::new(
        "[mcp_servers.test]\nurl='https://example.invalid/mcp?key=private-query'\nclientSecret='private-client'\ncustom = { nested = { password = 'private-password' }, urls = ['https://example.invalid?token=private-array'] }\n[mcp_servers.test.auth]\napi_key='private-key'\n",
    );
    let before = f.service.mcp_configuration(&Scope::User, "test").unwrap();
    let body = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap();
    assert!(!body.to_string().contains("private-"));
    assert!(!f.list().to_string().contains("private-"));
    f.mutate(json!({"action":"save_mcp","id":"test","configJson":body["configJson"]}))
        .unwrap();
    assert_eq!(
        before,
        f.service.mcp_configuration(&Scope::User, "test").unwrap()
    );
    let f =
        Fixture::new("[mcp_servers.test]\nurl='https://user:private-user@example.invalid/mcp'\n");
    assert!(
        !f.service
            .dispatch(json!({"action":"get_mcp","id":"test"}))
            .unwrap()
            .to_string()
            .contains("private-user")
    );
}

#[test]
fn comments_opaque_fields_and_command_credentials_are_not_exposed() {
    let f = Fixture::new(
        "[mcp_servers.test]\n# token=private-comment\ncommand='node' # private-inline\nargs=['server.js','--token','private-argument','--api-key=private-inline-argument']\ncustom={nested={opaque='private-opaque'}}\n",
    );
    let before = f.service.mcp_configuration(&Scope::User, "test").unwrap();
    let result = f
        .service
        .dispatch(json!({"action":"get_mcp","id":"test"}))
        .unwrap();
    let body = &result["configJson"];
    assert!(
        !body.to_string().contains("private-"),
        "display and export must not expose credentials or comments"
    );
    assert_eq!(body["args"][0], "server.js");
    f.mutate(json!({"action":"save_mcp","id":"test","configJson":body}))
        .unwrap();
    assert_eq!(
        before,
        f.service.mcp_configuration(&Scope::User, "test").unwrap()
    );
}

#[test]
fn new_mcp_is_enabled_and_existing_legacy_credentials_allow_toggle() {
    let f = Fixture::new(
        "[mcp_servers.legacy]\nurl='https://example.invalid'\nbearer_token='private-token'\nenabled=false\n",
    );
    let result = f
        .mutate(
            json!({"action":"save_mcp","id":"new","configJson":{"command":"demo","enabled":true}}),
        )
        .unwrap();
    assert_eq!(
        result["inventory"]["mcps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "new")
            .unwrap()["enabled"],
        true
    );
    f.mutate(json!({"action":"set_mcp_enabled","id":"legacy","enabled":true}))
        .unwrap();
    assert_eq!(
        f.service.mcp_configuration(&Scope::User, "legacy").unwrap()["enabled"],
        true
    );
}

#[test]
fn large_bodies_reject_before_parsing_or_locking() {
    let f = Fixture::new("");
    for (action, key) in [("create_skill", "content"), ("save_skill", "content")] {
        let mut request = json!({"action":action,"id":"demo","revision":f.list()["revision"]});
        request[key] = json!("x".repeat(fsutil::MAX_FILE as usize + 1));
        let error = f.service.dispatch(request).unwrap_err().to_string();
        assert!(error.contains("大小限制"));
    }
    assert!(!f.data.exists());
}

#[test]
fn create_export_import_roundtrip_preserves_resources_and_executable_flags() {
    let f = Fixture::new("");
    let content = "---\nname: created\ndescription: Created test\nmetadata:\n  version: '1.2.3'\n---\n# Skill\n";
    let created = f
        .mutate(json!({"action":"create_skill","content":content}))
        .unwrap();
    let entry = &created["inventory"]["skills"][0];
    assert_eq!(entry["enabled"], false);
    assert_eq!(entry["version"], "1.2.3");
    assert_eq!(entry["configurationStatus"], "valid");
    assert!(entry["updatedAt"].as_str().is_some());
    assert!(
        f.mutate(json!({"action":"create_skill","content":content}))
            .is_err()
    );
    let exported = f
        .service
        .dispatch(json!({"action":"export_skill","id":entry["id"]}))
        .unwrap();
    assert_eq!(exported["mediaType"], "application/zip");
    let zip = f.source.join("created.zip");
    fs::write(
        &zip,
        base64::engine::general_purpose::STANDARD
            .decode(exported["dataBase64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        skills::load_source(&zip).unwrap()[Path::new("SKILL.md")].bytes,
        content.as_bytes()
    );
    f.mutate(json!({"action":"uninstall_skill","id":entry["id"]}))
        .unwrap();
    f.mutate(json!({"action":"install_skill","sourcePath":zip}))
        .unwrap();
    fs::write(f.source.join("run.sh"), "#!/bin/sh\necho ok\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(f.source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::remove_file(f.source.join("created.zip")).unwrap();
    let bytes = skills::export(&f.source).unwrap();
    let zip = f.source.parent().unwrap().join("complete.zip");
    fs::write(&zip, bytes).unwrap();
    let files = skills::load_source(&zip).unwrap();
    assert!(files.contains_key(Path::new("run.sh")));
    #[cfg(unix)]
    assert!(files[Path::new("run.sh")].executable);
}

#[test]
fn batches_are_atomic_deduplicated_and_require_confirmation() {
    let f = Fixture::new(
        "[mcp_servers.first]\ncommand='demo'\nenabled=false\n[mcp_servers.second]\ncommand='demo'\nenabled=false\n",
    );
    let original = fs::read(f.home.join("config.toml")).unwrap();
    assert!(
        f.mutate(json!({"action":"set_mcps_enabled","ids":["first","missing"],"enabled":true}))
            .is_err()
    );
    assert_eq!(original, fs::read(f.home.join("config.toml")).unwrap());
    assert!(
        f.mutate(
            json!({"action":"set_mcps_enabled","ids":["first"],"enabled":true,"confirmed":false})
        )
        .is_err()
    );
    let result = f
        .mutate(
            json!({"action":"set_mcps_enabled","ids":["first","first","second"],"enabled":true}),
        )
        .unwrap();
    assert!(
        result["inventory"]["mcps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["enabled"] == true)
    );
    assert!(result.get("snapshotId").is_none());
    assert!(!f.data.join("snapshots").exists());
    for request in [
        json!({"action":"remove_mcp","id":"first"}),
        json!({"action":"uninstall_skill","id":"missing"}),
    ] {
        let mut request = request;
        request["revision"] = f.list()["revision"].clone();
        assert!(
            f.service
                .dispatch(request)
                .unwrap_err()
                .to_string()
                .contains("确认")
        );
    }
}

#[test]
fn invalid_and_readonly_skills_block_enabling_and_batch_partial_writes() {
    let f = Fixture::new("");
    f.mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let id = f.list()["skills"][0]["id"].clone();
    fs::write(f.home.join("skills/demo/SKILL.md"), "broken").unwrap();
    assert_eq!(f.list()["skills"][0]["configurationStatus"], "invalid");
    assert!(
        f.mutate(json!({"action":"set_skill_enabled","id":id,"enabled":true}))
            .is_err()
    );
    f.mutate(json!({"action":"set_skill_enabled","id":id,"enabled":false}))
        .unwrap();
    let system = f.home.join("skills/.system/system");
    fs::create_dir_all(&system).unwrap();
    fs::copy(f.source.join("SKILL.md"), system.join("SKILL.md")).unwrap();
    let system_id = skills::id(&system.join("SKILL.md"));
    let before = fs::read(f.home.join("config.toml")).unwrap();
    assert!(
        f.mutate(json!({"action":"set_skills_enabled","ids":[id,system_id],"enabled":false}))
            .is_err()
    );
    assert_eq!(before, fs::read(f.home.join("config.toml")).unwrap());
    assert!(
        f.mutate(json!({"action":"uninstall_skill","id":id}))
            .is_err()
    );
    let revision = f.list()["revision"].clone();
    fs::write(f.home.join("skills/demo/SKILL.md"), "changed again").unwrap();
    let content = fs::read_to_string(f.source.join("SKILL.md")).unwrap();
    assert!(
        f.service
            .dispatch(json!({"action":"save_skill","id":id,"revision":revision,"content":content}))
            .is_err()
    );
    f.mutate(json!({"action":"save_skill","id":id,"content":content}))
        .unwrap();
    assert_eq!(
        f.list()["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()["configurationStatus"],
        "valid"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let manifest = f.home.join("skills/demo/SKILL.md");
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            f.mutate(json!({"action":"save_skill","id":id,"content":content}))
                .is_err()
        );
    }
}

#[test]
fn dependency_declarations_are_explicit_and_invalid_declarations_remain_visible() {
    let f = Fixture::new("");
    fs::create_dir(f.source.join("agents")).unwrap();
    fs::write(
        f.source.join("agents/openai.yaml"),
        "dependencies:\n  tools:\n    - type: mcp\n      value: server-one\n",
    )
    .unwrap();
    f.mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let first = f.list();
    assert_eq!(
        first["skills"][0]["dependencies"],
        json!([{"type":"mcp","name":"server-one"}])
    );
    fs::write(
        f.home.join("skills/demo/agents/openai.yaml"),
        "dependencies: [invalid",
    )
    .unwrap();
    let invalid = f.list();
    assert_eq!(invalid["skills"].as_array().unwrap().len(), 1);
    assert!(
        !invalid["skills"][0]["dependencyWarnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_ne!(first["revision"], invalid["revision"]);
    let id = invalid["skills"][0]["id"].clone();
    assert!(f.mutate(json!({"action":"save_skill","id":id,"content":fs::read_to_string(f.source.join("SKILL.md")).unwrap()})).is_err());
}

#[test]
fn skill_names_stay_unique_across_scope_roots_and_renames() {
    let f = Fixture::new("");
    // 目录名与目标目录不同、但 name 相同的既有 Skill 同样算冲突。
    let external = f.home.parent().unwrap().join(".agents/skills/other-dir");
    fs::create_dir_all(&external).unwrap();
    fs::write(
        external.join("SKILL.md"),
        "---\nname: demo\ndescription: external copy\n---\n# External\n",
    )
    .unwrap();
    for request in [
        json!({"action":"install_skill","sourcePath":f.source}),
        json!({"action":"create_skill","content":"---\nname: demo\ndescription: created copy\n---\n"}),
    ] {
        assert!(
            f.mutate(request)
                .unwrap_err()
                .to_string()
                .contains("已存在名为 demo")
        );
    }
    f.mutate(json!({"action":"create_skill","content":"---\nname: alpha\ndescription: a\n---\n"}))
        .unwrap();
    f.mutate(json!({"action":"create_skill","content":"---\nname: beta\ndescription: b\n---\n"}))
        .unwrap();
    let id = f.list()["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "beta")
        .unwrap()["id"]
        .clone();
    // 改名撞上同范围其他 Skill 要拒绝；名称不变时保存照常。
    assert!(
        f.mutate(
            json!({"action":"save_skill","id":id,"content":"---\nname: alpha\ndescription: b\n---\n"})
        )
        .unwrap_err()
        .to_string()
        .contains("已存在名为 alpha")
    );
    f.mutate(
        json!({"action":"save_skill","id":id,"content":"---\nname: beta\ndescription: b2\n---\n"}),
    )
    .unwrap();
    let listed = f.list();
    let skills = listed["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 3);
    assert_eq!(
        skills
            .iter()
            .filter(|entry| entry["name"] == "beta")
            .count(),
        1
    );
}

#[test]
fn builtin_skills_do_not_block_installing_a_user_skill() {
    let f = Fixture::new("");
    let builtin = f.home.join("skills/.system/system");
    fs::create_dir_all(&builtin).unwrap();
    fs::write(
        builtin.join("SKILL.md"),
        "---\nname: demo\ndescription: builtin copy\n---\n# Builtin\n",
    )
    .unwrap();
    // 系统内置资源用户无法改名或卸载，不能因此挡住自己的 Skill。
    f.mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let listed = f.list();
    let ownership: Vec<_> = listed["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap().to_owned(),
                entry["ownership"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(ownership.contains(&("demo".to_owned(), "builtin".to_owned())));
    assert!(ownership.contains(&("demo".to_owned(), "managed".to_owned())));
}

#[test]
fn external_skills_report_their_directory_instead_of_a_scope_token() {
    let f = Fixture::new("");
    let external = f.home.parent().unwrap().join(".agents/skills/demo");
    fs::create_dir_all(&external).unwrap();
    fs::write(
        external.join("SKILL.md"),
        "---\nname: demo\ndescription: external copy\n---\n# External\n",
    )
    .unwrap();
    let listed = f.list();
    let entry = listed["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["ownership"] == "external")
        .unwrap()
        .clone();
    let expected = external.to_string_lossy().into_owned();
    // 外部安装没有托管记录：来源留空，由界面回退显示实际目录路径。
    assert_eq!(entry["origin"].as_str(), Some(""));
    assert_eq!(entry["sourcePath"].as_str(), Some(expected.as_str()));
}

#[test]
fn skill_install_preview_is_read_only_bounded_and_matches_installed_content() {
    let f = Fixture::new("# preserve\n");
    for index in 0..105 {
        fs::write(f.source.join(format!("asset-{index:03}.txt")), "resource").unwrap();
    }
    let before = fs::read(f.home.join("config.toml")).unwrap();
    let preview = f
        .service
        .dispatch(json!({
            "action":"preview_skill_install","sourcePath":f.source
        }))
        .unwrap();
    assert_eq!(preview["name"], "demo");
    assert_eq!(preview["description"], "A test skill");
    assert_eq!(preview["sourcePath"], json!(f.source));
    assert_eq!(preview["sourceKind"], "directory");
    assert_eq!(preview["fileCount"], 106);
    assert_eq!(preview["files"].as_array().unwrap().len(), 100);
    assert_eq!(preview["files"][0]["path"], "SKILL.md");
    let source = skills::load_source(&f.source).unwrap();
    assert_eq!(preview["sourceRevision"], skills::source_digest(&source));
    assert_eq!(
        preview["totalBytes"],
        source.values().map(|file| file.bytes.len()).sum::<usize>()
    );
    assert!(!f.data.exists());
    assert!(!f.home.join("config.toml.lock").exists());
    assert!(!f.home.join("skills").exists());
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), before);

    let installed = f.mutate(json!({
        "action":"install_skill","sourcePath":f.source,"sourceRevision":preview["sourceRevision"]
    })).unwrap();
    assert_eq!(installed["inventory"]["skills"][0]["enabled"], false);
    assert_eq!(
        fs::read(f.home.join("skills/demo/asset-104.txt")).unwrap(),
        b"resource"
    );
}

#[test]
fn previewed_skill_rejects_changed_sources_and_invalid_revisions_without_writes() {
    for key in ["sourceRevision", "expectedSourceDigest"] {
        let f = Fixture::new("# unchanged\n");
        let preview = f
            .service
            .dispatch(json!({
                "action":"preview_skill_install","sourcePath":f.source
            }))
            .unwrap();
        fs::write(f.source.join("new-resource.txt"), "new content").unwrap();
        let before = fs::read(f.home.join("config.toml")).unwrap();
        let mut request = json!({"action":"install_skill","sourcePath":f.source});
        request[key] = preview["sourceRevision"].clone();
        assert!(
            f.mutate(request.clone())
                .unwrap_err()
                .to_string()
                .contains("重新预览")
        );
        request[key] = json!(123);
        assert!(
            f.mutate(request.clone())
                .unwrap_err()
                .to_string()
                .contains("必须为字符串")
        );
        assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), before);
        assert!(!f.home.join("skills/demo").exists());
        assert!(!f.service.registry_path().exists());
        assert!(!f.data.join("audit.jsonl").exists());
        let latest = f
            .service
            .dispatch(json!({
                "action":"preview_skill_install","sourcePath":f.source
            }))
            .unwrap();
        request[key] = latest["sourceRevision"].clone();
        f.mutate(request).unwrap();
        assert!(f.home.join("skills/demo/new-resource.txt").exists());
    }
}

#[cfg(unix)]
#[test]
fn skill_preview_revision_covers_executable_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("");
    let script = f.source.join("run.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o600)).unwrap();
    let first = f
        .service
        .dispatch(json!({
            "action":"preview_skill_install","sourcePath":f.source
        }))
        .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let latest = f
        .service
        .dispatch(json!({
            "action":"preview_skill_install","sourcePath":f.source
        }))
        .unwrap();
    assert_ne!(first["sourceRevision"], latest["sourceRevision"]);
    assert_eq!(latest["files"][1]["executable"], true);
    assert!(
        f.mutate(json!({
            "action":"install_skill","sourcePath":f.source,"sourceRevision":first["sourceRevision"]
        }))
        .is_err()
    );
    assert!(!f.home.join("skills/demo").exists());
}

#[test]
fn skill_preview_uses_zip_import_safety_checks() {
    let f = Fixture::new("");
    let path = f.source.parent().unwrap().join("skill.zip");
    let manifest = fs::read(f.source.join("SKILL.md")).unwrap();
    archive(
        &path,
        &[("wrapper/SKILL.md", &manifest), ("wrapper/asset", b"asset")],
    );
    let preview = f
        .service
        .dispatch(json!({
            "action":"preview_skill_install","sourcePath":path
        }))
        .unwrap();
    assert_eq!(preview["sourceKind"], "zip");
    assert_eq!(preview["files"][0]["path"], "SKILL.md");
    assert_eq!(preview["fileCount"], 2);
    archive(&path, &[("../escape", b"secret"), ("SKILL.md", &manifest)]);
    let error = f
        .service
        .dispatch(json!({
            "action":"preview_skill_install","sourcePath":path
        }))
        .unwrap_err()
        .to_string();
    assert_eq!(error, skills::load_source(&path).err().unwrap().to_string());
    assert!(!error.contains("secret"));
    assert!(!f.data.exists());
    assert!(!f.home.join("skills").exists());
}

#[test]
fn skill_rename_preserves_path_identity_and_rejects_name_rule_conflicts() {
    let f = Fixture::new("[[skills.config]]\nname='blocked'\nenabled=false\n");
    let installed = f
        .mutate(json!({"action":"install_skill","sourcePath":f.source}))
        .unwrap();
    let id = installed["inventory"]["skills"][0]["id"].clone();
    let config = fs::read(f.home.join("config.toml")).unwrap();
    let registry = fs::read(f.service.registry_path()).unwrap();
    let manifest = f.home.join("skills/demo/SKILL.md");
    let original = fs::read(&manifest).unwrap();
    assert!(
        f.mutate(json!({
            "action":"save_skill","id":id,
            "content":"---\nname: blocked\ndescription: test\n---\n"
        }))
        .unwrap_err()
        .to_string()
        .contains("同名 Skill 规则")
    );
    assert_eq!(fs::read(&manifest).unwrap(), original);
    assert_eq!(fs::read(f.service.registry_path()).unwrap(), registry);
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), config);

    let renamed = f
        .mutate(json!({
            "action":"save_skill","id":id,
            "content":"---\nname: renamed\ndescription: updated\n---\n"
        }))
        .unwrap();
    let entry = &renamed["inventory"]["skills"][0];
    assert_eq!(entry["name"], "renamed");
    assert_eq!(entry["id"], id);
    assert_eq!(entry["manifestPath"], json!(manifest));
    assert_eq!(entry["enabled"], false);
    assert_eq!(entry["enabledKnown"], true);
    assert_eq!(entry["canEdit"], true);
    assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), config);
    f.mutate(json!({"action":"set_skill_enabled","id":id,"enabled":true}))
        .unwrap();
    assert_eq!(f.list()["skills"][0]["enabled"], true);
}

#[test]
fn inventory_budget_rejects_partial_revisions_and_preserves_mutation_targets() {
    for plugin_cache in [false, true] {
        let f = Fixture::new("[mcp_servers.test]\ncommand='node'\nenabled=false\n");
        let root = if plugin_cache {
            f.home.join("plugins/cache/vendor/plugin/1/skills")
        } else {
            f.home.join("skills")
        };
        let action = if plugin_cache {
            "list_skill_cache"
        } else {
            "list"
        };
        let before = f.service.dispatch(json!({"action":action})).unwrap();
        let resource = vec![b'x'; fsutil::MAX_FILE as usize];
        for skill in 0..3 {
            let directory = root.join(format!("skill-{skill}"));
            fs::create_dir_all(&directory).unwrap();
            fs::copy(f.source.join("SKILL.md"), directory.join("SKILL.md")).unwrap();
            for file in 0..6 {
                fs::write(directory.join(format!("asset-{file}")), &resource).unwrap();
            }
        }
        let error = f.service.dispatch(json!({"action":action})).unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        let mutation = if plugin_cache {
            json!({"action":"remove_skill_cache","id":skills::id(&root.join("skill-0/SKILL.md")),"confirmed":true,"revision":before["revision"]})
        } else {
            json!({"action":"set_mcp_enabled","id":"test","enabled":true,"confirmed":true,"revision":before["revision"]})
        };
        let original = fs::read(f.home.join("config.toml")).unwrap();
        assert!(
            f.service
                .dispatch(mutation)
                .unwrap_err()
                .to_string()
                .contains("64 MiB")
        );
        assert_eq!(fs::read(f.home.join("config.toml")).unwrap(), original);
        assert!(root.join("skill-0/SKILL.md").exists());
        assert!(!f.data.join("audit.jsonl").exists());
    }
}

#[test]
fn inventory_read_cache_does_not_charge_repeated_reads() {
    let f = Fixture::new("");
    let file = f.source.join("resource");
    fs::write(&file, vec![b'x'; fsutil::MAX_FILE as usize]).unwrap();
    let mut cache = fsutil::ReadCache::default();
    for _ in 0..(fsutil::MAX_INVENTORY_BYTES / fsutil::MAX_FILE + 1) {
        assert_eq!(
            cache.read(&file).unwrap().unwrap().len(),
            fsutil::MAX_FILE as usize
        );
    }
    cache.ensure_complete().unwrap();
}
