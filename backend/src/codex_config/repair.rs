use super::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigRepairReport {
    pub message: String,
    pub config_path: PathBuf,
    pub repaired: bool,
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct ConfigRepairFailure {
    pub stage: &'static str,
    pub path: PathBuf,
    pub message: String,
}

impl ConfigRepairFailure {
    pub(crate) fn new(stage: &'static str, path: &Path, message: impl Into<String>) -> Self {
        Self {
            stage,
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    pub(crate) fn description(&self) -> String {
        format!(
            "修复 Codex 配置失败（{}）：{}；路径：{}",
            self.stage,
            self.message,
            self.path.display()
        )
    }
}

type RepairResult<T> = std::result::Result<T, ConfigRepairFailure>;

// TOML errors may contain source lines, including credentials. Only filesystem
// causes are safe to include; never format arbitrary parser error chains here.
fn safe_error(
    stage: &'static str,
    path: &Path,
    action: &str,
    error: anyhow::Error,
) -> ConfigRepairFailure {
    let detail = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .map(|error| format!("{action}：{error}"))
        .unwrap_or_else(|| format!("{action}；请检查配置内容或重试，原有用户配置未被清空"));
    ConfigRepairFailure::new(stage, path, detail)
}

pub(crate) fn repair_codex_config(runtime_active: bool) -> RepairResult<ConfigRepairReport> {
    repair_at(codex_home(), &lease_marker_path(), runtime_active)
}

fn repair_at(home: &Path, marker: &Path, runtime_active: bool) -> RepairResult<ConfigRepairReport> {
    let config_path = home.join("config.toml");
    if !home.is_absolute() {
        return Err(ConfigRepairFailure::new(
            "检查路径",
            &config_path,
            "CODEX_HOME 使用相对路径，无法确认 Codex 工作目录；请将 CODEX_HOME 设置为绝对路径后重启 Codey",
        ));
    }
    let _lock = RuntimeConfigLock::acquire(marker)
        .map_err(|error| safe_error("获取运行时锁", &config_path, "无法获取运行时配置锁", error))?;
    check_config_file(&config_path)?;
    let original = read_optional(&config_path)
        .map_err(|error| safe_error("读取配置", &config_path, "无法读取配置文件", error))?;
    let mut document = parse_safe(original.as_deref().unwrap_or_default(), &config_path)?;

    let lease = read_optional(marker)
        .map_err(|error| safe_error("检查运行时", marker, "无法读取运行时租约", error))?
        .map(|bytes| {
            serde_json::from_slice::<RuntimeConfigLease>(&bytes).map_err(|_| {
                ConfigRepairFailure::new(
                    "检查运行时",
                    marker,
                    "Codey 运行时租约格式损坏，无法安全重建运行时文件，请重启 Codey",
                )
            })
        })
        .transpose()?;
    if let Some(lease) = &lease {
        let lease_home = runtime_home_for_lease(lease);
        if lease_home != home {
            return Err(ConfigRepairFailure::new(
                "检查运行时",
                &lease_home,
                "运行时租约与当前 Codex 配置目录不同，已停止修复，请重启 Codey",
            ));
        }
    }

    // Resolve only the exact Codey-owned catalog name. A custom relative path
    // depends on Codex's working directory and must not be guessed or rewritten.
    let mut corrected_catalog = false;
    if document.get("model_catalog_json").and_then(Item::as_str)
        == Some(crate::model_catalog::relative_path())
    {
        let catalog = home.join(crate::model_catalog::relative_path());
        check_reference(&catalog, "model_catalog_json", ReferenceKind::Json)?;
        // 就地替换值以保留该行原有的注释与格式；路径含非 UTF-8 字节时无法安全
        // 写回 TOML，此时跳过改写，不能写进一个不可用的路径。
        let path_text = catalog.to_string_lossy();
        if !path_text.contains(char::REPLACEMENT_CHARACTER) {
            if let Some(existing) = document
                .get_mut("model_catalog_json")
                .and_then(Item::as_value_mut)
            {
                let decor = existing.decor().clone();
                let mut replacement = toml_edit::Value::from(path_text.into_owned());
                *replacement.decor_mut() = decor;
                *existing = replacement;
            }
            corrected_catalog = true;
        }
    }

    // Preflight user references before any repair. Generated agent documents
    // covered by the active lease are validated again after reconciliation.
    let repair_roles = runtime_active
        && lease
            .as_ref()
            .is_some_and(|lease| lease.subagent_optimization_applied);
    let owned_agents = lease
        .as_ref()
        .filter(|_| repair_roles)
        .map(|lease| {
            lease
                .subagent_roles
                .keys()
                .map(|role| runtime_agent_path(&marker.with_file_name(CODEY_CONSTRAINTS_DIR), role))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    check_document_references(&document, home, &owned_agents)?;

    fs::create_dir_all(home).map_err(|error| {
        ConfigRepairFailure::new("检查写入权限", home, format!("无法创建配置目录：{error}"))
    })?;
    let probe = tempfile::NamedTempFile::new_in(home).map_err(|error| {
        ConfigRepairFailure::new(
            "检查写入权限",
            home,
            format!("无法在配置目录创建临时文件：{error}"),
        )
    })?;
    probe.close().map_err(|error| {
        ConfigRepairFailure::new(
            "检查写入权限",
            home,
            format!("无法清理配置目录的写入检查文件：{error}"),
        )
    })?;
    if original.is_some() {
        OpenOptions::new()
            .write(true)
            .open(&config_path)
            .map_err(|error| {
                ConfigRepairFailure::new(
                    "检查写入权限",
                    &config_path,
                    format!("配置文件不可写：{error}"),
                )
            })?;
    }

    let manager = ConfigManager::new(&config_path);
    let snapshot = manager
        .load()
        .map_err(|error| safe_error("读取配置", &config_path, "无法锁定或读取配置", error))?;
    if snapshot.exists() != original.is_some()
        || snapshot.raw() != original.as_deref().unwrap_or_default()
    {
        return Err(ConfigRepairFailure::new(
            "检查并发修改",
            &config_path,
            "配置已被其他程序修改，请重新点击修复",
        ));
    }
    let config_changed = original.is_none() || corrected_catalog;
    let mut backup_path = None;
    if config_changed {
        manager
            .replace_document(
                Some(snapshot.revision()),
                document,
                "repair missing Codex config or Codey-owned catalog path",
                "repair_codex_config",
            )
            .map_err(|error| {
                safe_error(
                    "备份并写入配置",
                    &config_path,
                    "配置备份或原子写入失败",
                    error,
                )
            })?;
        if original.is_some() {
            backup_path = Some(config_path.with_file_name("config.toml.bak"));
        }
    }

    let mut roles_repaired = false;
    if repair_roles {
        let lease = lease.as_ref().expect("active role lease");
        // Saved settings can have pending edits. Preserve the settings currently
        // attested by this lease rather than applying the next launch's settings.
        let applied_roles = SUBAGENT_ROLE_IDS
            .into_iter()
            .map(|role| {
                let selection = lease.subagent_roles.get(role).cloned().unwrap_or_else(|| {
                    let mut disabled = SubagentRoleConfig::new(
                        &lease.subagent_model,
                        &lease.subagent_reasoning_effort,
                    );
                    disabled.enabled = false;
                    disabled
                });
                (role.to_string(), selection)
            })
            .collect();
        let applied = CodeyConfig {
            subagent_optimization: true,
            subagent_model: lease.subagent_model.clone(),
            subagent_reasoning_effort: lease.subagent_reasoning_effort.clone(),
            subagent_roles: applied_roles,
            ..CodeyConfig::default()
        };
        roles_repaired = reconcile_runtime_subagent_roles_with_compaction(
            &applied,
            marker,
            &lease.remote_compaction_models,
        )
        .map_err(|error| {
            safe_error(
                "修复运行时资源",
                marker,
                "Codey 子代理生成文件修复失败，请检查约束模板并重启 Codey",
                error,
            )
        })?
        .repaired;
        if reconcile_runtime_subagent_roles_with_compaction(
            &applied,
            marker,
            &lease.remote_compaction_models,
        )
        .map_err(|error| safe_error("验证运行时资源", marker, "运行时资源验证失败", error))?
        .repaired
        {
            return Err(ConfigRepairFailure::new(
                "验证运行时资源",
                marker,
                "运行时文件仍在变化，请重启 Codey 后重试",
            ));
        }
    }

    check_config_file(&config_path)?;
    let verified = manager
        .reload()
        .map_err(|error| safe_error("读回验证", &config_path, "配置读回验证失败", error))?;
    if !verified.exists() {
        return Err(ConfigRepairFailure::new(
            "读回验证",
            &config_path,
            "配置文件仍然缺失",
        ));
    }
    check_document_references(verified.document(), home, &[])?;
    let repaired = config_changed || roles_repaired;
    let mut actions = Vec::new();
    if original.is_none() {
        actions.push("已补建缺失的 config.toml");
    }
    if corrected_catalog {
        actions.push("已备份配置并修正 Codey 模型目录路径");
    }
    if roles_repaired {
        actions.push("已修复 Codey 子代理运行时文件");
    }
    if actions.is_empty() {
        actions.push("未发现可自动修复的问题");
    }
    Ok(ConfigRepairReport {
        message: format!(
            "{}。已检查 Codey 所用配置的读写、TOML 语法及已识别的文件引用。另行启动的 Codex 可能使用其他配置目录；若仍报错，请核对报错中的路径。",
            actions.join("；")
        ),
        config_path,
        repaired,
        backup_path,
    })
}

fn check_config_file(path: &Path) -> RepairResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ConfigRepairFailure::new(
            "检查路径",
            path,
            "config.toml 是符号链接，自动替换可能改变链接目标，请先手动检查链接",
        )),
        Ok(metadata) if !metadata.is_file() => Err(ConfigRepairFailure::new(
            "检查路径",
            path,
            "config.toml 路径不是普通文件",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ConfigRepairFailure::new(
            "检查路径",
            path,
            format!("无法检查配置路径：{error}"),
        )),
    }
}

fn parse_safe(bytes: &[u8], path: &Path) -> RepairResult<DocumentMut> {
    let text = str::from_utf8(bytes).map_err(|_| {
        ConfigRepairFailure::new(
            "解析配置",
            path,
            "配置不是 UTF-8 文本，已保留原文件，请手动修正编码",
        )
    })?;
    text.parse::<DocumentMut>().map_err(|error| {
        let location = error
            .span()
            .map(|span| {
                format!(
                    "（第 {} 行附近）",
                    bytes[..span.start.min(bytes.len())]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count()
                        + 1
                )
            })
            .unwrap_or_default();
        ConfigRepairFailure::new(
            "解析配置",
            path,
            format!("TOML 语法无效{location}，已保留原文件，请手动修正；不会使用空配置覆盖"),
        )
    })
}

#[derive(Clone, Copy)]
enum ReferenceKind {
    Text,
    Toml,
    Json,
}

fn check_document_references(
    document: &DocumentMut,
    home: &Path,
    skip: &[PathBuf],
) -> RepairResult<()> {
    check_document_references_at(document, home, skip, 0)
}

fn check_document_references_at(
    document: &DocumentMut,
    home: &Path,
    skip: &[PathBuf],
    depth: usize,
) -> RepairResult<()> {
    if depth > 8 {
        return Err(ConfigRepairFailure::new(
            "检查文件引用",
            home,
            "子代理配置文件引用层数过多或存在循环，请手动检查引用关系",
        ));
    }
    check_table_references(document.as_table(), home, skip, depth)?;
    if let Some(profiles) = document.get("profiles").and_then(Item::as_table_like) {
        for (_, profile) in profiles.iter() {
            if let Some(profile) = profile.as_table_like() {
                check_table_references(profile, home, skip, depth)?;
            }
        }
    }
    Ok(())
}

fn check_table_references(
    table: &dyn TableLike,
    home: &Path,
    skip: &[PathBuf],
    depth: usize,
) -> RepairResult<()> {
    for (key, kind) in [
        ("model_catalog_json", ReferenceKind::Json),
        ("model_instructions_file", ReferenceKind::Text),
        ("experimental_instructions_file", ReferenceKind::Text),
    ] {
        if let Some(item) = table.get(key) {
            check_path_item(item, home, key, kind, skip, depth)?;
        }
    }
    if let Some(agents) = table.get("agents").and_then(Item::as_table_like) {
        for (_, agent) in agents.iter() {
            if let Some(item) = agent
                .as_table_like()
                .and_then(|agent| agent.get("config_file"))
            {
                check_path_item(
                    item,
                    home,
                    "agents.*.config_file",
                    ReferenceKind::Toml,
                    skip,
                    depth,
                )?;
            }
        }
    }
    // MCP commands may be PATH commands, provider fields may be URLs. Neither
    // is a config-file reference, so do not turn them into filesystem paths.
    Ok(())
}

fn check_path_item(
    item: &Item,
    home: &Path,
    key: &str,
    kind: ReferenceKind,
    skip: &[PathBuf],
    depth: usize,
) -> RepairResult<()> {
    let text = item
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            ConfigRepairFailure::new(
                "检查文件引用",
                &home.join("config.toml"),
                format!("{key} 必须是非空文件路径"),
            )
        })?;
    let path = Path::new(text);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        home.join(path)
    };
    if skip.contains(&resolved) {
        return Ok(());
    }
    check_reference_at(&resolved, key, kind, skip, depth)
}

fn check_reference(path: &Path, key: &str, kind: ReferenceKind) -> RepairResult<()> {
    check_reference_at(path, key, kind, &[], 0)
}

fn check_reference_at(
    path: &Path,
    key: &str,
    kind: ReferenceKind,
    skip: &[PathBuf],
    depth: usize,
) -> RepairResult<()> {
    let bytes = fs::read(path).map_err(|error| {
        ConfigRepairFailure::new(
            "检查文件引用",
            path,
            format!("{key} 引用的文件不可读取：{error}；请恢复该文件或修正配置中的路径"),
        )
    })?;
    match kind {
        ReferenceKind::Toml => {
            let document = parse_safe(&bytes, path)?;
            check_document_references_at(
                &document,
                path.parent().unwrap_or(path),
                skip,
                depth + 1,
            )?;
        }
        ReferenceKind::Json => {
            serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| {
                ConfigRepairFailure::new(
                    "检查文件引用",
                    path,
                    format!("{key} 引用的文件不是有效 JSON"),
                )
            })?;
        }
        ReferenceKind::Text => {
            str::from_utf8(&bytes).map_err(|_| {
                ConfigRepairFailure::new(
                    "检查文件引用",
                    path,
                    format!("{key} 引用的文件不是 UTF-8 文本"),
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> (PathBuf, PathBuf) {
        (root.join("codex"), root.join("codey/codex-lease.json"))
    }

    #[test]
    fn config_repair_creates_missing_home_and_file() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        let report = repair_at(&home, &marker, false).unwrap();
        assert!(report.repaired);
        assert!(report.backup_path.is_none());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), b"");
        assert!(!repair_at(&home, &marker, false).unwrap().repaired);
    }

    #[test]
    fn config_repair_preserves_user_content_and_path_commands() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        let content = "# Keep my comments\nmodel = 'custom-model'\n[mcp_servers.user]\ncommand = 'npx'\nargs = ['-y', 'custom-server']\n[model_providers.custom]\nbase_url = 'https://example.test/v1'\n";
        fs::write(home.join("config.toml"), content).unwrap();
        assert!(!repair_at(&home, &marker, false).unwrap().repaired);
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            content
        );
        assert!(!home.join("config.toml.bak").exists());
    }

    #[test]
    fn config_repair_backs_up_owned_catalog_path_correction() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(home.join("model-catalogs")).unwrap();
        fs::write(
            home.join(crate::model_catalog::relative_path()),
            r#"{"models":[]}"#,
        )
        .unwrap();
        let content = "# user comment\nmodel='mine'\nmodel_catalog_json='model-catalogs/codey-official.json'\n";
        fs::write(home.join("config.toml"), content).unwrap();
        let report = repair_at(&home, &marker, false).unwrap();
        assert!(report.repaired);
        assert_eq!(
            fs::read_to_string(report.backup_path.unwrap()).unwrap(),
            content
        );
        let repaired = fs::read_to_string(home.join("config.toml")).unwrap();
        let document = repaired.parse::<DocumentMut>().unwrap();
        assert_eq!(document["model"].as_str(), Some("mine"));
        assert_eq!(
            document["model_catalog_json"].as_str(),
            home.join(crate::model_catalog::relative_path()).to_str()
        );
        assert!(repaired.contains("# user comment"));
    }

    #[test]
    fn config_repair_rejects_invalid_toml_without_disclosing_source() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        let content = "token = 'private-secret' broken\n";
        fs::write(home.join("config.toml"), content).unwrap();
        let failure = repair_at(&home, &marker, false).unwrap_err();
        assert_eq!(failure.stage, "解析配置");
        assert!(!failure.description().contains("private-secret"));
        assert!(failure.message.contains("第 1 行"));
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            content
        );
    }

    #[test]
    fn config_repair_reports_missing_user_references_without_rewriting() {
        for key in [
            "model_catalog_json",
            "model_instructions_file",
            "experimental_instructions_file",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (home, marker) = paths(temp.path());
            fs::create_dir_all(&home).unwrap();
            let content = format!("{key}='missing-file'\n");
            fs::write(home.join("config.toml"), &content).unwrap();
            let failure = repair_at(&home, &marker, false).unwrap_err();
            assert_eq!(failure.path, home.join("missing-file"));
            assert!(failure.message.contains(key));
            assert_eq!(
                fs::read_to_string(home.join("config.toml")).unwrap(),
                content
            );
        }
    }

    #[test]
    fn config_repair_checks_profile_agent_references() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[profiles.work.agents.reader]\nconfig_file='reader.toml'\n",
        )
        .unwrap();
        let failure = repair_at(&home, &marker, false).unwrap_err();
        assert_eq!(failure.path, home.join("reader.toml"));
        assert!(failure.message.contains("agents.*.config_file"));
    }

    #[test]
    fn config_repair_checks_nested_agent_instruction_references() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(home.join("agents")).unwrap();
        fs::write(
            home.join("config.toml"),
            "[agents.reader]\nconfig_file='agents/reader.toml'\n",
        )
        .unwrap();
        fs::write(
            home.join("agents/reader.toml"),
            "model_instructions_file='instructions.md'\n",
        )
        .unwrap();
        let failure = repair_at(&home, &marker, false).unwrap_err();
        assert_eq!(failure.path, home.join("agents/instructions.md"));
        fs::write(home.join("agents/instructions.md"), "User instructions").unwrap();
        assert!(!repair_at(&home, &marker, false).unwrap().repaired);
    }

    #[cfg(unix)]
    #[test]
    fn config_repair_reports_read_only_config() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        let config = home.join("config.toml");
        fs::write(&config, "model='mine'\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o400)).unwrap();
        let result = repair_at(&home, &marker, false);
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(result.unwrap_err().stage, "检查写入权限");
        assert_eq!(fs::read_to_string(config).unwrap(), "model='mine'\n");
    }

    #[test]
    fn config_repair_rejects_directory_and_relative_home() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(home.join("config.toml")).unwrap();
        assert_eq!(
            repair_at(&home, &marker, false).unwrap_err().stage,
            "检查路径"
        );
        assert_eq!(
            repair_at(Path::new("relative-home"), &marker, false)
                .unwrap_err()
                .stage,
            "检查路径"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_repair_does_not_replace_dangling_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        let target = home.join("missing.toml");
        std::os::unix::fs::symlink(&target, home.join("config.toml")).unwrap();
        assert_eq!(
            repair_at(&home, &marker, false).unwrap_err().stage,
            "检查路径"
        );
        assert_eq!(fs::read_link(home.join("config.toml")).unwrap(), target);
        assert!(!target.exists());
    }

    #[test]
    fn config_repair_keeps_unrelated_runtime_lease() {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let lease = serde_json::json!({"backupDir":temp.path().join("backup"), "runtimeHome":temp.path().join("another-home")});
        let content = serde_json::to_vec(&lease).unwrap();
        fs::write(&marker, &content).unwrap();
        assert_eq!(
            repair_at(&home, &marker, true).unwrap_err().stage,
            "检查运行时"
        );
        assert_eq!(fs::read(&marker).unwrap(), content);
        assert!(!home.join("config.toml").exists());
    }

    #[test]
    fn config_repair_restores_generated_roles_from_active_lease() {
        assert_config_repair_restores_role_compaction(None);
    }

    #[test]
    fn config_repair_preserves_remote_role_compaction_without_catalog() {
        assert_config_repair_restores_role_compaction(Some(test_runtime_router_endpoint()));
    }

    fn assert_config_repair_restores_role_compaction(local_router: Option<&RuntimeRouterEndpoint>) {
        let temp = tempfile::tempdir().unwrap();
        let (home, marker) = paths(temp.path());
        fs::create_dir_all(&home).unwrap();
        let original = "# retain user model\nmodel='user-model'\n";
        fs::write(home.join("config.toml"), original).unwrap();
        let mut configured = crate::config::default_subagent_roles();
        configured
            .get_mut(crate::config::SUBAGENT_ROLE_WORKER)
            .unwrap()
            .enabled = false;
        apply_isolated_runtime_router_config(
            &home,
            RouterApplyOptions {
                remote_compaction_models: Some(&[DEFAULT_SUBAGENT_MODEL.to_string()]),
                conversation_git: false,
                model_contexts: None,
                stream_max_retries: 5,
                local_router,
                use_official_catalog: false,
                default_model: None,
                fastctx_command: None,
                subagent_optimization: true,
                subagent_model: DEFAULT_SUBAGENT_MODEL,
                subagent_reasoning_effort: DEFAULT_SUBAGENT_REASONING_EFFORT,
                subagent_roles: Some(&configured),
                marker: &marker,
                backup_root: &temp.path().join("backups"),
            },
        )
        .unwrap();
        let original_lease: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        let role_path = runtime_agent_path(
            &marker.with_file_name(CODEY_CONSTRAINTS_DIR),
            SUBAGENT_ROLE_DEFAULT,
        );
        let original_role = fs::read(&role_path).unwrap();
        let document = str::from_utf8(&original_role)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            document.get("model_provider").and_then(Item::as_str),
            local_router.map(|_| local_router::ROUTER_PROVIDER_ID)
        );
        fs::remove_file(&role_path).unwrap();
        let report = repair_at(&home, &marker, true).unwrap();
        assert!(report.repaired);
        assert_eq!(fs::read(&role_path).unwrap(), original_role);
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            original
        );
        let updated_lease: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        assert_eq!(updated_lease["backupDir"], original_lease["backupDir"]);
        assert_eq!(
            updated_lease["subagentRoles"],
            original_lease["subagentRoles"]
        );
        assert!(
            updated_lease["subagentRoles"]
                .get(crate::config::SUBAGENT_ROLE_WORKER)
                .is_none()
        );
        let (policy, _) = crate::subagent_gate::runtime_subagent_policy_paths(&home);
        let policy: serde_json::Value = serde_json::from_slice(&fs::read(policy).unwrap()).unwrap();
        assert!(
            policy["roles"]
                .get(crate::config::SUBAGENT_ROLE_WORKER)
                .is_none()
        );
        assert!(!repair_at(&home, &marker, true).unwrap().repaired);
    }
}
