//! 从本地对话的已完成工具记录恢复文件范围，不将历史记录写成新的执行前基线。
use super::*;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::time::SystemTime;

const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FAMILY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ACTORS: usize = 64;

#[derive(Clone)]
struct RecordedEdit {
    actor: String,
    start: Option<i64>,
    end: Option<i64>,
    ordinal: usize,
    edit: Edit,
    replacements: Option<usize>,
}

impl RecordedEdit {
    /// 原生补丁先按记录坐标套用，坐标不成立时才退回按内容唯一定位。
    /// 与 `verified_apply` 的严格模式相反，这里放宽的只是单个补丁的定位方式；
    /// 整条链仍须与 HEAD 和磁盘字节精确一致，共享文件的归属还要经过三方合并校验。
    fn replay_apply(&self, value: Option<&Entry>) -> Result<Option<Entry>> {
        ensure!(
            self.start
                .zip(self.end)
                .is_some_and(|(start, end)| start <= end),
            "编辑时间记录不完整"
        );
        let Edit::NativePatch(diff) = &self.edit else {
            return self.verified_apply(value);
        };
        let before = value.context("缺少原生编辑基线")?;
        let text = std::str::from_utf8(&before.bytes)?;
        let (result, inverse) = match apply_native_patch_checked(text, diff, false, false) {
            Ok(applied) => applied,
            Err(_) => apply_native_patch_checked(text, diff, false, true)?,
        };
        ensure!(
            apply_native_patch(&result, &inverse, false)?.as_bytes() == before.bytes,
            "原生编辑无法唯一还原基线"
        );
        Ok(Some(Entry {
            mode: before.mode.clone(),
            bytes: result.into_bytes(),
        }))
    }

    fn verified_apply(&self, value: Option<&Entry>) -> Result<Option<Entry>> {
        ensure!(
            self.start
                .zip(self.end)
                .is_some_and(|(start, end)| start <= end),
            "编辑时间记录不完整"
        );
        if let Edit::NativePatch(diff) = &self.edit {
            let before = value.context("缺少原生编辑基线")?;
            let (text, inverse) =
                apply_native_patch_checked(std::str::from_utf8(&before.bytes)?, diff, false, true)?;
            ensure!(
                apply_native_patch(&text, &inverse, false)?.as_bytes() == before.bytes,
                "原生编辑无法唯一还原基线"
            );
            return Ok(Some(Entry {
                mode: before.mode.clone(),
                bytes: text.into_bytes(),
            }));
        }
        let next = apply_edit(value, &self.edit)?;
        let reverse = match &self.edit {
            Edit::Patch(hunks) => Edit::Patch(
                hunks
                    .iter()
                    .map(|(old, new)| (new.clone(), old.clone()))
                    .collect(),
            ),
            Edit::Replace(args) => {
                ensure!(
                    args["literal"] == true,
                    "历史正则替换缺少执行前内容，不能自动认领"
                );
                let pattern = args["pattern"].as_str().context("缺少替换模式")?;
                let replacement = args["replacement"].as_str().context("缺少替换内容")?;
                ensure!(
                    !pattern.is_empty()
                        && !replacement.is_empty()
                        && args["case_insensitive"] != true,
                    "历史替换无法唯一还原执行前内容"
                );
                let old_text = std::str::from_utf8(&value.context("缺少替换基线")?.bytes)?;
                let new_text = std::str::from_utf8(&next.as_ref().context("缺少替换结果")?.bytes)?;
                ensure!(
                    Some(old_text.matches(pattern).count()) == self.replacements
                        && Some(new_text.matches(replacement).count()) == self.replacements,
                    "替换回执与历史基线不一致"
                );
                let mut reversed = args.clone();
                reversed["pattern"] = json!(replacement);
                reversed["replacement"] = json!(pattern);
                Edit::Replace(reversed)
            }
            Edit::NativePatch(diff) => {
                let current = next.as_ref().context("缺少原生编辑结果")?;
                let original =
                    apply_native_patch(std::str::from_utf8(&current.bytes)?, diff, true)?;
                ensure!(
                    value.is_some_and(|before| before.bytes == original.as_bytes()),
                    "原生编辑无法还原基线"
                );
                return Ok(next);
            }
            // 格式化是可重现的确定操作；最终候选仍须精确匹配完整磁盘内容或通过隔离校验。
            Edit::FormatRust { .. } => return Ok(next),
            // 创建内容和删除目标由成功回执限定，后续完整链仍须与 HEAD 和磁盘精确对应。
            Edit::Add(_) | Edit::Delete => return Ok(next),
        };
        ensure!(
            apply_edit(next.as_ref(), &reverse)?.as_ref() == value,
            "历史编辑无法唯一还原基线"
        );
        Ok(next)
    }
}

#[derive(Clone)]
struct Spawn {
    binding: String,
    role: String,
}

// 仅用于完整文件证明：保留全部可能位置，直到后续成功记录和完整磁盘内容消除歧义。
// 共享文件的分离仍使用 verified_apply，不能通过这里枚举出一部分提交内容。
fn full_replay_candidates(
    record: &RecordedEdit,
    value: Option<&Entry>,
    work: &mut usize,
) -> Result<Vec<Option<Entry>>> {
    *work += 1;
    ensure!(*work <= 4096, "历史完整重放超过安全计算上限，已停止提交");
    if let Ok(next) = record.verified_apply(value) {
        return Ok(vec![next]);
    }
    if record
        .start
        .zip(record.end)
        .is_none_or(|(start, end)| start > end)
    {
        return Ok(Vec::new());
    }
    let Some(before) = value else {
        return Ok(Vec::new());
    };
    if let Edit::Replace(args) = &record.edit {
        let Some(pattern) = args["pattern"].as_str() else {
            return Ok(Vec::new());
        };
        let pattern = if args["literal"] == true {
            regex::escape(pattern)
        } else {
            pattern.to_string()
        };
        let regex = regex::RegexBuilder::new(&pattern)
            .case_insensitive(args["case_insensitive"] == true)
            .dot_matches_new_line(args["dot_all"] == true)
            .build()?;
        let count = regex.find_iter(std::str::from_utf8(&before.bytes)?).count();
        if record.replacements != Some(count) || count == 0 {
            return Ok(Vec::new());
        }
        return Ok(apply_edit(value, &record.edit).ok().into_iter().collect());
    }
    let hunks = match &record.edit {
        Edit::NativePatch(diff) => parse_native_hunks(diff, false)?
            .into_iter()
            .map(|(_, _, old, new)| {
                (
                    old.into_iter().map(str::to_string).collect(),
                    new.into_iter().map(str::to_string).collect(),
                )
            })
            .collect(),
        Edit::Patch(hunks) => hunks.clone(),
        _ => return Ok(Vec::new()),
    };
    let text = std::str::from_utf8(&before.bytes)?;
    ensure!(
        !text.contains('\r') && (text.is_empty() || text.ends_with('\n')),
        "历史补丁换行无法精确确认"
    );
    let mut states = vec![(text.lines().map(str::to_string).collect::<Vec<_>>(), 0usize)];
    for (old, new) in hunks {
        let mut next = Vec::new();
        for (lines, cursor) in states {
            if old.is_empty() && !lines.is_empty() {
                continue;
            }
            *work += 1;
            ensure!(*work <= 4096, "历史完整重放超过安全计算上限，已停止提交");
            let positions: Vec<_> = (cursor..=lines.len())
                .filter(|start| {
                    *start + old.len() <= lines.len() && lines[*start..*start + old.len()] == old
                })
                .take(17)
                .collect();
            ensure!(
                positions.len() <= 16,
                "历史补丁候选位置过多，无法可靠确认归属，已停止提交"
            );
            for position in positions {
                let mut candidate = lines.clone();
                candidate.splice(position..position + old.len(), new.iter().cloned());
                let state = (candidate, position + new.len());
                if !next.contains(&state) {
                    next.push(state);
                }
                ensure!(next.len() <= 16, "历史补丁候选内容过多，已停止提交");
            }
        }
        states = next;
    }
    let mut entries = Vec::new();
    for (lines, _) in states {
        let text = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        ensure!(text.len() as u64 <= MAX_BYTES, "历史文件内容超过安全上限");
        let entry = Some(Entry {
            mode: before.mode.clone(),
            bytes: text.into_bytes(),
        });
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

type RecordedCall = (String, Value, Value, Option<i64>, Option<i64>, usize);

#[derive(Clone, Default)]
struct Transcript {
    meta: Value,
    calls: Vec<RecordedCall>,
    spawns: Vec<Spawn>,
    native_changes: Vec<(Value, i64, i64, usize)>,
    incomplete: bool,
    bytes: u64,
}

// 原生完成记录带精确行号；严格按坐标校验，避免把相同上下文中的其他位置认作本次编辑。
pub(super) fn apply_native_patch(input: &str, diff: &str, reverse: bool) -> Result<String> {
    apply_native_patch_at(input, diff, reverse, false)
}

fn apply_native_patch_at(input: &str, diff: &str, reverse: bool, relocate: bool) -> Result<String> {
    Ok(apply_native_patch_checked(input, diff, reverse, relocate)?.0)
}

type NativeHunk<'a> = (usize, usize, Vec<&'a str>, Vec<&'a str>);

fn parse_native_hunks(diff: &str, reverse: bool) -> Result<Vec<NativeHunk<'_>>> {
    let header = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?: .*)?$")?;
    let lines: Vec<_> = diff.lines().collect();
    let mut i = 0;
    let mut hunks = Vec::new();
    ensure!(!lines.is_empty(), "原生补丁缺少内容");
    while i < lines.len() {
        let captures = header.captures(lines[i]).context("原生补丁头无效")?;
        let old_start: usize = captures[1].parse()?;
        let old_count: usize = captures.get(2).map_or("1", |v| v.as_str()).parse()?;
        let new_start: usize = captures[3].parse()?;
        let new_count: usize = captures.get(4).map_or("1", |v| v.as_str()).parse()?;
        let mut old = Vec::new();
        let mut new = Vec::new();
        i += 1;
        while i < lines.len() && !lines[i].starts_with("@@") {
            let (prefix, text) = lines[i].split_at_checked(1).context("原生补丁含无效空行")?;
            match prefix {
                " " => {
                    old.push(text);
                    new.push(text);
                }
                "-" => old.push(text),
                "+" => new.push(text),
                _ => bail!("原生补丁含不支持的内容或换行标记"),
            }
            i += 1;
        }
        ensure!(
            old.len() == old_count && new.len() == new_count,
            "原生补丁行数不一致"
        );
        let position = |start: usize, count: usize| -> Result<usize> {
            if count == 0 {
                Ok(start)
            } else {
                start.checked_sub(1).context("原生补丁行号无效")
            }
        };
        let (from, to, removed, added) = if reverse {
            (
                position(new_start, new_count)?,
                position(old_start, old_count)?,
                new,
                old,
            )
        } else {
            (
                position(old_start, old_count)?,
                position(new_start, new_count)?,
                old,
                new,
            )
        };
        hunks.push((from, to, removed, added));
    }
    Ok(hunks)
}

fn apply_native_patch_checked(
    input: &str,
    diff: &str,
    reverse: bool,
    relocate: bool,
) -> Result<(String, String)> {
    ensure!(
        !input.contains('\r') && (input.is_empty() || input.ends_with('\n')),
        "原生补丁的换行格式无法精确确认"
    );
    let source: Vec<_> = input.lines().collect();
    let mut output = Vec::new();
    let mut inverse = String::new();
    let mut cursor = 0usize;
    for (mut from, to, removed, added) in parse_native_hunks(diff, reverse)? {
        if relocate {
            ensure!(!removed.is_empty(), "无上下文补丁不能按内容定位");
            let matches: Vec<_> = source
                .windows(removed.len())
                .enumerate()
                .filter_map(|(position, lines)| (lines == removed.as_slice()).then_some(position))
                .collect();
            ensure!(matches.len() == 1, "原生补丁上下文不能唯一定位");
            from = matches[0];
        }
        ensure!(
            from >= cursor && from <= source.len(),
            "原生补丁位置超出基线或相互重叠"
        );
        let end = from
            .checked_add(removed.len())
            .context("原生补丁范围过大")?;
        ensure!(
            source.get(from..end) == Some(removed.as_slice()),
            "原生补丁内容与当前基线不一致"
        );
        output.extend_from_slice(&source[cursor..from]);
        ensure!(relocate || output.len() == to, "原生补丁前后行号不一致");
        let new_position = output.len();
        let inverse_start = if added.is_empty() {
            new_position
        } else {
            new_position + 1
        };
        let original_start = if removed.is_empty() { from } else { from + 1 };
        inverse.push_str(&format!(
            "@@ -{inverse_start},{} +{original_start},{} @@\n",
            added.len(),
            removed.len()
        ));
        for line in &added {
            inverse.push('-');
            inverse.push_str(line);
            inverse.push('\n');
        }
        for line in &removed {
            inverse.push('+');
            inverse.push_str(line);
            inverse.push('\n');
        }
        output.extend(added);
        cursor = end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok((
        if output.is_empty() {
            String::new()
        } else {
            format!("{}\n", output.join("\n"))
        },
        inverse,
    ))
}

type Signature = Vec<(PathBuf, u64, SystemTime, String)>;
static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, (Signature, Transcript)>>> = OnceLock::new();

fn timestamp(record: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(record["timestamp"].as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn output(value: &Value) -> Value {
    // 新版 rollout 将工具输出存成文本块数组，旧版使用字符串。
    if value.is_array() {
        json!({"content": value})
    } else {
        value.clone()
    }
}

// 识别先执行一次补丁，再执行命令或收取命令结果的固定包装；不执行 JavaScript。
fn batch_patch(name: &str, args: &Value) -> Option<(Value, usize)> {
    if !matches!(name, "exec" | "functions.exec") {
        return None;
    }
    let code = args
        .as_str()
        .or_else(|| args["code"].as_str())
        .or_else(|| args["input"].as_str())?
        .trim();
    let code = if code.starts_with("// @exec:") {
        code.split_once('\n')?.1.trim()
    } else {
        code
    };
    let literal = code.strip_prefix("text(await tools.apply_patch(")?;
    let mut stream = serde_json::Deserializer::from_str(literal).into_iter::<String>();
    let patch = stream.next()?.ok()?;
    let mut rest = literal[stream.byte_offset()..].strip_prefix("));")?.trim();
    let string = r#""(?:[^"\\\r\n]|\\.)*""#;
    let value = format!(r"(?:{string}|-?\d+(?:\.\d+)?|true|false|null)");
    let field = format!(r"[a-zA-Z_][a-zA-Z0-9_]*\s*:\s*{value}");
    let call = regex::Regex::new(&format!(r"^text\(await tools\.(?:exec_command|write_stdin)\(\{{\s*(?:{field}(?:\s*,\s*{field})*\s*,?)?\s*\}}\)\);" )).ok()?;
    let mut count = 0;
    while !rest.is_empty() {
        let matched = call.find(rest)?;
        rest = rest[matched.end()..].trim();
        count += 1;
        if count > 8 {
            return None;
        }
    }
    (count > 0).then_some((json!(patch), count))
}

fn batch_patch_receipt(response: &Value, trailing: usize) -> Option<Value> {
    let content = response["content"].as_array()?;
    if content.len() != trailing + 2 {
        return None;
    }
    for block in &content[2..] {
        let text = block["text"].as_str()?;
        if !serde_json::from_str::<Value>(text).ok()?.is_object() {
            return None;
        }
    }
    Some(json!({"content": &content[..2]}))
}

fn multiple_patches(name: &str, args: &Value) -> Option<(Vec<Value>, usize)> {
    if !matches!(name, "exec" | "functions.exec") {
        return None;
    }
    let mut code = args
        .as_str()
        .or_else(|| args["code"].as_str())
        .or_else(|| args["input"].as_str())?
        .trim();
    if code.starts_with("// @exec:") {
        code = code.split_once('\n')?.1.trim();
    }
    let mut patches = Vec::new();
    while !code.is_empty() {
        if !code.starts_with("text(await tools.apply_patch(") {
            break;
        }
        let literal = code.strip_prefix("text(await tools.apply_patch(")?;
        let mut stream = serde_json::Deserializer::from_str(literal).into_iter::<String>();
        patches.push(json!(stream.next()?.ok()?));
        code = literal[stream.byte_offset()..].strip_prefix("));")?.trim();
        if patches.len() > 8 {
            return None;
        }
    }
    let trailing = if code.is_empty() {
        0
    } else {
        batch_patch(
            "exec",
            &json!(format!("text(await tools.apply_patch(\"\"));{code}")),
        )?
        .1
    };
    (patches.len() > 1).then_some((patches, trailing))
}

fn completed_multiple_patches(response: &Value, count: usize, trailing: usize) -> bool {
    if response["isError"] == true {
        return false;
    }
    let Some(content) = response["content"].as_array() else {
        return false;
    };
    content.len() == count + trailing + 1
        && content[0]["text"].as_str().is_some_and(|text| {
            text.starts_with("Script completed\n") && text.trim_end().ends_with("Output:")
        })
        && content[1..count + 1].iter().all(|block| {
            block["text"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                == Some(json!({}))
        })
        && content[count + 1..].iter().all(|block| {
            block["text"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .is_some_and(|value| value.is_object())
        })
}

#[test]
fn relocated_native_patch_rejects_repeated_context_even_with_unique_anchor() {
    let input = "prefix\nanchor\nrepeat\nuser\nrepeat\n";
    let diff = "@@ -1 +1 @@\n-anchor\n+own-anchor\n@@ -2 +2 @@\n-repeat\n+own-repeat\n";
    assert!(apply_native_patch_checked(input, diff, false, true).is_err());
}

#[test]
fn relocated_native_deletion_reverses_using_resolved_coordinates() {
    let input = "prefix\nunique\nrepeat\nrepeat\n";
    let diff = "@@ -1 +0,0 @@\n-unique\n";
    let (after, inverse) = apply_native_patch_checked(input, diff, false, true).unwrap();
    assert_eq!(after, "prefix\nrepeat\nrepeat\n");
    assert_eq!(apply_native_patch(&after, &inverse, false).unwrap(), input);
}

fn retain_input_context(
    history: &mut History,
    actor: &str,
    scopes: &[(i64, i64, BTreeSet<String>)],
    start: Option<i64>,
    end: Option<i64>,
    edits: &[(String, Edit)],
) -> Result<bool> {
    let Some((start, end)) = start.zip(end) else {
        return Ok(false);
    };
    let reported: BTreeSet<_> = edits.iter().map(|(file, _)| file.clone()).collect();
    let matches: Vec<_> = scopes
        .iter()
        .filter(|(from, to, files)| start <= *from && *to <= end && *files == reported)
        .collect();
    if matches.len() != 1 {
        return Ok(false);
    }
    let Some((from, to, _)) = matches.first() else {
        return Ok(false);
    };
    for (file, edit) in edits {
        if matches!(edit, Edit::Patch(_)) {
            if let Edit::Patch(hunks) = edit
                && hunks
                    .iter()
                    .any(|(old, new)| old == new || old.len() < 2 || new.len() < 2)
            {
                continue;
            }
            let records = history
                .files
                .get_mut(file)
                .context("原生编辑缺少文件记录")?;
            let records: Vec<_> = records
                .iter_mut()
                .filter(|record| {
                    record.actor == actor && record.start == Some(*from) && record.end == Some(*to)
                })
                .collect();
            ensure!(records.len() == 1, "原生编辑记录不能唯一对应补丁输入");
            let record = records.into_iter().next().unwrap();
            // 原生回执可能缩短上下文；保留成功请求中的完整上下文并继续验证正反向结果。
            record.edit = edit.clone();
        }
    }
    Ok(true)
}

// 只解析固定字面量调用，历史脚本本身永远不会被执行。
fn formatter_calls(name: &str, args: &Value) -> Option<(Vec<Option<Value>>, usize)> {
    if matches!(name, "exec_command" | "functions.exec_command") {
        return Some((vec![Some(args.clone())], 0));
    }
    if !matches!(name, "exec" | "functions.exec") {
        return None;
    }
    let mut code = args
        .as_str()
        .or_else(|| args["code"].as_str())
        .or_else(|| args["input"].as_str())?
        .trim();
    if !code.contains("rustfmt ") && !code.contains("cargo fmt ") && !code.contains("write_stdin") {
        return None;
    }
    if code.starts_with("// @exec:") {
        code = code.split_once('\n')?.1.trim();
    }
    let mut offset = 1;
    while let Some(literal) = code.strip_prefix("text(await tools.apply_patch(") {
        let mut stream = serde_json::Deserializer::from_str(literal).into_iter::<String>();
        stream.next()?.ok()?;
        code = literal[stream.byte_offset()..].strip_prefix("));")?.trim();
        offset += 1;
        if offset > 9 {
            return None;
        }
    }
    let string = r#""(?:[^"\\\r\n]|\\.)*""#;
    let primitive = format!(r"(?:{string}|-?\d+(?:\.\d+)?|true|false|null)");
    let field = format!(r"(?:[a-zA-Z_][a-zA-Z0-9_]*|{string})\s*:\s*{primitive}");
    let call = regex::Regex::new(&format!(r"^text\(await tools\.(exec_command|write_stdin)\(\{{\s*((?:{field}(?:\s*,\s*{field})*\s*,?)?)\s*\}}\)\);")).ok()?;
    let fields = regex::Regex::new(&format!(
        r"([a-zA-Z_][a-zA-Z0-9_]*|{string})\s*:\s*({primitive})"
    ))
    .ok()?;
    let mut calls = Vec::new();
    while !code.is_empty() {
        // 原生补丁可与只读检查、格式化调用交错；保留回执位置，避免遗漏后续格式化。
        if let Some(literal) = code.strip_prefix("text(await tools.apply_patch(") {
            let mut stream = serde_json::Deserializer::from_str(literal).into_iter::<String>();
            stream.next()?.ok()?;
            code = literal[stream.byte_offset()..].strip_prefix("));")?.trim();
            calls.push(None);
            if calls.len() > 8 {
                return None;
            }
            continue;
        }
        let captures = call.captures(code)?;
        let mut object = serde_json::Map::new();
        for field in fields.captures_iter(&captures[2]) {
            let key = if field[1].starts_with('"') {
                serde_json::from_str::<String>(&field[1]).ok()?
            } else {
                field[1].to_string()
            };
            let value = serde_json::from_str::<Value>(&field[2]).ok()?;
            if key == "__codey_poll" {
                return None;
            }
            if object.insert(key, value).is_some() {
                return None;
            }
        }
        if captures[1] == *"write_stdin" {
            object.insert("__codey_poll".into(), Value::Bool(true));
        }
        calls.push(Some(Value::Object(object)));
        code = code[captures.get(0)?.end()..].trim();
        if calls.len() > 8 {
            return None;
        }
    }
    (!calls.is_empty()).then_some((calls, offset))
}

fn has_formatter(name: &str, args: &Value) -> bool {
    formatter_calls(name, args).is_some_and(|(calls, _)| {
        calls.iter().flatten().any(|args| {
            args["cmd"]
                .as_str()
                .is_some_and(|cmd| cmd.starts_with("rustfmt "))
        })
    })
}

fn cargo_formatter(name: &str, args: &Value) -> bool {
    if args["tty"] == true || args.get("shell").is_some_and(|value| !value.is_null()) {
        return false;
    }
    if !matches!(name, "exec_command" | "functions.exec_command") {
        return false;
    }
    let Some(command) = args["cmd"].as_str() else {
        return false;
    };
    let parts: Vec<_> = command.split(" && ").collect();
    matches!(
        parts.first(),
        Some(&"cargo fmt -p codey" | &"cargo fmt --all")
    ) && parts.iter().skip(1).all(|part| {
        *part == "cargo fmt -p codey -- --check" || *part == "echo FORMAT_OK" || {
            let command = part.split(" 2>&1 | tail -").collect::<Vec<_>>();
            command.len() <= 2
                && command
                    .get(1)
                    .is_none_or(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
                && (command[0].starts_with("cargo test -p codey")
                    || command[0].starts_with("cargo clippy -p codey"))
                && command[0]
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b" _:-".contains(&c))
        }
    })
}

// 固定顺序的原生终端调用可以拆开核对；不接受变量、任意脚本或修改文件的附加命令。
fn wrapped_cargo_calls(name: &str, args: &Value) -> Option<Vec<(String, Value)>> {
    if !matches!(name, "exec" | "functions.exec") {
        return None;
    }
    let (calls, offset) = formatter_calls(name, args)?;
    if offset != 1 {
        return None;
    }
    let mut result = Vec::new();
    for args in calls {
        let mut args = args?;
        let name = if args["__codey_poll"] == true {
            args.as_object_mut()?.remove("__codey_poll");
            if args["session_id"].as_u64().is_none()
                || args["chars"].as_str().is_some_and(|s| !s.is_empty())
            {
                return None;
            }
            "write_stdin"
        } else {
            if args["tty"] == true || args.get("shell").is_some_and(|v| !v.is_null()) {
                return None;
            }
            if !cargo_formatter("exec_command", &args)
                && !matches!(
                    args["cmd"].as_str(),
                    Some(
                        "cargo fmt --all -- --check"
                            | "cargo fmt -p codey -- --check"
                            | "git diff --check"
                            | "git diff --stat"
                    )
                )
            {
                return None;
            }
            "exec_command"
        };
        result.push((name.into(), args));
    }
    Some(result)
}

fn wrapped_cargo_receipts(
    name: &str,
    args: &Value,
    response: &Value,
) -> Option<Vec<(String, Value, Value)>> {
    let calls = wrapped_cargo_calls(name, args)?;
    if response["isError"] == true || response.get("error").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let content = response["content"].as_array()?;
    if content.len() != calls.len() + 1 {
        return None;
    }
    let envelope = content[0]["text"].as_str()?;
    if !envelope.starts_with("Script completed\n") || !envelope.trim_end().ends_with("Output:") {
        return None;
    }
    calls
        .into_iter()
        .zip(&content[1..])
        .map(|((name, args), block)| {
            let receipt: Value = serde_json::from_str(block["text"].as_str()?).ok()?;
            if !receipt.is_object() {
                return None;
            }
            Some((name, args, receipt))
        })
        .collect()
}

fn terminal_receipt(response: &Value) -> Option<Value> {
    if response.get("exit_code").is_some() || response["session_id"].as_u64().is_some() {
        return Some(response.clone());
    }
    let text = tracking::output_text(response)?;
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        return Some(value);
    }
    let header = text.split_once("\nOutput:\n")?.0;
    if !header.starts_with("Chunk ID: ") {
        return None;
    }
    for line in header.lines() {
        if let Some(code) = line.strip_prefix("Process exited with code ") {
            return Some(json!({"exit_code": code.parse::<i32>().ok()?}));
        }
        if let Some(session) = line.strip_prefix("Process running with session ID ") {
            return Some(json!({"session_id": session.parse::<u64>().ok()?}));
        }
    }
    None
}

fn completed_formatters(
    name: &str,
    args: &Value,
    response: &Value,
    root: &Path,
    workspace: &Path,
) -> Result<Vec<(String, Edit)>> {
    if response["isError"] == true || response.get("error").is_some_and(|value| !value.is_null()) {
        return Ok(Vec::new());
    }
    let Some((calls, offset)) = formatter_calls(name, args) else {
        return Ok(Vec::new());
    };
    let wrapped = matches!(name, "exec" | "functions.exec");
    let receipts = if wrapped {
        let Some(content) = response["content"].as_array() else {
            return Ok(Vec::new());
        };
        if content.len() != calls.len() + offset {
            return Ok(Vec::new());
        }
        let envelope = content[0]["text"].as_str().unwrap_or_default();
        if !envelope.starts_with("Script completed\n") || !envelope.trim_end().ends_with("Output:")
        {
            return Ok(Vec::new());
        }
        let mut receipts = Vec::new();
        for block in &content[offset..] {
            let Some(text) = block["text"].as_str() else {
                return Ok(Vec::new());
            };
            let Ok(value) = serde_json::from_str::<Value>(text) else {
                return Ok(Vec::new());
            };
            if !value.is_object() {
                return Ok(Vec::new());
            }
            receipts.push(value);
        }
        receipts
    } else {
        let value = if response.is_object() && response.get("exit_code").is_some() {
            response.clone()
        } else {
            match tracking::output_text(response)
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            {
                Some(value) => value,
                None => return Ok(Vec::new()),
            }
        };
        vec![value]
    };
    let mut edits = Vec::new();
    for (args, receipt) in calls.iter().zip(receipts) {
        let Some(args) = args else {
            continue;
        };
        let Some(command) = args["cmd"].as_str() else {
            continue;
        };
        if receipt["exit_code"] != 0
            || receipt
                .get("session_id")
                .is_some_and(|value| !value.is_null())
            || receipt["isError"] == true
            || receipt.get("error").is_some_and(|value| !value.is_null())
        {
            continue;
        }
        let tokens: Vec<_> = command.split_whitespace().collect();
        if tokens.len() < 4
            || tokens[0] != "rustfmt"
            || tokens[1] != "--edition"
            || !matches!(tokens[2], "2015" | "2018" | "2021" | "2024")
        {
            continue;
        }
        if let Some(workdir) = args["workdir"].as_str()
            && Path::new(workdir).canonicalize().ok().as_deref() != Some(workspace)
        {
            continue;
        }
        if tokens.len() - 3 > MAX_FILES
            || tokens[3..].iter().any(|path| {
                !path.ends_with(".rs")
                    || path.starts_with('-')
                    || !path
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_./:-".contains(&byte))
            })
        {
            continue;
        }
        let mut paths = BTreeSet::new();
        for raw in &tokens[3..] {
            paths.insert(relative_path(root, workspace, raw)?);
        }
        for path in paths {
            edits.push((
                path,
                Edit::FormatRust {
                    root: root.to_path_buf(),
                    edition: tokens[2].into(),
                },
            ));
        }
    }
    Ok(edits)
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn accepts_only_fixed_sequential_patch_wrapper_and_ordered_receipts() {
        let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch";
        let code = format!(
            "text(await tools.apply_patch({}));\ntext(await tools.exec_command({{cmd:\"node --test test.mjs\",workdir:\"/tmp/repo\",max_output_tokens:2000,yield_time_ms:1000}}));\ntext(await tools.write_stdin({{session_id:71171,chars:\"\",max_output_tokens:2200,yield_time_ms:1000}}));",
            serde_json::to_string(patch).unwrap()
        );
        let found = batch_patch("exec", &json!(code.clone())).expect("明确包装可识别");
        assert_eq!(found, (json!(patch), 2));
        assert!(batch_patch("exec", &json!(format!("if (true) {{ {code} }}"))).is_none());
        assert!(batch_patch("exec", &json!(format!("{code}\ntext({{}});"))).is_none());
        let response = json!({"content":[{"type":"text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":"{}"},{"type":"text","text":"{\"exit_code\":0}"},{"type":"text","text":"{\"exit_code\":0}"}]});
        assert!(batch_patch_receipt(&response, 2).is_some());
        assert!(batch_patch_receipt(&response, 1).is_none());
    }
}

fn segment_paths(home: &Path, anchor: &Path, id: &str) -> Result<Vec<PathBuf>> {
    // 仅枚举文件名，不读取其他对话的正文。文件名匹配后仍须校验内部身份。
    let pattern = regex::Regex::new(&format!(
        r"^rollout-(?:\d{{4}}-\d{{2}}-\d{{2}}T\d{{2}}-\d{{2}}-\d{{2}}-)?{}(?:_[0-9a-f-]{{36}})?\.jsonl$",
        regex::escape(id)
    ))?;
    let mut paths = BTreeSet::from([anchor.to_path_buf()]);
    let mut queue = vec![(home.join("sessions"), 0)];
    let mut entries = 0;
    while let Some((directory, depth)) = queue.pop() {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= 100_000, "本地对话目录过大，无法完整核对历史分段");
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && depth < 3 && name.chars().all(|c| c.is_ascii_digit()) {
                queue.push((entry.path(), depth + 1));
            } else if kind.is_file() && pattern.is_match(&name) {
                paths.insert(crate::session_transfer::checked_rollout_path(
                    home,
                    &entry.path(),
                )?);
                ensure!(paths.len() <= 32, "同一对话的历史分段过多，无法完整校验");
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn read_transcript(home: &Path, raw: &Path) -> Result<Transcript> {
    read_transcript_mode(home, raw, false)
}

fn read_transcript_mode(home: &Path, raw: &Path, display_only: bool) -> Result<Transcript> {
    let path = crate::session_transfer::checked_rollout_path(home, raw)?;
    ensure!(
        !path.components().any(|part| part.as_os_str() == "imported"),
        "导入对话的历史编辑缺少本机执行基线，无法自动认领"
    );
    let anchor_meta = tracking::metadata(home, &path)?;
    let id = anchor_meta["id"].as_str().context("对话分段缺少身份")?;
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "对话分段身份无效");
    let mut segments = Vec::new();
    let mut signature = Vec::new();
    let mut total_bytes = 0;
    let paths = segment_paths(home, &path, id)?;
    let segmented = paths.len() > 1;
    for segment in paths {
        let metadata = fs::metadata(&segment)?;
        ensure!(
            metadata.len() <= MAX_TRANSCRIPT_BYTES,
            "对话历史过大，无法完整校验编辑范围"
        );
        total_bytes += metadata.len();
        ensure!(
            total_bytes <= MAX_FAMILY_BYTES,
            "对话历史分段过大，无法完整校验"
        );
        let meta = tracking::metadata(home, &segment)?;
        ensure!(
            [
                "id",
                "cwd",
                "source",
                "forked_from_id",
                "agent_role",
                "agent_path",
                "subagent_history_start_ordinal",
                "creator_user_id",
                "creator_account_id"
            ]
            .iter()
            .all(|key| meta[*key] == anchor_meta[*key]),
            "同一对话历史分段的身份、工作区或继承边界不一致"
        );
        let created = match meta["timestamp"].as_str() {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)?.timestamp_millis(),
            None if !segmented => 0,
            None => bail!("对话分段缺少创建时间"),
        };
        signature.push((
            segment.clone(),
            metadata.len(),
            metadata.modified()?,
            if display_only {
                String::new()
            } else {
                digest(&read_bounded(&segment, MAX_TRANSCRIPT_BYTES)?)
            },
        ));
        segments.push((created, segment));
    }
    segments.sort();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some((old, transcript)) = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("历史缓存不可用"))?
        .get(&path)
        && (*old == signature
            || (display_only
                && old.len() == signature.len()
                && old
                    .iter()
                    .zip(&signature)
                    .all(|(old, new)| old.0 == new.0 && old.1 == new.1 && old.2 == new.2)))
    {
        return Ok(transcript.clone());
    }
    // 展示缓存仅依据元数据复用已经完整解析的历史；缓存缺失时完整读取。
    // 预览和提交始终走内容摘要校验，不以展示缓存作为文件归属证明。
    if display_only {
        return read_transcript(home, raw);
    }
    let mut result = Transcript {
        bytes: total_bytes,
        ..Default::default()
    };
    let mut calls = HashMap::<String, (String, Value, Option<i64>, usize)>::new();
    let mut seen = BTreeSet::new();
    let mut native_seen = BTreeSet::new();
    let mut captured = 0usize;
    let mut records = Vec::new();
    let segmented = segments.len() > 1;
    let mut boundaries = Vec::new();
    for (_, segment) in &segments {
        let mut lines = BufReader::new(File::open(segment)?).lines();
        let meta: Value = serde_json::from_str(&lines.next().context("对话分段为空")??)?;
        if segmented {
            boundaries.push(
                meta["ordinal"]
                    .as_u64()
                    .context("对话历史分段缺少连续序号")?,
            );
        }
    }
    ensure!(
        boundaries.windows(2).all(|pair| pair[0] < pair[1]),
        "对话历史分段顺序不明确"
    );
    let mut previous = None;
    for (index, (_, segment)) in segments.iter().enumerate() {
        for (line_index, line) in BufReader::new(File::open(segment)?).lines().enumerate() {
            let line = line?;
            ensure!(line.len() <= 8 * 1024 * 1024, "单条对话记录过大");
            let record: Value =
                serde_json::from_str(&line).context("对话编辑记录尚未完整保存，请稍后重试")?;
            if line_index == 0 {
                ensure!(record["type"] == "session_meta", "对话分段缺少可信身份记录");
                if index == 0 {
                    records.push(record);
                }
                continue;
            }
            if segmented {
                let position = record["ordinal"]
                    .as_u64()
                    .context("对话历史分段记录缺少连续序号")?;
                // 恢复点之后属于新分段；旧进程的尾部记录不属于恢复后的编辑链。
                if boundaries
                    .get(index + 1)
                    .is_some_and(|end| position >= *end)
                {
                    continue;
                }
                ensure!(
                    position >= boundaries[index] && previous.is_none_or(|old| old < position),
                    "对话历史分段的记录顺序不明确"
                );
                previous = Some(position);
            }
            records.push(record);
        }
    }
    let mut ordinal = 0usize;
    for record in records {
        let payload = &record["payload"];
        if ordinal == 0 {
            ensure!(record["type"] == "session_meta", "对话缺少可信身份记录");
            result.meta = payload.clone();
            ensure!(
                result.meta.get("forked_from_id").is_none_or(Value::is_null)
                    || result.meta["subagent_history_start_ordinal"]
                        .as_u64()
                        .is_some(),
                "分叉对话缺少历史边界，无法将继承的编辑归入当前对话"
            );
        }
        ordinal += 1;
        let native_file_change = record["type"] == "event_msg"
            && payload["type"] == "item_completed"
            && payload["item"]["type"] == "FileChange";
        if (record["type"] == "response_item" || native_file_change)
            && let Some(start) = result.meta["subagent_history_start_ordinal"].as_u64()
        {
            let position = record["ordinal"]
                .as_u64()
                .context("子代理历史缺少继承边界序号")?;
            if position < start {
                continue;
            }
        }
        if native_file_change {
            let item = &payload["item"];
            if item["status"] != "completed" {
                continue;
            }
            ensure!(
                payload["thread_id"] == result.meta["id"],
                "原生编辑记录的对话身份不匹配"
            );
            let id = item["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("原生编辑记录缺少身份")?;
            ensure!(native_seen.insert(id.to_string()), "原生编辑记录身份重复");
            let start = payload["started_at_ms"]
                .as_i64()
                .context("原生编辑记录缺少开始时间")?;
            let end = payload["completed_at_ms"]
                .as_i64()
                .context("原生编辑记录缺少结束时间")?;
            ensure!(start <= end, "原生编辑时间无效");
            captured += item.to_string().len();
            ensure!(
                captured <= 16 * 1024 * 1024 && result.native_changes.len() < 4096,
                "对话编辑记录过多"
            );
            result
                .native_changes
                .push((item.clone(), start, end, ordinal));
            continue;
        }
        if record["type"] != "response_item" {
            continue;
        }
        let kind = payload["type"].as_str().unwrap_or_default();
        let Some(id) = payload["call_id"].as_str() else {
            continue;
        };
        if matches!(kind, "function_call" | "custom_tool_call") {
            let raw_name = payload["name"].as_str().unwrap_or_default();
            let name = match payload["namespace"].as_str() {
                Some(namespace) => format!("{namespace}.{raw_name}"),
                None => raw_name.into(),
            };
            let args = if kind == "custom_tool_call" {
                payload["input"].clone()
            } else if let Some(text) = payload["arguments"].as_str() {
                serde_json::from_str(text).context("历史工具参数无效")?
            } else {
                payload["arguments"].clone()
            };
            if name == "agents.spawn_agent"
                || tracking::normalized_input(&name, &args)
                    .ok()
                    .flatten()
                    .is_some()
                || batch_patch(&name, &args).is_some()
                || multiple_patches(&name, &args).is_some()
                || has_formatter(&name, &args)
                || cargo_formatter(&name, &args)
                || wrapped_cargo_calls(&name, &args).is_some()
                || matches!(name.as_str(), "write_stdin" | "functions.write_stdin")
            {
                ensure!(
                    seen.insert(id.to_string()),
                    "对话中出现重复工具调用身份，无法确认编辑归属"
                );
                captured += args.to_string().len();
                ensure!(
                    captured <= 16 * 1024 * 1024 && calls.len() < 4096,
                    "对话编辑记录过多"
                );
                calls.insert(id.into(), (name, args, timestamp(&record), ordinal));
            }
        } else if matches!(kind, "function_call_output" | "custom_tool_call_output")
            && let Some((name, args, start, order)) = calls.remove(id)
        {
            let response = output(&payload["output"]);
            if name == "agents.spawn_agent" {
                let value = if let Some(text) = response.as_str() {
                    serde_json::from_str::<Value>(text).ok()
                } else if response.get("content").is_none() {
                    Some(response.clone())
                } else {
                    tracking::output_text(&response)
                        .and_then(|text| serde_json::from_str(&text).ok())
                };
                if let Some(value) = value
                    && value["isError"] != true
                    && value.get("error").is_none_or(Value::is_null)
                    && !matches!(value["status"].as_str(), Some("failed" | "error"))
                    && let Some(binding) = value["agent_id"]
                        .as_str()
                        .or_else(|| value["task_name"].as_str())
                {
                    result.spawns.push(Spawn {
                        binding: binding.into(),
                        role: args["agent_type"].as_str().unwrap_or("default").into(),
                    });
                }
            } else {
                result
                    .calls
                    .push((name, args, response, start, timestamp(&record), order));
            }
        }
    }
    result.incomplete = calls.values().any(|(name, _, _, _)| {
        !matches!(
            name.as_str(),
            "agents.spawn_agent" | "write_stdin" | "functions.write_stdin"
        )
    });
    for (segment, length, modified, hash) in &signature {
        let current = fs::metadata(segment)?;
        ensure!(
            current.len() == *length
                && current.modified()? == *modified
                && digest(&read_bounded(segment, MAX_TRANSCRIPT_BYTES)?) == *hash,
            "读取期间对话记录发生变化，请重试"
        );
    }
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("历史缓存不可用"))?;
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(path, (signature, result.clone()));
    Ok(result)
}

fn child_paths(home: &Path, parent: &str, workspace: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut result = BTreeMap::new();
    for database in codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        if !database.exists() {
            continue;
        }
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let columns = crate::sqlite_util::table_columns(&connection, "threads")?;
        // 发现列表也可能包含日志库等辅助数据库，与主会话查询保持一致。
        if columns.is_empty() {
            continue;
        }
        let has_source = columns.iter().any(|column| column == "source");
        if !has_source {
            let count: usize =
                connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
            ensure!(
                count <= MAX_ACTORS,
                "早期会话库缺少子代理父关系且记录过多，无法完整确认历史范围"
            );
        }
        let query = if has_source {
            "SELECT id, rollout_path, cwd FROM threads WHERE json_valid(source) AND json_extract(source, '$.subagent.thread_spawn.parent_thread_id')=?1 LIMIT 65"
        } else {
            // 兼容早期会话库；只读取身份行，不能按文件名猜测子代理。
            "SELECT id, rollout_path, cwd FROM threads WHERE ?1 IS NOT NULL LIMIT 65"
        };
        let mut statement = connection.prepare(query)?;
        for row in statement.query_map([parent], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (id, path, cwd) = row?;
            if Path::new(&cwd).canonicalize().ok().as_deref() != Some(workspace) {
                continue;
            }
            let path = home.join(path);
            let meta = tracking::metadata(home, &path)?;
            if meta["parent_thread_id"] == parent {
                result.insert(id, path);
            }
        }
    }
    ensure!(
        result.len() < MAX_ACTORS,
        "历史子代理过多，无法完整校验范围"
    );
    Ok(result.into_iter().collect())
}

#[derive(Default)]
pub(super) struct History {
    files: BTreeMap<String, Vec<RecordedEdit>>,
    incomplete: bool,
}

impl History {
    #[cfg(test)]
    pub(super) fn diagnostic_replay(&self, path: &str, before: Option<&Entry>) -> Vec<String> {
        let Some(records) = self.files.get(path) else {
            return Vec::new();
        };
        let mut records = records.clone();
        records.sort_by_key(|record| (record.start, record.ordinal));
        let mut result = Vec::new();
        for start in records.len().saturating_sub(12)..records.len() {
            let mut value = before.cloned();
            let mut error = None;
            for record in &records[start..] {
                match record.verified_apply(value.as_ref()) {
                    Ok(next) => value = next,
                    Err(e) => {
                        error = Some(format!("{}: {e:#}", record.ordinal));
                        break;
                    }
                }
            }
            result.push(format!(
                "{} -> {}",
                records[start].ordinal,
                error.unwrap_or_else(|| format!(
                    "成功 {}",
                    value.map_or_else(|| "删除".into(), |entry| digest(&entry.bytes))
                ))
            ));
        }
        result
    }
    pub(super) fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }

    pub(super) fn changes_in_head(&self, path: &str, head: Option<&Entry>) -> bool {
        let Some(head) = head else {
            return false;
        };
        let Some(records) = self.files.get(path) else {
            return false;
        };
        // 新建文件的完整创建链已进入 HEAD 后，不再认领随后无记录的磁盘改动。
        if records
            .iter()
            .any(|record| matches!(record.edit, Edit::Add(_)))
        {
            return self.replay(path, None, Some(head)).is_ok();
        }
        if records.is_empty()
            || !records.iter().all(|record| {
                matches!(
                    record.edit,
                    Edit::NativePatch(_) | Edit::Patch(_) | Edit::Replace(_)
                )
            })
        {
            return false;
        }
        let mut records: Vec<_> = records.iter().collect();
        records.sort_by_key(|record| (record.start, record.ordinal));
        let mut value = head.clone();
        for record in records.iter().rev() {
            if let Edit::NativePatch(diff) = &record.edit {
                let result = (|| -> Result<Entry> {
                    ensure!(
                        record
                            .start
                            .zip(record.end)
                            .is_some_and(|(start, end)| start <= end),
                        "编辑时间记录不完整"
                    );
                    let previous = apply_native_patch_at(
                        std::str::from_utf8(&value.bytes)?,
                        diff,
                        true,
                        true,
                    )?;
                    ensure!(
                        apply_native_patch_at(&previous, diff, false, true)?.as_bytes()
                            == value.bytes,
                        "原生补丁不能唯一还原"
                    );
                    Ok(Entry {
                        bytes: previous.into_bytes(),
                        mode: value.mode.clone(),
                    })
                })();
                match result {
                    Ok(previous) => {
                        value = previous;
                        continue;
                    }
                    Err(_) => return false,
                }
            }
            let previous = (|| -> Result<Entry> {
                let edit = match &record.edit {
                    Edit::NativePatch(diff) => {
                        let mut previous = value.clone();
                        previous.bytes =
                            apply_native_patch(std::str::from_utf8(&value.bytes)?, diff, true)?
                                .into_bytes();
                        return Ok(previous);
                    }
                    Edit::Patch(hunks) => Edit::Patch(
                        hunks
                            .iter()
                            .map(|(old, new)| (new.clone(), old.clone()))
                            .collect(),
                    ),
                    Edit::Replace(args) => {
                        let mut reverse = args.clone();
                        reverse["pattern"] = args["replacement"].clone();
                        reverse["replacement"] = args["pattern"].clone();
                        Edit::Replace(reverse)
                    }
                    _ => bail!("编辑缺少可逆基线"),
                };
                apply_edit(Some(&value), &edit)?.context("编辑缺少可逆基线")
            })();
            let Ok(previous) = previous else {
                return false;
            };
            if record
                .verified_apply(Some(&previous))
                .ok()
                .flatten()
                .as_ref()
                != Some(&value)
            {
                return false;
            }
            value = previous;
        }
        true
    }

    pub(super) fn validate_complete(&self) -> Result<()> {
        ensure!(
            !self.incomplete,
            "当前对话历史仍有编辑未完成或缺少工具回执，已停止提交"
        );
        Ok(())
    }

    pub(super) fn replay(
        &self,
        path: &str,
        before: Option<&Entry>,
        after: Option<&Entry>,
    ) -> Result<()> {
        let records = self.files.get(path).context("文件没有可验证的历史编辑")?;
        let mut ordered = records.clone();
        ordered.sort_by_key(|record| (record.start, record.ordinal));
        for pair in ordered.windows(2) {
            if pair[0].actor != pair[1].actor {
                ensure!(
                    pair[0]
                        .end
                        .zip(pair[1].start)
                        .is_some_and(|(end, start)| end < start),
                    "{path} 的主代理与子代理编辑顺序不明确，已停止提交"
                );
            }
        }
        // 允许已提交的早期编辑留在历史中，只接受能唯一对应当前 HEAD 的后续编辑链。
        ensure!(ordered.len() <= 256, "{path} 的历史编辑过多，无法可靠重放");
        let mut matches = 0;
        for start in 0..ordered.len() {
            let mut value = before.cloned();
            let mut valid = true;
            for record in &ordered[start..] {
                let result = record.replay_apply(value.as_ref());
                match result {
                    Ok(next) => value = next,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && value.as_ref() == after {
                matches += 1;
            }
        }
        if matches == 0 {
            let mut work = 0;
            for start in (0..ordered.len()).rev() {
                let mut values = vec![before.cloned()];
                for record in &ordered[start..] {
                    let mut next = Vec::new();
                    for value in values {
                        for candidate in full_replay_candidates(record, value.as_ref(), &mut work)?
                        {
                            if !next.contains(&candidate) {
                                next.push(candidate);
                            }
                            ensure!(next.len() <= 16, "历史完整重放候选过多，已停止提交");
                        }
                    }
                    values = next;
                    if values.is_empty() {
                        break;
                    }
                }
                if values.iter().any(|value| value.as_ref() == after) {
                    return Ok(());
                }
            }
        }
        ensure!(
            matches > 0,
            "{path} 的历史编辑无法与当前 HEAD 和文件内容唯一对应，可能缺少执行前基线、含其他改动或记录不完整，已停止提交"
        );
        Ok(())
    }

    pub(super) fn isolate_changes(
        &self,
        root: &Path,
        path: &str,
        before: Option<&Entry>,
        disk: Option<&Entry>,
    ) -> Result<Entry> {
        let before = before.context("共享文件缺少已提交基线，无法自动分离")?;
        let disk = disk.context("共享文件已删除，无法自动分离")?;
        ensure!(
            before.mode == disk.mode,
            "{path} 的文件权限也已变化，无法自动分离"
        );
        let mut records = self
            .files
            .get(path)
            .context("共享文件缺少本对话的实际补丁记录")?
            .clone();
        records.sort_by_key(|record| (record.start, record.ordinal));
        ensure!(
            records.len() <= 256,
            "{path} 缺少可验证的修改记录，无法自动分离"
        );
        for pair in records.windows(2) {
            ensure!(
                pair[0].actor == pair[1].actor
                    || pair[0]
                        .end
                        .zip(pair[1].start)
                        .is_some_and(|(end, start)| end < start),
                "{path} 的主代理与子代理编辑顺序不明确，无法自动分离"
            );
        }
        // 记录按时间排序，每段能完整套用到 HEAD 的后缀各给出一个候选；候选唯一才说明本对话
        // 改动与当前 HEAD 的对应关系明确，否则无法确定该提交哪一段。
        let mut candidates = Vec::new();
        for start in 0..records.len() {
            let candidate = (|| -> Result<Entry> {
                let mut value = before.clone();
                for record in &records[start..] {
                    value = record
                        .verified_apply(Some(&value))?
                        .context("共享文件修改不能删除文件")?;
                }
                ensure!(&value != before, "本对话补丁已提交或没有剩余改动");
                Ok(value)
            })();
            if let Ok(candidate) = candidate {
                // 已提交的格式化等记录可能产生相同结果；范围由内容决定，不能重复计数。
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        ensure!(
            candidates.len() == 1,
            "{path} 的本对话补丁不能唯一对应当前 HEAD，无法自动分离"
        );
        let candidate = candidates.pop().unwrap();
        for entry in [before, &candidate, disk] {
            ensure!(
                !entry.bytes.contains(&0) && std::str::from_utf8(&entry.bytes).is_ok(),
                "共享文件仅支持 UTF-8 文本的自动分离"
            );
        }
        // 所有输入在临时目录中，-p 仅输出结果，不修改工作区、索引或用户文件。
        let directory = tempfile::tempdir()?;
        let own_path = directory.path().join("conversation");
        let base_path = directory.path().join("base");
        let disk_path = directory.path().join("disk");
        fs::write(&own_path, &candidate.bytes)?;
        fs::write(&base_path, &before.bytes)?;
        fs::write(&disk_path, &disk.bytes)?;
        let merged = git(
            root,
            &[
                "merge-file",
                "-p",
                "--diff3",
                "--",
                own_path.to_str().context("临时路径编码不受支持")?,
                base_path.to_str().context("临时路径编码不受支持")?,
                disk_path.to_str().context("临时路径编码不受支持")?,
            ],
            None,
            None,
        )
        .with_context(|| format!("{path} 的本对话改动与其他改动冲突，无法自动分离"))?;
        ensure!(
            merged == disk.bytes,
            "{path} 的本对话补丁已被修改或撤销，无法自动分离"
        );
        Ok(candidate)
    }
}

pub(super) fn recover(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
) -> Result<History> {
    recover_mode(home, session, root, workspace, false)
}

pub(super) fn recover_for_display(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
) -> Result<History> {
    recover_mode(home, session, root, workspace, true)
}

fn recover_mode(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
    display_only: bool,
) -> Result<History> {
    let (thread, _) =
        crate::session_transfer::find_thread(home, session)?.context("未找到本地对话")?;
    let path = home.join(
        thread["rollout_path"]
            .as_str()
            .context("对话缺少编辑记录")?,
    );
    let mut queue = vec![(session.to_string(), path)];
    let mut visited = BTreeSet::new();
    let mut bytes = 0;
    let mut history = History::default();
    while let Some((actor, path)) = queue.pop() {
        ensure!(
            uuid::Uuid::parse_str(&actor).is_ok(),
            "历史对话或子代理身份格式无效"
        );
        ensure!(
            visited.insert(actor.clone()) && visited.len() <= MAX_ACTORS,
            "历史子代理身份重复或数量过多"
        );
        let transcript = read_transcript_mode(home, &path, display_only)?;
        bytes += transcript.bytes;
        ensure!(
            bytes <= MAX_FAMILY_BYTES,
            "对话及子代理历史过大，无法完整校验"
        );
        history.incomplete |= transcript.incomplete;
        ensure!(
            transcript.meta["id"] == actor
                && transcript.meta["cwd"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    == Some(workspace),
            "历史对话身份或工作区不一致"
        );
        let mut native_scopes = Vec::new();
        for (item, start, end, ordinal) in transcript.native_changes {
            let changes = item["changes"]
                .as_object()
                .context("原生编辑记录缺少文件范围")?;
            // 临时产物等明确位于工作区外的编辑不属于此仓库；混合范围仍需完整校验。
            if !changes.is_empty()
                && changes.keys().all(|raw| {
                    let path = Path::new(raw);
                    path.is_absolute() && !path.starts_with(workspace)
                })
            {
                continue;
            }
            let reported = tracking::reported_files(&item["stdout"], true, root, workspace)?;
            let mut edits = BTreeMap::new();
            for (raw, change) in changes {
                let file = relative_path(root, workspace, raw)?;
                ensure!(
                    change.get("move_path").is_none_or(Value::is_null),
                    "{file} 的历史移动缺少可验证基线，已停止提交"
                );
                let edit = match change["type"].as_str() {
                    Some("update") => Edit::NativePatch(
                        change["unified_diff"]
                            .as_str()
                            .context("原生编辑记录缺少实际补丁")?
                            .into(),
                    ),
                    Some("add") => Edit::Add(
                        change["content"]
                            .as_str()
                            .context("原生新增记录缺少内容")?
                            .into(),
                    ),
                    Some("delete") => Edit::Delete,
                    _ => bail!("原生编辑类型不受支持"),
                };
                ensure!(edits.insert(file, edit).is_none(), "原生编辑记录含重复路径");
            }
            ensure!(
                !reported.is_empty() && reported == edits.keys().cloned().collect(),
                "原生编辑记录与完成回执的文件范围不一致"
            );
            native_scopes.push((start, end, reported));
            for (file, edit) in edits {
                // 成功但没有实际 diff 的原生补丁不产生归属，也不能阻断此前的编辑链。
                if matches!(&edit, Edit::NativePatch(diff) if diff.trim().is_empty()) {
                    continue;
                }
                history.files.entry(file).or_default().push(RecordedEdit {
                    actor: actor.clone(),
                    start: Some(start),
                    end: Some(end),
                    ordinal,
                    edit,
                    replacements: None,
                });
            }
        }
        let mut running_formats = BTreeMap::new();
        let calls =
            transcript
                .calls
                .into_iter()
                .flat_map(|(name, args, response, start, end, ordinal)| {
                    if let Some(calls) = wrapped_cargo_receipts(&name, &args, &response) {
                        calls
                            .into_iter()
                            .map(|(name, mut args, response)| {
                                if name == "exec_command" && args.get("workdir").is_none() {
                                    args["workdir"] = json!(workspace);
                                }
                                (name, args, response, start, end, ordinal)
                            })
                            .collect::<Vec<_>>()
                    } else {
                        vec![(name, args, response, start, end, ordinal)]
                    }
                });
        for (mut name, mut args, response, mut start, end, mut ordinal) in calls {
            if matches!(name.as_str(), "write_stdin" | "functions.write_stdin") {
                if args["chars"]
                    .as_str()
                    .is_some_and(|chars| !chars.is_empty())
                {
                    continue;
                }
                let Some(session) = args["session_id"].as_u64() else {
                    continue;
                };
                let Some((original_name, original_args, original_start, original_order)) =
                    running_formats.remove(&session)
                else {
                    continue;
                };
                name = original_name;
                args = original_args;
                start = original_start;
                ordinal = original_order;
            }
            if cargo_formatter(&name, &args) {
                let Some(receipt) = terminal_receipt(&response) else {
                    continue;
                };
                if let Some(session) = receipt["session_id"].as_u64() {
                    running_formats.insert(session, (name, args, start, ordinal));
                    continue;
                }
                if receipt["exit_code"] != 0
                    || response["isError"] == true
                    || response.get("error").is_some_and(|v| !v.is_null())
                {
                    continue;
                }
                if args["workdir"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    != Some(workspace)
                {
                    continue;
                }
                let manifest = std::str::from_utf8(&read_bounded(
                    &workspace.join("backend/Cargo.toml"),
                    MAX_BYTES,
                )?)?
                .parse::<toml_edit::DocumentMut>()?;
                let Some(package) = manifest.get("package").and_then(toml_edit::Item::as_table)
                else {
                    continue;
                };
                if package.get("name").and_then(toml_edit::Item::as_str) != Some("codey") {
                    continue;
                }
                let root_manifest =
                    std::str::from_utf8(&read_bounded(&workspace.join("Cargo.toml"), MAX_BYTES)?)?
                        .parse::<toml_edit::DocumentMut>()?;
                let inherited = package
                    .get("edition")
                    .and_then(|item| item.get("workspace"))
                    .and_then(toml_edit::Item::as_bool)
                    == Some(true);
                let inherited_edition = root_manifest
                    .get("workspace")
                    .and_then(|item| item.get("package"))
                    .and_then(|item| item.get("edition"))
                    .and_then(toml_edit::Item::as_str);
                let Some(edition) = package
                    .get("edition")
                    .and_then(toml_edit::Item::as_str)
                    .or_else(|| inherited.then_some(inherited_edition).flatten())
                    .filter(|edition| matches!(*edition, "2015" | "2018" | "2021" | "2024"))
                else {
                    continue;
                };
                let paths: Vec<_> = history
                    .files
                    .iter()
                    .filter(|(path, records)| {
                        path.starts_with("backend/src/")
                            && path.ends_with(".rs")
                            && records.iter().any(|record| {
                                record.actor == actor
                                    && record
                                        .end
                                        .zip(start)
                                        .is_some_and(|(end, start)| end < start)
                            })
                    })
                    .map(|(path, _)| path.clone())
                    .collect();
                for file in paths {
                    history.files.entry(file).or_default().push(RecordedEdit {
                        actor: actor.clone(),
                        start: end,
                        end,
                        ordinal,
                        edit: Edit::FormatRust {
                            root: root.to_path_buf(),
                            edition: edition.into(),
                        },
                        replacements: None,
                    });
                }
                continue;
            }
            for (file, edit) in completed_formatters(&name, &args, &response, root, workspace)? {
                history.files.entry(file).or_default().push(RecordedEdit {
                    actor: actor.clone(),
                    start: end,
                    end,
                    ordinal,
                    edit,
                    replacements: None,
                });
            }
            if let Some((patches, trailing)) = multiple_patches(&name, &args) {
                if completed_multiple_patches(&response, patches.len(), trailing) {
                    for patch in patches {
                        let edits = tracking::operations(root, workspace, true, &patch)?;
                        retain_input_context(
                            &mut history,
                            &actor,
                            &native_scopes,
                            start,
                            end,
                            &edits,
                        )?;
                    }
                }
                continue;
            }
            let batch = batch_patch(&name, &args);
            let Some((patch, args)) = tracking::normalized_input(&name, &args)
                .ok()
                .flatten()
                .or_else(|| batch.as_ref().map(|(patch, _)| (true, patch.clone())))
            else {
                continue;
            };
            let response = if let Some((_, trailing)) = batch {
                let Some(receipt) = batch_patch_receipt(&response, trailing) else {
                    continue;
                };
                receipt
            } else {
                response
            };
            let response = if patch {
                let Some(text) = tracking::output_text(&response) else {
                    continue;
                };
                // rollout 文本块可能带执行耗时等元信息；只有一次已知补丁调用的成功回执可用。
                let marker = "Success. Updated the following files:\n";
                if text.matches(marker).count() == 1 {
                    json!(text[text.find(marker).expect("one marker")..].trim())
                } else {
                    response
                }
            } else {
                response
            };
            let reported_result = tracking::completed_files(
                &response,
                patch,
                root,
                workspace,
                matches!(name.as_str(), "functions.exec" | "exec"),
                &args,
            );
            let Ok(reported) = reported_result else {
                continue;
            };
            let edits = if patch {
                tracking::operations(root, workspace, true, &args)?
            } else {
                let raw = args["path"].as_str().context("历史替换缺少路径")?;
                let target = workspace.join(raw);
                // 目录替换只认领完成回执中的实际文件，不重新扫描当前目录。
                reported
                    .iter()
                    .map(|file| {
                        let absolute = root.join(file);
                        ensure!(
                            absolute == target || absolute.starts_with(&target),
                            "历史替换回执包含目标外文件"
                        );
                        Ok((file.clone(), Edit::Replace(args.clone())))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let expected: BTreeSet<_> = edits.iter().map(|(path, _)| path.clone()).collect();
            ensure!(
                expected == reported,
                "历史工具输入与完成回执的文件范围不一致"
            );
            // 同一次补丁可能同时留下工具回执和原生事件，只重放原生实际改动一次。
            if patch && let Some((start, end)) = start.zip(end) {
                let contained: Vec<_> = native_scopes
                    .iter()
                    .filter(|(from, to, _)| start <= *from && *to <= end)
                    .collect();
                if !contained.is_empty() {
                    ensure!(
                        contained.len() == 1 && contained[0].2 == reported,
                        "补丁调用与原生完成记录不能唯一对应"
                    );
                    retain_input_context(
                        &mut history,
                        &actor,
                        &native_scopes,
                        start.into(),
                        end.into(),
                        &edits,
                    )?;
                    continue;
                }
            }
            for (file, edit) in edits {
                let replacements = if patch {
                    None
                } else {
                    let text = tracking::output_text(&response).context("替换回执不完整")?;
                    text.lines().find_map(|line| {
                        let (raw, count) = line.rsplit_once(": ")?;
                        (relative_path(root, workspace, raw).ok().as_deref() == Some(&file))
                            .then(|| count.split_whitespace().next()?.parse().ok())
                            .flatten()
                    })
                };
                history.files.entry(file).or_default().push(RecordedEdit {
                    actor: actor.clone(),
                    start,
                    end,
                    ordinal,
                    edit,
                    replacements,
                });
            }
        }
        history.incomplete |= !running_formats.is_empty();
        if !transcript.spawns.is_empty() {
            for (id, child_path) in child_paths(home, &actor, workspace)? {
                let meta = tracking::metadata(home, &child_path)?;
                if meta["cwd"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    != Some(workspace)
                {
                    continue;
                }
                let nested = &meta["source"]["subagent"]["thread_spawn"];
                if nested
                    .get("parent_thread_id")
                    .is_some_and(|p| p != &json!(actor))
                {
                    continue;
                }
                let Some(role) = meta["agent_role"].as_str() else {
                    continue;
                };
                if nested.get("agent_role").is_some_and(|r| r != &json!(role)) {
                    continue;
                }
                let Some(agent_path) = meta["agent_path"].as_str() else {
                    continue;
                };
                if nested
                    .get("agent_path")
                    .is_some_and(|p| p != &json!(agent_path))
                {
                    continue;
                }
                let bindings = transcript
                    .spawns
                    .iter()
                    .filter(|spawn| {
                        (spawn.binding == id || spawn.binding == agent_path) && spawn.role == role
                    })
                    .count();
                if bindings == 1 {
                    queue.push((id, child_path));
                }
            }
        }
    }
    ensure!(
        history.files.len() <= MAX_FILES,
        "历史对话涉及文件过多，请人工拆分提交"
    );
    Ok(history)
}
