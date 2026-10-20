//! Independent, filesystem-based Codex extension management. No process execution.
mod fsutil;
mod mcp;
mod skill_cache;
mod skill_config;
mod skills;
mod transaction;

use anyhow::{Context, Result, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use transaction::Change;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Scope {
    #[default]
    User,
    Project {
        #[serde(rename = "projectPath")]
        project_path: PathBuf,
    },
}

pub struct ExtensionService {
    codex_home: PathBuf,
    data_dir: PathBuf,
    user_home: PathBuf,
}

impl ExtensionService {
    pub fn new(codex_home: PathBuf, data_dir: PathBuf, user_home: PathBuf) -> Self {
        Self {
            codex_home,
            data_dir,
            user_home,
        }
    }

    fn config_path(&self, scope: &Scope) -> Result<PathBuf> {
        let path = match scope {
            Scope::User => self.codex_home.join("config.toml"),
            Scope::Project { project_path } => {
                fsutil::safe_path(project_path)?;
                ensure!(project_path.is_dir(), "项目必须为存在的绝对目录");
                project_path.join(".codex/config.toml")
            }
        };
        fsutil::safe_path(&path)?;
        Ok(path)
    }

    fn registry_path(&self) -> PathBuf {
        self.data_dir.join("managed-skills.json")
    }
    fn registry(&self) -> Result<skills::Registry> {
        fsutil::read(&self.registry_path())?
            .map(|b| serde_json::from_slice(&b).context("托管 Skill 记录损坏，拒绝修改"))
            .unwrap_or_else(|| Ok(Default::default()))
    }

    fn disabled_skills(&self) -> Result<(Vec<PathBuf>, BTreeSet<String>)> {
        let doc = mcp::parse(fsutil::read(&self.codex_home.join("config.toml"))?.as_deref())?;
        skill_config::disabled(&doc)
    }

    fn skill_roots(&self, scope: &Scope) -> Vec<PathBuf> {
        match scope {
            Scope::User => vec![
                self.user_home.join(".agents/skills"),
                self.codex_home.join("skills"),
            ],
            Scope::Project { project_path } => vec![
                project_path.join(".agents/skills"),
                project_path.join(".codex/skills"),
            ],
        }
    }

    /// Codex 按 SKILL.md 的名称加载，同范围重名会让实际生效项不确定，
    /// 因此安装、创建和改名都要确认名称在整个作用域内唯一。
    fn ensure_name_available(&self, scope: &Scope, name: &str, target: &Path) -> Result<()> {
        for root in self.skill_roots(scope) {
            for (existing, manifest) in skills::named_manifests(&root)? {
                // 系统内置资源由 Codex 提供，用户无法改名或卸载，不能因此挡住自己的 Skill。
                if manifest.components().any(|c| c.as_os_str() == ".system") {
                    continue;
                }
                ensure!(
                    existing != name || manifest == target,
                    "当前范围已存在名为 {name} 的 Skill（{}）；请先重命名或卸载后再试",
                    manifest.display()
                );
            }
        }
        Ok(())
    }

    pub fn inventory(&self, scope: &Scope) -> Result<Value> {
        let config_path = self.config_path(scope)?;
        let mut cache = fsutil::ReadCache::default();
        let doc = mcp::parse(cache.read(&config_path)?)?;
        let registry: skills::Registry = cache
            .read(&self.registry_path())?
            .map(serde_json::from_slice)
            .transpose()
            .context("托管 Skill 记录损坏，拒绝修改")?
            .unwrap_or_default();
        let user_doc = if matches!(scope, Scope::User) {
            doc.clone()
        } else {
            mcp::parse(cache.read(&self.codex_home.join("config.toml"))?)?
        };
        let (disabled, named) = skill_config::disabled(&user_doc)?;
        let unknown_paths = skill_config::unknown_paths(&user_doc)?;
        let mut warnings = vec!["当前会话和 Codey 启动参数可能覆盖这里的配置。".to_owned()];
        let mut roots = match scope {
            Scope::User => vec![
                (self.user_home.join(".agents/skills"), "user"),
                (self.codex_home.join("skills"), "user"),
            ],
            Scope::Project { project_path } => vec![
                (project_path.join(".agents/skills"), "project"),
                (project_path.join(".codex/skills"), "project"),
            ],
        };
        roots.dedup();
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        for (root, kind) in roots {
            for mut entry in
                skills::entries(&root, kind, &registry, &disabled, &mut warnings, &mut cache)
            {
                if named.contains(entry["name"].as_str().unwrap_or_default())
                    || unknown_paths
                        .contains(&PathBuf::from(entry["manifestPath"].as_str().unwrap()))
                {
                    entry["readOnly"] = json!(true);
                    entry["enabledKnown"] = json!(false);
                    entry["reason"] =
                        json!("启停规则包含名称规则或无效状态，请先在 Codex 配置中处理该规则");
                    for key in ["canEdit", "canToggle", "canRemove"] {
                        entry[key] = json!(false);
                    }
                }
                if !fsutil::writable(&self.codex_home.join("config.toml")) {
                    entry["canToggle"] = json!(false);
                }
                if seen.insert(entry["id"].as_str().unwrap().to_owned()) {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by_key(|e| e["manifestPath"].as_str().unwrap_or_default().to_owned());
        let mut fingerprints = vec![(
            config_path.to_string_lossy().into_owned(),
            cache.read(&config_path)?.map(fsutil::digest),
        )];
        for path in [self.codex_home.join("config.toml"), self.registry_path()] {
            fingerprints.push((
                path.to_string_lossy().into_owned(),
                cache.read(&path)?.map(fsutil::digest),
            ));
        }
        for entry in &mut entries {
            let manifest = PathBuf::from(entry["manifestPath"].as_str().unwrap());
            fingerprints.push((
                manifest.to_string_lossy().into_owned(),
                cache.read(&manifest)?.map(fsutil::digest),
            ));
            let dependencies = manifest.parent().unwrap().join("agents/openai.yaml");
            fingerprints.push((
                dependencies.to_string_lossy().into_owned(),
                cache.read(&dependencies).ok().flatten().map(fsutil::digest),
            ));
            if entry["ownership"] == "managed" {
                match fsutil::walk(manifest.parent().unwrap()) {
                    Ok(paths) => {
                        for path in paths {
                            fingerprints.push((
                                path.to_string_lossy().into_owned(),
                                cache.read(&path)?.map(|b| {
                                    format!(
                                        "{}:{:?}",
                                        fsutil::digest(b),
                                        fsutil::file_mode(&path).ok().flatten()
                                    )
                                }),
                            ));
                        }
                    }
                    Err(error) => {
                        // 目录里一个符号链接或超限文件就让整页失效会把用户挡在外面，
                        // 因此降级为「暂停编辑与启停」。卸载保持可用：它是清除该问题
                        // 的出口，且只删除本模块登记过的文件。
                        entry["canEdit"] = json!(false);
                        entry["canToggle"] = json!(false);
                        entry["reason"] =
                            json!(format!("目录内容无法完整校验，已暂停编辑与启停：{error}"));
                        warnings.push(format!(
                            "{} 的内容无法完整校验，已暂停编辑与启停：{error}",
                            manifest.to_string_lossy()
                        ));
                    }
                }
            }
            if entry["ownership"] == "external" && entry["canRemove"] == true {
                let removal_fingerprints = (|| -> Result<Vec<_>> {
                    skills::external_removal_files(manifest.parent().unwrap())?
                        .into_iter()
                        .map(|path| {
                            let bytes = cache.read(&path)?.context("Skill 资源已消失")?;
                            Ok((
                                path.to_string_lossy().into_owned(),
                                Some(format!(
                                    "{}:{:?}",
                                    fsutil::digest(bytes),
                                    fsutil::file_mode(&path)?
                                )),
                            ))
                        })
                        .collect()
                })();
                match removal_fingerprints {
                    Ok(values) => fingerprints.extend(values),
                    Err(error) => {
                        entry["canRemove"] = json!(false);
                        entry["reason"] = json!(format!("目录内容无法安全删除：{error}"));
                    }
                }
            }
        }
        cache.ensure_complete()?;
        let revision = fsutil::digest(&serde_json::to_vec(&fingerprints)?);
        let mut mcps = mcp::list(&doc, &config_path.to_string_lossy())?;
        for entry in &mut mcps {
            entry["scope"] = json!(if matches!(scope, Scope::User) {
                "user"
            } else {
                "project"
            });
            entry["updatedAt"] = json!(fsutil::updated_at(&config_path));
            if !fsutil::writable(&config_path) {
                entry["readOnly"] = json!(true);
                entry["reason"] = json!("配置文件或所在目录不可写");
                for key in ["canEdit", "canToggle", "canRemove"] {
                    entry[key] = json!(false);
                }
            }
        }
        Ok(
            json!({"scope":scope,"configPath":config_path,"skillConfigPath":self.codex_home.join("config.toml"),"revision":revision,"mcps":mcps,"skills":entries,"warnings":warnings,"applyNotice":"MCP 保存后自动刷新 Codex 配置；Skill 变更请在新会话中确认。当前 Codey 启动覆盖可能优先生效。"}),
        )
    }

    pub fn mcp_configuration(&self, scope: &Scope, id: &str) -> Result<Value> {
        let doc = mcp::parse(fsutil::read(&self.config_path(scope)?)?.as_deref())?;
        let config = mcp::object(&mcp::table(&doc, id)?)?;
        mcp::validate_existing(&config)?;
        Ok(config)
    }

    fn entry(&self, inventory: &Value, id: &str) -> Result<Value> {
        inventory["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .cloned()
            .context("Skill 不存在或不属于当前作用域")
    }

    fn required<'a>(request: &'a Value, key: &str) -> Result<&'a str> {
        request
            .get(key)
            .and_then(Value::as_str)
            .context(format!("缺少字符串参数 {key}"))
    }

    pub fn dispatch(&self, request: Value) -> Result<Value> {
        let action = Self::required(&request, "action")?;
        if ["list_skill_cache", "read_skill_cache", "remove_skill_cache"].contains(&action) {
            return self.dispatch_skill_cache(&request, action);
        }
        if let Some(text) = request.get("content").and_then(Value::as_str) {
            ensure!(
                text.len() as u64 <= fsutil::MAX_FILE,
                "提交内容超过大小限制"
            );
        }
        let mcp_table = if action == "save_mcp" {
            ensure!(
                request.get("configToml").is_none(),
                "MCP 管理仅接受 configJson 配置，不支持 configToml"
            );
            Some(mcp::json_table(
                request
                    .get("configJson")
                    .context("请提供 configJson 配置")?,
            )?)
        } else {
            None
        };
        let scope: Scope = request
            .get("scope")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let config_path = self.config_path(&scope)?;
        if action == "list" {
            return self.inventory(&scope);
        }
        if action == "preview_skill_install" {
            let source = PathBuf::from(Self::required(&request, "sourcePath")?);
            let files = skills::load_source(&source)?;
            let (name, description) = skills::metadata(&files[Path::new("SKILL.md")].bytes)?;
            let total_bytes: usize = files.values().map(|file| file.bytes.len()).sum();
            let preview_files: Vec<_> = files.iter().take(100).map(|(path, file)| {
                json!({"path":path,"size":file.bytes.len(),"executable":file.executable})
            }).collect();
            return Ok(json!({
                "name":name,"description":description,"sourcePath":source,
                "sourceKind":if source.is_dir() {"directory"} else {"zip"},
                "files":preview_files,"fileCount":files.len(),"totalBytes":total_bytes,
                "sourceRevision":skills::source_digest(&files)
            }));
        }
        if action == "export_skill" {
            let inventory = self.inventory(&scope)?;
            let entry = self.entry(&inventory, Self::required(&request, "id")?)?;
            let bytes = skills::export(Path::new(entry["sourcePath"].as_str().unwrap()))?;
            ensure!(
                self.inventory(&scope)?["revision"] == inventory["revision"],
                "导出期间内容已变化，请刷新后重试"
            );
            return Ok(
                json!({"filename":format!("{}.zip",entry["name"].as_str().unwrap_or("skill")),"mediaType":"application/zip","dataBase64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
            );
        }
        if action == "get_mcp" {
            let inventory = self.inventory(&scope)?;
            let id = Self::required(&request, "id")?;
            let doc = mcp::parse(fsutil::read(&config_path)?.as_deref())?;
            return Ok(
                json!({"id":id,"configJson":mcp::redacted(&mcp::table(&doc,id)?)?,"revision":inventory["revision"]}),
            );
        }
        if action == "read_skill" || action == "validate_skill" {
            let inventory = self.inventory(&scope)?;
            let entry = self.entry(&inventory, Self::required(&request, "id")?)?;
            let path = PathBuf::from(entry["manifestPath"].as_str().unwrap());
            let bytes = fsutil::read(&path)?.context("Skill 文件不存在")?;
            if action == "read_skill" {
                return Ok(
                    json!({"id":entry["id"],"content":String::from_utf8(bytes)?,"revision":inventory["revision"],"readOnly":entry["ownership"] != "managed" || entry["readOnly"] == true}),
                );
            }
            let meta = skills::metadata(&bytes);
            let tree = fsutil::walk(path.parent().unwrap());
            let ok = meta.is_ok() && tree.is_ok();
            return Ok(
                json!({"ok":ok,"summary":if ok {"Skill 结构检查通过，未执行内容；依赖与任务效果仍需实际验证"}else{"Skill 检查失败"},"checks":[{"name":"元数据","ok":meta.is_ok(),"message":meta.err().map(|e|e.to_string()).unwrap_or_else(||"元数据有效".to_owned())},{"name":"资源路径","ok":tree.is_ok(),"message":tree.err().map(|e|e.to_string()).unwrap_or_else(||"目录和文件符合安全限制".to_owned())}]}),
            );
        }
        ensure!(
            [
                "save_mcp",
                "set_mcp_enabled",
                "set_mcps_enabled",
                "remove_mcp",
                "save_skill",
                "set_skill_enabled",
                "set_skills_enabled",
                "install_skill",
                "create_skill",
                "uninstall_skill",
            ]
            .contains(&action),
            "不支持的扩展管理操作"
        );
        if [
            "remove_mcp",
            "uninstall_skill",
            "set_mcps_enabled",
            "set_skills_enabled",
        ]
        .contains(&action)
            || (["set_mcp_enabled", "set_skill_enabled"].contains(&action)
                && request.get("enabled").and_then(Value::as_bool) == Some(true))
        {
            ensure!(
                request.get("confirmed").and_then(Value::as_bool) == Some(true),
                "请先确认操作影响"
            );
        }
        Self::required(&request, "revision")?;
        let _locks = transaction::lock(&[
            self.data_dir.join("extensions.lock"),
            config_path.with_extension("toml.lock"),
            self.codex_home.join("config.toml.lock"),
        ])?;
        let inventory = self.inventory(&scope)?;
        ensure!(
            request["revision"] == inventory["revision"],
            "配置已变化，请刷新后重新操作"
        );
        let mut changes = Vec::new();
        match action {
            "save_mcp" | "set_mcp_enabled" | "remove_mcp" => {
                ensure!(
                    fsutil::writable(&config_path),
                    "MCP 配置文件或所在目录不可写"
                );
                let id = Self::required(&request, "id")?;
                let mut doc = mcp::parse(fsutil::read(&config_path)?.as_deref())?;
                if action == "save_mcp" {
                    if request.get("createOnly").and_then(Value::as_bool) == Some(true) {
                        ensure!(
                            mcp::table(&doc, id).is_err(),
                            "此 MCP 服务标识已存在，请打开原有服务进行编辑"
                        );
                    }
                    let was_enabled = mcp::table(&doc, id)
                        .ok()
                        .and_then(|item| item.get("enabled").and_then(toml_edit::Item::as_bool))
                        .unwrap_or(false);
                    mcp::save(&mut doc, id, mcp_table.context("缺少 MCP 配置")?)?;
                    let enabled = mcp::table(&doc, id)?
                        .get("enabled")
                        .and_then(toml_edit::Item::as_bool)
                        .unwrap_or(true);
                    ensure!(
                        !enabled
                            || was_enabled
                            || request.get("confirmed").and_then(Value::as_bool) == Some(true),
                        "保存将启用 MCP，请先确认信任此服务"
                    );
                } else {
                    let item = mcp::table(&doc, id)?;
                    if action == "remove_mcp" {
                        doc["mcp_servers"]
                            .as_table_like_mut()
                            .context("MCP 表无效")?
                            .remove(id);
                    } else {
                        let enabled = request
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .context("缺少 enabled")?;
                        ensure!(
                            item.get("enabled").is_none_or(|v| v.as_bool().is_some()),
                            "MCP 启用状态无效，请先修复配置"
                        );
                        if enabled {
                            mcp::validate_existing(&mcp::object(&item)?)?;
                        }
                        doc["mcp_servers"][id]
                            .as_table_like_mut()
                            .context("MCP 配置必须为表")?
                            .insert("enabled", toml_edit::value(enabled));
                    }
                }
                changes.push(Change::new(
                    config_path,
                    Some(doc.to_string().into_bytes()),
                )?);
            }
            "set_mcps_enabled" => {
                ensure!(
                    fsutil::writable(&config_path),
                    "MCP 配置文件或所在目录不可写"
                );
                let ids = Self::batch_ids(&request)?;
                let enabled = request
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .context("缺少 enabled")?;
                let mut doc = mcp::parse(fsutil::read(&config_path)?.as_deref())?;
                for id in ids {
                    let item = mcp::table(&doc, &id)?;
                    ensure!(
                        item.get("enabled").is_none_or(|v| v.as_bool().is_some()),
                        "MCP 启用状态无效，请先修复配置"
                    );
                    if enabled {
                        mcp::validate_existing(&mcp::object(&item)?)?;
                    }
                    doc["mcp_servers"][&id]
                        .as_table_like_mut()
                        .context("MCP 配置必须为表")?
                        .insert("enabled", toml_edit::value(enabled));
                }
                changes.push(Change::new(
                    config_path,
                    Some(doc.to_string().into_bytes()),
                )?);
            }
            "set_skill_enabled" | "set_skills_enabled" => {
                let ids = if action == "set_skills_enabled" {
                    Self::batch_ids(&request)?
                } else {
                    vec![Self::required(&request, "id")?.to_owned()]
                };
                let enabled = request
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .context("缺少 enabled")?;
                let config = self.codex_home.join("config.toml");
                ensure!(
                    fsutil::writable(&config),
                    "Skill 启停配置文件或所在目录不可写"
                );
                let mut doc = mcp::parse(fsutil::read(&config)?.as_deref())?;
                for id in ids {
                    let entry = self.entry(&inventory, &id)?;
                    ensure!(entry["readOnly"] == false, "系统或插件 Skill 不允许修改");
                    ensure!(
                        entry["enabledKnown"] == true,
                        "Skill 启用状态未知，无法修改"
                    );
                    if enabled {
                        ensure!(
                            entry["configurationStatus"] == "valid",
                            "Skill 元数据无效，请先修复后启用"
                        );
                    }
                    let path = entry["manifestPath"].as_str().unwrap();
                    skill_config::set_enabled(&mut doc, path, enabled)?;
                }
                changes.push(Change::new(config, Some(doc.to_string().into_bytes()))?);
            }
            "uninstall_skill"
                if self.entry(&inventory, Self::required(&request, "id")?)?["ownership"]
                    == "external" =>
            {
                let entry = self.entry(&inventory, Self::required(&request, "id")?)?;
                ensure!(
                    entry["canRemove"] == true,
                    "Skill 当前不允许删除，请检查目录或冲突规则"
                );
                let path = PathBuf::from(entry["sourcePath"].as_str().unwrap());
                for target in skills::external_removal_files(&path)? {
                    ensure!(fsutil::writable(&target), "Skill 文件或所在目录不可写");
                    changes.push(Change::new(target, None)?);
                }
            }
            "save_skill" | "uninstall_skill" => {
                let entry = self.entry(&inventory, Self::required(&request, "id")?)?;
                ensure!(
                    entry["ownership"] == "managed",
                    "仅允许编辑或卸载 Codey 托管 Skill"
                );
                ensure!(
                    entry["readOnly"] == false,
                    "Skill 当前为只读，请先处理冲突规则"
                );
                let path = PathBuf::from(entry["sourcePath"].as_str().unwrap());
                ensure!(
                    fsutil::writable(&path.join("SKILL.md")),
                    "Skill 文件或所在目录不可写"
                );
                let mut registry = self.registry()?;
                let managed = registry.get_mut(&path).context("托管记录不存在")?;
                // 卸载只处理本模块登记过的文件，所以目录里多出的内容（符号链接、
                // 超限文件等无法枚举的项）不该把用户锁在界面上：枚举失败时退回登记
                // 清单，额外内容保持原样。编辑仍要求目录可完整校验，避免覆盖看不见
                // 的外部修改。
                let uninstalling = action == "uninstall_skill";
                let actual = match fsutil::walk(&path) {
                    Ok(actual) => actual,
                    Err(_) if uninstalling => managed.files.keys().cloned().collect(),
                    Err(error) => return Err(error),
                };
                ensure!(
                    uninstalling || actual.len() == managed.files.len(),
                    "Skill 文件集合已被外部修改，请保留原文件并重新导入"
                );
                for p in actual {
                    // 覆盖写入的目标内容由请求决定，但权限必须保持与登记一致。
                    let skip_content = action == "save_skill" && p == path.join("SKILL.md");
                    if !skip_content {
                        match fsutil::read(&p) {
                            Ok(bytes) => ensure!(
                                managed.files.get(&p) == bytes.map(|b| fsutil::digest(&b)).as_ref(),
                                "Skill 文件已被外部修改，拒绝覆盖或删除"
                            ),
                            // 登记项被替换成链接或不可读内容时，删除它本身不会跟随链接，
                            // 是用户清除该 Skill 的出口；编辑必须拒绝。
                            Err(_) if uninstalling => {}
                            Err(error) => return Err(error),
                        }
                    }
                    if let Some(expected) = managed.modes.get(&p) {
                        match fsutil::file_mode(&p) {
                            Ok(mode) => ensure!(
                                mode == Some(*expected),
                                "Skill 文件权限已被外部修改，拒绝覆盖或删除"
                            ),
                            Err(_) if uninstalling => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
                if action == "save_skill" {
                    let content = Self::required(&request, "content")?.as_bytes().to_vec();
                    let (name, _) = skills::metadata(&content)?;
                    ensure!(
                        content.len() as u64 <= fsutil::MAX_FILE,
                        "Skill 文件超过大小限制"
                    );
                    let target = path.join("SKILL.md");
                    self.ensure_name_available(&scope, &name, &target)?;
                    // 身份和启停规则按路径保留；改名不能引入已有名称规则，
                    // 否则保存后将无法确认启用状态，且失去继续编辑的入口。
                    ensure!(
                        !self.disabled_skills()?.1.contains(&name),
                        "存在同名 Skill 规则，无法保证保存后的启用状态；请先处理冲突规则"
                    );
                    managed
                        .files
                        .insert(target.clone(), fsutil::digest(&content));
                    changes.push(Change::new(target, Some(content))?);
                } else {
                    for target in managed.files.keys() {
                        changes.push(Change::new(target.clone(), None)?);
                    }
                    registry.remove(&path);
                }
                changes.push(Change::new(
                    self.registry_path(),
                    Some(serde_json::to_vec(&registry)?),
                )?);
            }
            "install_skill" | "create_skill" => {
                let (source, files) = if action == "create_skill" {
                    let bytes = Self::required(&request, "content")?.as_bytes().to_vec();
                    skills::metadata(&bytes)?;
                    (
                        "Codey 创建".to_owned(),
                        BTreeMap::from([(
                            PathBuf::from("SKILL.md"),
                            skills::SourceFile {
                                bytes,
                                executable: false,
                            },
                        )]),
                    )
                } else {
                    let source = PathBuf::from(Self::required(&request, "sourcePath")?);
                    (
                        source.to_string_lossy().into_owned(),
                        skills::load_source(&source)?,
                    )
                };
                // 新界面传预览版本；兼容旧调用方的 expectedSourceDigest 和无预览安装。
                let source_revision = skills::source_digest(&files);
                for key in ["sourceRevision", "expectedSourceDigest"] {
                    if let Some(expected) = request.get(key) {
                        let expected = expected.as_str().context("安装来源版本必须为字符串")?;
                        ensure!(
                            expected == source_revision,
                            "安装来源内容已变化，请重新预览后安装"
                        );
                    }
                }
                let (name, _) = skills::metadata(&files.get(Path::new("SKILL.md")).unwrap().bytes)?;
                ensure!(
                    !self.disabled_skills()?.1.contains(&name),
                    "存在同名 Skill 规则，无法保证新安装默认禁用；请先处理冲突规则"
                );
                let root = match &scope {
                    Scope::User => self.codex_home.join("skills"),
                    Scope::Project { project_path } => project_path.join(".agents/skills"),
                };
                let target = root.join(&name);
                fsutil::safe_path(&target)?;
                ensure!(
                    fsutil::writable(&target)
                        && fsutil::writable(&self.codex_home.join("config.toml")),
                    "Skill 目标或启停配置目录不可写"
                );
                ensure!(!target.exists(), "安装目标已存在，请先解决同名目录冲突");
                self.ensure_name_available(&scope, &name, &target.join("SKILL.md"))?;
                // 禁用规则先于 SKILL.md 落盘，避免正在运行的 Codex 提前发现并启用。
                let user_config = self.codex_home.join("config.toml");
                let mut doc = mcp::parse(fsutil::read(&user_config)?.as_deref())?;
                skill_config::set_enabled(
                    &mut doc,
                    &target.join("SKILL.md").to_string_lossy(),
                    false,
                )?;
                changes.push(Change::new(
                    user_config,
                    Some(doc.to_string().into_bytes()),
                )?);
                let mut managed = skills::Managed {
                    source,
                    ..Default::default()
                };
                for (relative, source_file) in files {
                    let path = target.join(relative);
                    let bytes = source_file.bytes;
                    managed.files.insert(path.clone(), fsutil::digest(&bytes));
                    let change = Change::new(path.clone(), Some(bytes))?;
                    #[cfg(unix)]
                    let change = {
                        let mode = if source_file.executable { 0o700 } else { 0o600 };
                        managed.modes.insert(path, mode);
                        Change {
                            after_mode: Some(mode),
                            ..change
                        }
                    };
                    changes.push(change);
                }
                let mut registry = self.registry()?;
                registry.insert(target, managed);
                changes.push(Change::new(
                    self.registry_path(),
                    Some(serde_json::to_vec(&registry)?),
                )?);
            }
            _ => unreachable!(),
        }
        changes.retain(|c| c.before != c.after || c.before_mode != c.after_mode);
        if changes.is_empty() {
            return Ok(
                json!({"inventory":inventory,"applyStatus":"unchanged","message":"配置未变化"}),
            );
        }
        // 扫描、解析和生成变更期间也可能发生外部编辑，再次核对整个作用域。
        ensure!(
            self.inventory(&scope)?["revision"] == inventory["revision"],
            "配置在准备操作期间发生变化，请刷新后重试"
        );
        let removed: Vec<_> = changes
            .iter()
            .filter(|c| c.after.is_none())
            .map(|c| c.path.clone())
            .collect();
        let created: Vec<_> = changes
            .iter()
            .filter(|c| c.before.is_none())
            .map(|c| c.path.clone())
            .collect();
        transaction::commit(&self.data_dir, action, changes)
            .inspect_err(|_| self.clean_empty_skill_dirs(&scope, &created))?;
        self.clean_empty_skill_dirs(&scope, &removed);
        Ok(
            json!({"inventory":self.inventory(&scope)?,"applyStatus":if matches!(action, "save_mcp" | "set_mcp_enabled" | "set_mcps_enabled" | "remove_mcp") {"reload-required"} else {"restart-required"},"message":"配置已保存。"}),
        )
    }

    fn batch_ids(request: &Value) -> Result<Vec<String>> {
        let ids = request
            .get("ids")
            .and_then(Value::as_array)
            .context("缺少 ids 数组")?;
        ensure!(
            !ids.is_empty() && ids.len() <= 100,
            "一次批量操作支持 1 到 100 个条目"
        );
        let ids: BTreeSet<String> = ids
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .context("条目标识必须为非空字符串")
            })
            .collect::<Result<_>>()?;
        Ok(ids.into_iter().collect())
    }

    fn clean_empty_skill_dirs(&self, scope: &Scope, removed: &[PathBuf]) {
        let roots = match scope {
            Scope::User => vec![
                self.codex_home.join("skills"),
                self.user_home.join(".agents/skills"),
            ],
            Scope::Project { project_path } => vec![
                project_path.join(".agents/skills"),
                project_path.join(".codex/skills"),
            ],
        };
        for file in removed {
            let Some(root) = roots.iter().find(|root| file.starts_with(root)) else {
                continue;
            };
            let mut dir = file.parent();
            while let Some(path) = dir {
                if path == root || !path.starts_with(root) || fsutil::safe_path(path).is_err() {
                    break;
                }
                if std::fs::remove_dir(path).is_err() {
                    break;
                }
                dir = path.parent();
            }
        }
    }
}

#[cfg(test)]
mod tests;
