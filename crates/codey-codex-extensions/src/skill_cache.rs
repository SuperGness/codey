use crate::{ExtensionService, fsutil, skills, transaction};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

// 目录必须位于插件的 skills 目录内，不能把插件根或缓存根作为删除目标。
fn removable_files(root: &Path, directory: &Path) -> Result<Vec<PathBuf>> {
    fsutil::safe_path(root)?;
    fsutil::safe_path(directory)?;
    let relative = directory
        .strip_prefix(root)
        .context("Skill 不属于插件缓存")?;
    let components: Vec<_> = relative.components().collect();
    ensure!(
        components.len() >= 3
            && components[..components.len() - 1]
                .iter()
                .enumerate()
                .any(|(index, component)| index > 0 && component.as_os_str() == "skills"),
        "无法确认独立 Skill 目录，禁止删除插件目录或缓存根"
    );
    let manifest = directory.join("SKILL.md");
    let files = fsutil::walk(directory)?;
    ensure!(files.contains(&manifest), "Skill 文件已消失");
    ensure!(
        !files.iter().any(
            |path| path != &manifest && path.file_name().is_some_and(|name| name == "SKILL.md")
        ),
        "目录包含其他独立 Skill，无法安全删除"
    );
    ensure!(
        fsutil::writable(directory)
            && directory.parent().is_some_and(fsutil::writable)
            && files
                .iter()
                .all(|path| path.parent().is_some_and(fsutil::writable)),
        "Skill 缓存目录不可写"
    );
    Ok(files)
}

// 只删除已经为空的目录；不删除扫描之外的文件，也不向 Skill 的父目录递归。
fn clean_empty_dirs(directory: &Path, depth: usize) {
    if depth > 20 || fsutil::safe_path(directory).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().take(fsutil::MAX_FILES) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            clean_empty_dirs(&path, depth + 1);
        }
    }
    let _ = fs::remove_dir(directory);
}

impl ExtensionService {
    fn skill_cache_inventory(&self) -> Result<Value> {
        let root = self.codex_home.join("plugins/cache");
        let mut warnings =
            vec!["插件缓存可能仍被已安装插件引用，删除后插件可能需要重新下载。".to_owned()];
        let mut cache = fsutil::ReadCache::default();
        let mut entries = skills::entries(
            &root,
            "plugin",
            &Default::default(),
            &[],
            &mut warnings,
            &mut cache,
        );
        let mut fingerprints = Vec::new();
        for entry in &mut entries {
            let directory = PathBuf::from(entry["sourcePath"].as_str().unwrap());
            let files = removable_files(&root, &directory);
            entry["canRemove"] = json!(files.is_ok());
            entry["readOnly"] = json!(true);
            entry["canEdit"] = json!(false);
            entry["canToggle"] = json!(false);
            entry["canCheck"] = json!(false);
            entry["enabledKnown"] = json!(false);
            entry["reason"] = json!(match &files {
                Ok(_) => "插件缓存内容仅供查看；删除会移除该 Skill 目录及其资源".to_owned(),
                Err(error) => error.to_string(),
            });
            // 可删除目录的每项资源参与版本检查，防止按过时确认删除变化后的内容。
            let paths = files.unwrap_or_else(|_| vec![directory.join("SKILL.md")]);
            for path in paths {
                fingerprints.push(json!([
                    path,
                    cache.read(&path).ok().flatten().map(fsutil::digest),
                    fsutil::file_mode(&path).ok().flatten()
                ]));
            }
        }
        entries.sort_by_key(|entry| {
            entry["manifestPath"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        });
        cache.ensure_complete()?;
        let revision = fsutil::digest(&serde_json::to_vec(&json!([root, entries, fingerprints]))?);
        Ok(json!({"revision":revision,"skills":entries,"warnings":warnings}))
    }

    pub(crate) fn dispatch_skill_cache(&self, request: &Value, action: &str) -> Result<Value> {
        if action == "list_skill_cache" {
            return self.skill_cache_inventory();
        }
        let id = Self::required(request, "id")?;
        if action == "read_skill_cache" {
            let inventory = self.skill_cache_inventory()?;
            let entry = self.entry(&inventory, id)?;
            let bytes = fsutil::read(Path::new(entry["manifestPath"].as_str().unwrap()))?
                .context("Skill 缓存文件不存在")?;
            ensure!(
                self.skill_cache_inventory()?["revision"] == inventory["revision"],
                "缓存内容已变化，请刷新后重试"
            );
            return Ok(
                json!({"id":id,"content":String::from_utf8(bytes)?,"revision":inventory["revision"],"readOnly":true}),
            );
        }
        ensure!(
            request.get("confirmed").and_then(Value::as_bool) == Some(true),
            "请先确认删除缓存的影响"
        );
        Self::required(request, "revision")?;
        let _locks = transaction::lock(&[
            self.data_dir.join("extensions.lock"),
            self.codex_home.join("plugins/skill-cache.lock"),
        ])?;
        let inventory = self.skill_cache_inventory()?;
        ensure!(
            request["revision"] == inventory["revision"],
            "缓存已变化，请刷新后重新操作"
        );
        let entry = self.entry(&inventory, id)?;
        ensure!(
            entry["canRemove"] == true,
            "{}",
            entry["reason"].as_str().unwrap_or("此缓存无法安全删除")
        );
        let directory = PathBuf::from(entry["sourcePath"].as_str().unwrap());
        let files = removable_files(&self.codex_home.join("plugins/cache"), &directory)?;
        let changes = files
            .into_iter()
            .map(|path| transaction::Change::new(path, None))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            self.skill_cache_inventory()?["revision"] == inventory["revision"],
            "缓存在准备删除期间发生变化，请刷新后重试"
        );
        transaction::commit(&self.data_dir, "remove_skill_cache", changes)?;
        clean_empty_dirs(&directory, 0);
        Ok(
            json!({"cache":self.skill_cache_inventory()?,"message":"已删除所选 Skill 缓存；插件可能需要重新下载相关资源。"}),
        )
    }
}
