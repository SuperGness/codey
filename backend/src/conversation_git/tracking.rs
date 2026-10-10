//! 执行前后核对确定的编辑结果；只持久化内容摘要，不保存源码。
use super::*;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};

pub(crate) const HOOK_ARGUMENT: &str = "--codey-conversation-git-hook";
pub(crate) const HOOK_TIMEOUT_SECONDS: u64 = 60;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Fingerprint {
    mode: String,
    hash: String,
}

fn fingerprint(entry: Option<&Entry>) -> Option<Fingerprint> {
    entry.map(|entry| Fingerprint {
        mode: entry.mode.clone(),
        hash: digest(&entry.bytes),
    })
}

fn has_current_live_edit(ledger: &Ledger, session: &str, path: &str, head: Option<&Entry>) -> bool {
    ledger
        .pending
        .values()
        .any(|pending| pending.session == session && pending.candidates.contains_key(path))
        || ledger.files.get(path).is_some_and(|claim| {
            claim.sessions.contains(session) && {
                let head = fingerprint(head);
                claim.baseline == head && claim.expected != head
            }
        })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Claim {
    sessions: BTreeSet<String>,
    baseline: Option<Fingerprint>,
    expected: Option<Fingerprint>,
    valid: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Candidate {
    before: Option<Fingerprint>,
    claim: Claim,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    session: String,
    candidates: BTreeMap<String, Candidate>,
    patch: bool,
    #[serde(default)]
    aggregate: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    files: BTreeMap<String, Claim>,
    pending: BTreeMap<String, Pending>,
}

fn ledger_path(home: &Path, root: &Path) -> PathBuf {
    home.join("codey-conversation-git-v2").join(format!(
        "{}.json",
        digest(root.as_os_str().as_encoded_bytes())
    ))
}

fn load(path: &Path) -> Result<Ledger> {
    match read_bounded(path, MAX_BYTES) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Ledger::default())
        }
        Err(error) => Err(error),
    }
}

fn lock_ledger(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().context("文件记录路径无效")?)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("文件归属记录正在更新，请稍后重试")?;
    Ok(lock)
}

fn field<'a>(input: &'a Value, snake: &str, camel: &str) -> Option<&'a Value> {
    input.get(snake).or_else(|| input.get(camel))
}

pub(super) fn metadata(home: &Path, path: &Path) -> Result<Value> {
    let path = crate::session_transfer::checked_rollout_path(home, path)?;
    let mut bytes = Vec::new();
    BufReader::new(File::open(path)?)
        .take(1024 * 1024 + 1)
        .read_until(b'\n', &mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "对话身份记录过大");
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(value["type"] == "session_meta", "缺少可信的对话身份记录");
    Ok(value["payload"].clone())
}

// 身份只来自运行时 Hook 与本地会话记录；不接受工具参数中声称的父对话。
fn owner(
    home: &Path,
    input: &Value,
    workspace: &Path,
    runtime: Option<&str>,
) -> Result<(String, String)> {
    let session = field(input, "session_id", "sessionId")
        .and_then(Value::as_str)
        .context("文件记录缺少对话身份")?;
    let transcript = field(input, "agent_transcript_path", "agentTranscriptPath")
        .or_else(|| field(input, "transcript_path", "transcriptPath"))
        .and_then(Value::as_str)
        .context("文件记录缺少可信会话路径")?;
    let meta = metadata(home, Path::new(transcript))?;
    let id = meta["id"].as_str().context("会话记录缺少身份")?;
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "会话身份格式无效");
    ensure!(
        meta["cwd"]
            .as_str()
            .and_then(|cwd| Path::new(cwd).canonicalize().ok())
            .as_deref()
            == Some(workspace),
        "会话工作区与编辑工作区不一致"
    );
    let agent = field(input, "agent_id", "agentId")
        .or_else(|| input.get("agent_name"))
        .or_else(|| input.get("agentName"))
        .or_else(|| input.get("subagent_id"))
        .or_else(|| input.get("subagentId"))
        .and_then(Value::as_str);
    let parent = field(&meta, "parent_thread_id", "parentThreadId").and_then(Value::as_str);
    if let Some(parent) = parent {
        ensure!(
            session == parent || session == id,
            "子代理 Hook 的会话身份不匹配"
        );
        ensure!(agent == Some(id), "子代理身份不能可靠绑定");
        let runtime = runtime.context("缺少子代理运行时身份")?;
        let context = crate::subagent_orchestrator::ChildToolContext {
            agent_id: id,
            agent_type: field(input, "agent_type", "agentType")
                .or_else(|| input.get("subagent_type"))
                .or_else(|| input.get("subagentType"))
                .and_then(Value::as_str),
            transcript_path: Some(transcript),
            tool_name: "",
            tool_input: None,
        };
        ensure!(
            crate::subagent_orchestrator::trusted_child_workspace(
                &home.join(crate::subagent_gate::STATE_DIRECTORY),
                runtime,
                parent,
                context,
                workspace
            )?,
            "子代理身份或工作区未通过可信记录校验"
        );
        let (_, parent_workspace, _) = source(home, parent)?;
        ensure!(
            parent_workspace.canonicalize()? == workspace,
            "子代理在其他工作区编辑，不能纳入父对话提交"
        );
        Ok((parent.into(), id.into()))
    } else {
        ensure!(
            session == id
                && agent.is_none_or(|agent| matches!(agent, "" | "root" | "/root") || agent == id),
            "主对话身份不匹配"
        );
        ensure!(
            meta.pointer("/source/subagent").is_none(),
            "子代理记录缺少父对话，不能确认归属"
        );
        Ok((session.into(), id.into()))
    }
}

pub(super) fn normalized_input(name: &str, args: &Value) -> Result<Option<(bool, Value)>> {
    if matches!(name, "apply_patch" | "functions.apply_patch") {
        return Ok(Some((true, args.clone())));
    }
    if name.contains("codey_fastctx") && (name.ends_with("__replace") || name.ends_with(".replace"))
    {
        let args = if let Some(text) = args.as_str() {
            serde_json::from_str(text)?
        } else {
            args.clone()
        };
        return Ok((args["dry_run"] != true).then_some((false, args)));
    }
    if matches!(name, "functions.exec" | "exec") {
        let code = args
            .as_str()
            .or_else(|| args["code"].as_str())
            .or_else(|| args["input"].as_str())
            .unwrap_or_default()
            .trim();
        let code = if code.starts_with("// @exec:") {
            code.split_once('\n').map_or("", |(_, code)| code).trim()
        } else {
            code
        };
        // 仅解析单次已知调用，绝不执行或猜测任意 JavaScript 的写入范围。
        let literal = code
            .strip_prefix("text(await tools.apply_patch(")
            .and_then(|code| code.trim_end_matches(';').strip_suffix("))"));
        if let Some(literal) = literal {
            let patch: String = serde_json::from_str(literal.trim())
                .context("聚合工具中的补丁须使用 JSON 字符串")?;
            return Ok(Some((true, Value::String(patch))));
        }
    }
    Ok(None)
}

pub(super) fn operations(
    root: &Path,
    workspace: &Path,
    patch: bool,
    args: &Value,
) -> Result<Vec<(String, Edit)>> {
    if patch {
        return parse_patch(
            args.as_str()
                .or_else(|| args["input"].as_str())
                .or_else(|| args["patch"].as_str())
                .context("补丁参数不受支持")?,
        )?
        .into_iter()
        .map(|(path, edit)| Ok((relative_path(root, workspace, &path)?, edit)))
        .collect();
    }
    let raw = args["path"].as_str().context("替换没有文件路径")?;
    let target = workspace.join(raw);
    if !target.is_dir() {
        return Ok(vec![(
            relative_path(root, workspace, raw)?,
            Edit::Replace(args.clone()),
        )]);
    }
    let target = target.canonicalize()?;
    ensure!(target.starts_with(workspace), "替换目录不在当前工作区");
    // 候选来自 Git 文件清单，完成时还必须匹配工具报告的实际写入文件。
    let output = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            target.to_str().context("目录编码不受支持")?,
        ],
        None,
        None,
    )?;
    let mut paths = BTreeSet::new();
    for path in output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)?;
        paths.insert(relative_path(
            root,
            workspace,
            root.join(path).to_str().context("文件名编码不受支持")?,
        )?);
    }
    ensure!(paths.len() <= 10_000, "目录文件过多，无法可靠记录此次替换");
    let mut edits = Vec::new();
    let mut total = 0;
    for path in paths {
        let Ok(before) = disk_entry(root, &path) else {
            continue;
        };
        total += before.as_ref().map_or(0, |entry| entry.bytes.len());
        ensure!(
            total <= 16 * 1024 * 1024,
            "目录文本过多，无法可靠记录此次替换"
        );
        if apply_edit(before.as_ref(), &Edit::Replace(args.clone()))
            .is_ok_and(|after| after != before)
        {
            edits.push((path, Edit::Replace(args.clone())));
        }
    }
    Ok(edits)
}

pub(super) fn output_text(value: &Value) -> Option<String> {
    if value.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if let Some(text) = value.as_str() {
        return match serde_json::from_str::<Value>(text) {
            Ok(parsed) if !parsed.is_string() => output_text(&parsed),
            _ => Some(text.into()),
        };
    }
    let content = value
        .as_array()
        .or_else(|| value.get("content")?.as_array())?;
    content
        .iter()
        .map(|block| {
            matches!(block["type"].as_str(), Some("text" | "input_text"))
                .then(|| block["text"].as_str())
                .flatten()
        })
        .collect::<Option<Vec<_>>>()
        .map(|texts| texts.join("\n"))
}

pub(super) fn reported_files(
    output: &Value,
    patch: bool,
    root: &Path,
    workspace: &Path,
) -> Result<BTreeSet<String>> {
    let text = output_text(output).context("编辑工具没有成功回执")?;
    let mut files = BTreeSet::new();
    if patch {
        let report = text
            .trim()
            .strip_prefix("Success. Updated the following files:\n")
            .context("补丁未成功完成")?;
        for line in report.lines() {
            let path = line
                .strip_prefix("A ")
                .or_else(|| line.strip_prefix("M "))
                .or_else(|| line.strip_prefix("D "))
                .context("补丁回执格式无效")?;
            files.insert(relative_path(root, workspace, path)?);
        }
    } else {
        let summary = regex::Regex::new(
            r"^\(Complete: [1-9][0-9]* replacements? in ([1-9][0-9]*) files?\.\)$",
        )?;
        let count: usize = summary
            .captures(text.trim().lines().last().unwrap_or_default())
            .context("替换未完整完成")?[1]
            .parse()?;
        let line_pattern = regex::Regex::new(r"^(.+): [1-9][0-9]* replacements?$")?;
        for line in text.lines() {
            if let Some(captures) = line_pattern.captures(line) {
                files.insert(relative_path(root, workspace, &captures[1])?);
            }
        }
        ensure!(files.len() == count, "替换回执缺少完整文件列表");
    }
    Ok(files)
}

pub(super) fn completed_files(
    output: &Value,
    patch: bool,
    root: &Path,
    workspace: &Path,
    aggregate: bool,
    args: &Value,
) -> Result<BTreeSet<String>> {
    if let Ok(files) = reported_files(output, patch, root, workspace) {
        return Ok(files);
    }
    // 单次 await apply_patch 的原生执行器可能只返回空对象；保留成功执行信封并继续精确核对内容。
    // 任意脚本、错误信封、额外输出均不取得此权限。
    ensure!(aggregate && patch, "编辑工具没有可验证的成功回执");
    let text = output_text(output).context("编辑工具没有成功回执")?;
    let (header, body) = text
        .trim()
        .split_once("\nOutput:\n")
        .context("聚合补丁缺少成功执行回执")?;
    ensure!(
        header.starts_with("Script completed\nWall time ")
            && header.lines().count() == 2
            && body.trim() == "{}",
        "聚合补丁未成功完成或包含额外输出"
    );
    Ok(operations(root, workspace, true, args)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

pub(crate) fn observe(home: &Path, input: &Value) -> Result<()> {
    observe_with_runtime(
        home,
        input,
        std::env::var(crate::subagent_gate::RUNTIME_ID_ENV)
            .ok()
            .as_deref(),
    )
}

pub(super) fn observe_with_runtime(
    home: &Path,
    input: &Value,
    runtime: Option<&str>,
) -> Result<()> {
    let event = field(input, "hook_event_name", "hookEventName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(event, "PreToolUse" | "PostToolUse") {
        return Ok(());
    }
    let name = field(input, "tool_name", "toolName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = field(input, "tool_input", "toolInput").unwrap_or(&Value::Null);
    let Some((patch, args)) = normalized_input(name, args)? else {
        return Ok(());
    };
    let cwd = input
        .get("cwd")
        .or_else(|| input.get("working_dir"))
        .or_else(|| input.get("workingDirectory"))
        .and_then(Value::as_str)
        .context("文件记录缺少工作区")?;
    let workspace = Path::new(cwd).canonicalize()?;
    let (session, actor) = owner(home, input, &workspace, runtime)?;
    let root =
        PathBuf::from(git_text(&workspace, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
    let call = field(input, "tool_call_id", "toolCallId")
        .or_else(|| input.get("call_id"))
        .cloned()
        .unwrap_or(Value::Null);
    let token = digest(
        format!(
            "{session}:{actor}:{name}:{}:{call}:{args}",
            field(input, "turn_id", "turnId").context("编辑缺少轮次身份")?
        )
        .as_bytes(),
    );
    let path = ledger_path(home, &root);
    let _lock = lock_ledger(&path)?;
    let mut ledger = load(&path)?;
    if event == "PreToolUse" {
        let operations = operations(&root, &workspace, patch, &args)?;
        ensure!(
            operations.len() <= MAX_FILES && ledger.pending.len() < 128,
            "编辑范围过大或有过多未结束编辑，停止记录"
        );
        let head = git_text(&root, &["rev-parse", "--verify", "HEAD"])?;
        let mut candidates = BTreeMap::new();
        for (file, edit) in operations {
            let disk = disk_entry(&root, &file)?;
            let before = fingerprint(disk.as_ref());
            let expected = apply_edit(disk.as_ref(), &edit);
            let baseline = fingerprint(head_entry(&root, &head, &file)?.as_ref());
            let previous = ledger.files.get(&file);
            let owned = previous.is_some_and(|claim| {
                claim.valid
                    && claim.sessions == BTreeSet::from([session.clone()])
                    && claim.baseline == baseline
                    && claim.expected == before
            });
            let staged = !git(
                &root,
                &["diff", "--cached", "--name-only", "-z", "HEAD", "--", &file],
                None,
                None,
            )?
            .is_empty();
            let mut valid = !staged && (before == baseline || owned) && expected.is_ok();
            for pending in ledger.pending.values_mut() {
                if let Some(candidate) = pending.candidates.get_mut(&file) {
                    candidate.claim.valid = false;
                    valid = false;
                }
            }
            if candidates.contains_key(&file) {
                valid = false;
            }
            let mut sessions = BTreeSet::from([session.clone()]);
            if !valid && let Some(previous) = previous {
                sessions.extend(previous.sessions.iter().cloned());
            }
            candidates.insert(
                file,
                Candidate {
                    before,
                    claim: Claim {
                        sessions,
                        baseline,
                        expected: fingerprint(expected.ok().flatten().as_ref()),
                        valid,
                    },
                },
            );
        }
        if let Some(old) = ledger.pending.remove(&token) {
            for (file, mut candidate) in old.candidates {
                candidate.claim.valid = false;
                ledger.files.insert(file, candidate.claim);
            }
            for candidate in candidates.values_mut() {
                candidate.claim.valid = false;
            }
        }
        ledger.pending.insert(
            token,
            Pending {
                session,
                candidates,
                patch,
                aggregate: matches!(name, "functions.exec" | "exec"),
            },
        );
    } else if let Some(pending) = ledger.pending.remove(&token) {
        let reported = field(input, "tool_response", "toolResponse")
            .context("编辑缺少工具回执")
            .and_then(|output| {
                completed_files(
                    output,
                    pending.patch,
                    &root,
                    &workspace,
                    pending.aggregate,
                    &args,
                )
            });
        for (file, mut candidate) in pending.candidates {
            let current = disk_entry(&root, &file).map(|entry| fingerprint(entry.as_ref()));
            if current
                .as_ref()
                .is_ok_and(|current| *current == candidate.before)
            {
                // 未发生实际写入的候选不能覆盖已有归属，也不能取得提交权限。
                continue;
            }
            candidate.claim.valid &= current
                .is_ok_and(|current| current == candidate.claim.expected)
                && reported.as_ref().is_ok_and(|files| files.contains(&file));
            ledger.files.insert(file, candidate.claim);
        }
    }
    ensure!(ledger.files.len() <= 10_000, "文件归属记录过多，停止记录");
    crate::fs_util::atomic_write_private(&path, &serde_json::to_vec(&ledger)?)?;
    Ok(())
}

pub(super) fn dirty_paths(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
    head: &str,
) -> Result<Vec<String>> {
    dirty_paths_mode(
        home,
        session,
        root,
        workspace,
        head,
        false,
        &mut BTreeMap::new(),
    )
}

fn remember_display_heads(
    cache: &mut BTreeMap<String, Option<Entry>>,
    entries: BTreeMap<String, Option<Entry>>,
) {
    let bytes: usize = cache
        .values()
        .chain(entries.values())
        .flatten()
        .map(|entry| entry.bytes.len())
        .sum();
    // 同次查询的缓存最多保留一批最大文件内容，避免候选文件多时累积大量内存。
    if bytes > MAX_BYTES as usize * 8 {
        cache.clear();
    }
    cache.extend(entries);
}

fn load_display_heads(
    root: &Path,
    workspace: &Path,
    head: &str,
    paths: &[String],
    cache: &mut BTreeMap<String, Option<Entry>>,
) -> Result<()> {
    let missing: Vec<_> = paths
        .iter()
        .filter(|path| !cache.contains_key(*path))
        .cloned()
        .collect();
    let validated = missing.iter().try_for_each(|path| {
        relative_path(
            root,
            workspace,
            root.join(path).to_str().context("文件名编码不受支持")?,
        )
        .map(|_| ())
    });
    // 批次中后面的无效文件不能影响前面已可确认的改动；失败时按原顺序逐项读取。
    if validated.is_ok()
        && let Ok(entries) = head_entries(root, head, &missing)
    {
        remember_display_heads(cache, entries);
    }
    Ok(())
}

pub(super) fn has_dirty_files(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
    head: &str,
) -> Result<bool> {
    let mut heads = BTreeMap::new();
    // 执行记录已能确认当前改动时，按钮展示无需重新恢复整个会话。
    // 未找到有效候选再查询历史；完整提交范围仍由 changes 独立校验。
    {
        let path = ledger_path(home, root);
        let ledger = {
            let _lock = lock_ledger(&path)?;
            load(&path)?
        };
        let mut candidates: BTreeSet<_> = ledger
            .files
            .iter()
            .filter(|(_, claim)| claim.sessions.contains(session))
            .map(|(path, _)| path.clone())
            .collect();
        for pending in ledger
            .pending
            .values()
            .filter(|pending| pending.session == session)
        {
            candidates.extend(pending.candidates.keys().cloned());
        }
        ensure!(
            candidates.len() <= MAX_FILES,
            "当前对话文件过多，请拆分提交"
        );
        let candidates: Vec<_> = candidates.into_iter().collect();
        for paths in candidates.chunks(8) {
            load_display_heads(root, workspace, head, paths, &mut heads)?;
            for path in paths {
                relative_path(
                    root,
                    workspace,
                    root.join(path).to_str().context("文件名编码不受支持")?,
                )?;
                if !heads.contains_key(path) {
                    remember_display_heads(
                        &mut heads,
                        BTreeMap::from([(path.clone(), head_entry(root, head, path)?)]),
                    );
                }
                let before = heads[path].as_ref();
                if has_current_live_edit(&ledger, session, path, before)
                    && before != disk_entry(root, path)?.as_ref()
                {
                    return Ok(true);
                }
            }
        }
    }
    dirty_paths_mode(home, session, root, workspace, head, true, &mut heads)
        .map(|files| !files.is_empty())
}

fn dirty_paths_mode(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
    head: &str,
    display_only: bool,
    heads: &mut BTreeMap<String, Option<Entry>>,
) -> Result<Vec<String>> {
    let history = if display_only {
        history::recover_for_display(home, session, root, workspace)?
    } else {
        history::recover(home, session, root, workspace)?
    };
    let path = ledger_path(home, root);
    let lock = lock_ledger(&path)?;
    let ledger = load(&path)?;
    // 展示使用当前记录的快照，耗时的 Git 和磁盘读取无需阻塞新的编辑回执。
    let _lock = if display_only {
        drop(lock);
        None
    } else {
        Some(lock)
    };
    let mut candidates: BTreeSet<String> = ledger
        .files
        .iter()
        .filter(|(_, claim)| claim.sessions.contains(session))
        .map(|(path, _)| path.clone())
        .collect();
    candidates.extend(history.paths().cloned());
    for pending in ledger
        .pending
        .values()
        .filter(|pending| pending.session == session)
    {
        candidates.extend(pending.candidates.keys().cloned());
    }
    ensure!(
        candidates.len() <= MAX_FILES,
        "当前对话文件过多，请拆分提交"
    );
    let mut files = Vec::new();
    let candidates: Vec<_> = candidates.into_iter().collect();
    for paths in candidates.chunks(if display_only { 8 } else { 1 }) {
        if display_only {
            load_display_heads(root, workspace, head, paths, heads)?;
        }
        for path in paths {
            relative_path(
                root,
                workspace,
                root.join(path).to_str().context("文件名编码不受支持")?,
            )?;
            let uncached;
            let before = if display_only {
                if !heads.contains_key(path) {
                    remember_display_heads(
                        heads,
                        BTreeMap::from([(path.clone(), head_entry(root, head, path)?)]),
                    );
                }
                heads[path].as_ref()
            } else {
                uncached = head_entry(root, head, path)?;
                uncached.as_ref()
            };
            if before != disk_entry(root, path)?.as_ref()
                && (has_current_live_edit(&ledger, session, path, before)
                    || !history.changes_in_head(path, before))
            {
                files.push(path.clone());
                if display_only {
                    return Ok(files);
                }
            }
        }
    }
    if !display_only {
        ensure!(!files.is_empty(), "当前对话没有可确认的未提交文件改动");
    }
    Ok(files)
}

pub(super) fn changes(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
    head: &str,
) -> Result<Vec<Change>> {
    let history = history::recover(home, session, root, workspace)?;
    let path = ledger_path(home, root);
    let _lock = lock_ledger(&path)?;
    let ledger = load(&path)?;
    ensure!(
        !ledger
            .pending
            .values()
            .any(|pending| pending.session == session),
        "当前对话仍有编辑未完成或缺少完成回执，无法可靠确认文件范围"
    );
    history.validate_complete()?;
    let mut changes = Vec::new();
    let mut paths: BTreeSet<String> = ledger
        .files
        .iter()
        .filter(|(_, claim)| claim.sessions.contains(session))
        .map(|(path, _)| path.clone())
        .collect();
    paths.extend(history.paths().cloned());
    for path in &paths {
        relative_path(
            root,
            workspace,
            root.join(path).to_str().context("文件名编码不受支持")?,
        )?;
        let before = head_entry(root, head, path)?;
        let disk = disk_entry(root, path)?;
        if before == disk {
            continue;
        }
        if !has_current_live_edit(&ledger, session, path, before.as_ref())
            && history.changes_in_head(path, before.as_ref())
        {
            continue;
        }
        ensure!(
            !ledger
                .pending
                .values()
                .any(|pending| pending.candidates.contains_key(path)),
            "{path} 仍有编辑未完成或缺少完成回执，已停止提交"
        );
        let after = if let Some(claim) = ledger.files.get(path) {
            ensure!(
                before.is_some() && disk.is_some()
                    || claim.sessions == BTreeSet::from([session.to_string()]),
                "{path} 的新增或删除含其他对话的编辑记录，无法确认完整归属，已停止提交"
            );
            if claim.valid
                && claim.sessions == BTreeSet::from([session.to_string()])
                && claim.baseline == fingerprint(before.as_ref())
                && claim.expected == fingerprint(disk.as_ref())
            {
                disk.clone()
            } else if (claim.sessions == BTreeSet::from([session.to_string()])
                || claim.expected != fingerprint(disk.as_ref())
                || before.is_none()
                || disk.is_none())
                && history.replay(path, before.as_ref(), disk.as_ref()).is_ok()
            {
                // 本对话无效摘要或过时摘要只能由完整重放证明替代，其他对话的当前归属仍需分离。
                disk.clone()
            } else {
                let candidate = history
                    .isolate_changes(root, path, before.as_ref(), disk.as_ref())
                    .with_context(|| {
                        if claim.sessions != BTreeSet::from([session.to_string()]) {
                            format!("{path} 含其他对话的编辑记录，无法自动分离，已停止提交")
                        } else {
                            format!("{path} 的编辑基线或对话归属不明确，已停止提交")
                        }
                    })?;
                // 混合归属却没有可独立保留的剩余改动时，仍不能把整份文件认领为本对话。
                ensure!(
                    claim.sessions == BTreeSet::from([session.to_string()])
                        || Some(&candidate) != disk.as_ref(),
                    "{path} 含其他对话的编辑记录且无法确认独立改动，已停止提交"
                );
                Some(candidate)
            }
        } else {
            if before.is_none() || disk.is_none() {
                history.replay(path, before.as_ref(), disk.as_ref()).with_context(|| {
                    format!("{path} 的新增或删除缺少完整的成功记录，或后续内容与记录不一致，已停止提交")
                })?;
                changes.push(Change {
                    path: path.clone(),
                    before,
                    after: disk.clone(),
                    disk,
                });
                continue;
            }
            match history.replay(path, before.as_ref(), disk.as_ref()) {
                Ok(()) => disk.clone(),
                Err(error) => Some(
                    history
                        .isolate_changes(root, path, before.as_ref(), disk.as_ref())
                        .with_context(|| format!("{error:#}"))?,
                ),
            }
        };
        changes.push(Change {
            path: path.clone(),
            before,
            after,
            disk,
        });
    }
    ensure!(!changes.is_empty(), "当前对话没有可确认的未提交文件改动");
    ensure!(changes.len() <= MAX_FILES, "当前对话文件过多，请拆分提交");
    Ok(changes)
}
