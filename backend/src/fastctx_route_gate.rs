use std::ffi::OsStr;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const HOOK_ARGUMENT: &str = "--codey-fastctx-route-hook";
pub(crate) const HOOK_TIMEOUT_SECONDS: u64 = 5;
pub(crate) const HOOK_MATCHER: &str =
    "^(Bash|list_mcp_resources|list_mcp_resource_templates|read_mcp_resource)$";
const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;
const FALLBACK_MARKER: &str = "# codey-fastctx-fallback";

#[derive(Debug, Deserialize)]
struct HookInput {
    hook_event_name: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FastctxRoute {
    Inspect,
    Search,
    Discover,
}

impl FastctxRoute {
    fn tool_name(self) -> &'static str {
        match self {
            Self::Inspect => "mcp__codey_fastctx__inspect_local_file",
            Self::Search => "mcp__codey_fastctx__grep",
            Self::Discover => "mcp__codey_fastctx__glob",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Inspect => 1,
            Self::Discover => 2,
            Self::Search => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SegmentClass {
    Harmless,
    Routed(FastctxRoute),
    Unknown,
}

pub fn run_hook_if_requested() -> Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(HOOK_ARGUMENT)) {
        return Ok(false);
    }

    let raw = crate::hook_io::read_stdin_bounded(
        MAX_HOOK_INPUT_BYTES,
        "读取 Codex FastCtx 路由 Hook 输入失败",
    )?;
    // 路由改道只是上下文优化,不是安全边界:输入超限或解析失败时显式放行原命令,
    // 与未安装本 Hook 的行为一致,避免非零退出在 Codex 中被反复报告为 Hook 错误。
    if raw.len() as u64 > MAX_HOOK_INPUT_BYTES {
        eprintln!("Codey FastCtx 路由 Hook 输入超过 1 MiB 上限，已放行");
        return write_hook_output(&json!({})).map(|()| true);
    }
    let output = match serde_json::from_slice::<HookInput>(&raw) {
        Ok(input) => handle_hook(&input),
        Err(error) => {
            eprintln!("Codey FastCtx 路由 Hook 输入无效，已放行：{error:#}");
            json!({})
        }
    };
    write_hook_output(&output)?;
    Ok(true)
}

fn write_hook_output(output: &Value) -> Result<()> {
    crate::hook_io::write_output(output, "序列化 Codex FastCtx 路由 Hook 输出失败")
}

fn handle_hook(input: &HookInput) -> Value {
    hook_output(
        &input.hook_event_name,
        input.tool_name.as_deref(),
        input.tool_input.as_ref(),
    )
}

pub(crate) fn hook_output(
    hook_event_name: &str,
    tool_name: Option<&str>,
    tool_input: Option<&Value>,
) -> Value {
    if hook_event_name != "PreToolUse" {
        return json!({});
    }
    let Some(tool_name) = tool_name else {
        return json!({});
    };

    if !tool_name.eq_ignore_ascii_case("Bash") {
        return handle_resource_tool(tool_name, tool_input);
    }
    let Some(command) = tool_input
        .and_then(|tool_input| tool_input.get("command"))
        .and_then(Value::as_str)
    else {
        return json!({});
    };
    let Some(route) = route_for_command(command) else {
        return json!({});
    };

    deny(&format!(
        "Codey FastCtx：请改用 `{}`；仅当该工具不可用时，才以 `{FALLBACK_MARKER}` 作为命令首行重试。",
        route.tool_name(),
    ))
}

fn handle_resource_tool(tool_name: &str, tool_input: Option<&Value>) -> Value {
    if matches_resource_tool(tool_name, "read_mcp_resource") {
        return guard_resource_read(tool_input);
    }
    if matches_resource_tool(tool_name, "list_mcp_resources")
        || matches_resource_tool(tool_name, "list_mcp_resource_templates")
    {
        return guard_resource_list(tool_input);
    }
    json!({})
}

fn guard_resource_read(tool_input: Option<&Value>) -> Value {
    let server = string_field(tool_input, "server");
    let uri = string_field(tool_input, "uri");
    if server.is_none_or(str::is_empty) || uri.is_none_or(str::is_empty) {
        return deny(&invalid_resource_read_reason());
    }
    if server.is_some_and(is_codey_fastctx_resource_alias) {
        return deny(&fastctx_resource_reason());
    }
    if uri.is_some_and(is_local_file_uri) {
        return deny(&local_resource_bypass_reason());
    }
    if uri.is_some_and(|uri| is_plain_local_path(uri) || !has_uri_scheme(uri)) {
        return deny(&invalid_resource_read_reason());
    }
    json!({})
}

fn guard_resource_list(tool_input: Option<&Value>) -> Value {
    let Some(server) = explicit_string_field(tool_input, "server") else {
        return json!({});
    };
    if server.trim().is_empty() {
        return deny(
            "MCP 资源发现已停止：若要查询全部已配置服务器，请省略 `server`；若要查询单个服务器，请传入配置中真实存在的名称。",
        );
    }
    if is_codey_fastctx_resource_alias(server) {
        return deny(&fastctx_resource_reason());
    }
    json!({})
}

fn explicit_string_field<'a>(tool_input: Option<&'a Value>, field: &str) -> Option<&'a str> {
    tool_input?.get(field)?.as_str()
}

fn string_field<'a>(tool_input: Option<&'a Value>, field: &str) -> Option<&'a str> {
    explicit_string_field(tool_input, field).map(str::trim)
}

fn matches_resource_tool(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn is_codey_fastctx_resource_alias(server: &str) -> bool {
    matches!(
        server.trim().to_ascii_lowercase().as_str(),
        "codey_fastctx" | "mcp__codey_fastctx"
    )
}

fn is_plain_local_path(uri: &str) -> bool {
    let bytes = uri.as_bytes();
    uri.starts_with('/')
        || uri.starts_with("\\\\")
        || bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\')
}

fn has_uri_scheme(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn is_local_file_uri(uri: &str) -> bool {
    uri.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("file"))
}

fn invalid_resource_read_reason() -> String {
    "MCP 资源读取已停止：`server` 和 `uri` 必须原样使用成功资源发现返回的真实值，不能填写 `x`、`none`、本地路径或其他占位内容。本地工作区文件请直接调用 `mcp__codey_fastctx__inspect_local_file`；若工具尚未暴露，先用 `tool_search`，或在 code mode 中从 `ALL_TOOLS` 定位。".to_string()
}

fn fastctx_resource_reason() -> String {
    "Codey FastCtx 只提供直接调用的文件工具，不能作为资源服务器使用。本地文件请调用 `mcp__codey_fastctx__inspect_local_file`，搜索与发现请分别调用 `mcp__codey_fastctx__grep` 和 `mcp__codey_fastctx__glob`；若工具尚未暴露，先用 `tool_search`，或在 code mode 中从 `ALL_TOOLS` 定位。".to_string()
}

fn local_resource_bypass_reason() -> String {
    "本地 `file://` MCP 资源读取已停止：本地工作区文件必须通过 Codey FastCtx 直接文件工具访问，不能经外部 filesystem 资源服务器绕过 direct-only 边界。请调用 `mcp__codey_fastctx__inspect_local_file`；搜索与发现请分别调用 `mcp__codey_fastctx__grep` 和 `mcp__codey_fastctx__glob`。".to_string()
}

fn deny(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

fn route_for_command(command: &str) -> Option<FastctxRoute> {
    if command
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim() == FALLBACK_MARKER)
    {
        return None;
    }
    let segments = split_shell_segments(command)?;
    let mut route = None;
    for segment in segments {
        match classify_segment(&segment) {
            SegmentClass::Unknown => return None,
            SegmentClass::Harmless => {}
            SegmentClass::Routed(candidate) => {
                if route
                    .is_none_or(|current: FastctxRoute| candidate.priority() > current.priority())
                {
                    route = Some(candidate);
                }
            }
        }
    }
    route
}

fn split_shell_segments(command: &str) -> Option<Vec<String>> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut quote = Quote::None;
    let mut current = String::new();
    let mut segments = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        match quote {
            Quote::Single => {
                current.push(character);
                if character == '\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => {
                if character == '"' {
                    quote = Quote::None;
                    current.push(character);
                } else if character == '`' || (character == '$' && chars.peek() == Some(&'(')) {
                    return None;
                } else {
                    current.push(character);
                }
            }
            Quote::None => match character {
                '\'' => {
                    quote = Quote::Single;
                    current.push(character);
                }
                '"' => {
                    quote = Quote::Double;
                    current.push(character);
                }
                '>' | '<' | '`' | '(' | ')' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '&' => {
                    chars.next_if_eq(&'&')?;
                    push_segment(&mut segments, &mut current);
                }
                '|' => {
                    let _ = chars.next_if_eq(&'|');
                    push_segment(&mut segments, &mut current);
                }
                ';' | '\n' => push_segment(&mut segments, &mut current),
                '\r' => {}
                '\\' => {
                    current.push(character);
                    if let Some(next) = chars.peek().copied()
                        && matches!(next, ' ' | '\t' | '\'' | '"' | ';' | '|' | '&' | '>' | '<')
                    {
                        current.push(chars.next().expect("peeked escaped shell character"));
                    }
                }
                _ => current.push(character),
            },
        }
    }
    if quote != Quote::None {
        return None;
    }
    push_segment(&mut segments, &mut current);
    (!segments.is_empty()).then_some(segments)
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    let segment = current.trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
    current.clear();
}

fn classify_segment(segment: &str) -> SegmentClass {
    let Some(words) = shell_words(segment) else {
        return SegmentClass::Unknown;
    };
    let Some((command, arguments)) = command_and_arguments(&words) else {
        return SegmentClass::Harmless;
    };
    let command = normalized_command(command);
    let lower_arguments = arguments
        .iter()
        .map(|argument| argument.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let args = lower_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();

    match command.as_str() {
        "cd" | "pwd" | "pushd" | "popd" | "set-location" => SegmentClass::Harmless,
        "rg" | "ripgrep" if safe_ripgrep(&args) && args.contains(&"--files") => {
            SegmentClass::Routed(FastctxRoute::Discover)
        }
        "rg" | "ripgrep" if safe_ripgrep(&args) && !args.is_empty() => {
            SegmentClass::Routed(FastctxRoute::Search)
        }
        "cat" | "get-content" | "gc" if simple_file_inspection(&command, &args) => {
            SegmentClass::Routed(FastctxRoute::Inspect)
        }
        _ => SegmentClass::Unknown,
    }
}

fn command_and_arguments(words: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0;
    while words.get(index).is_some_and(|word| shell_assignment(word)) {
        index += 1;
    }
    if words
        .get(index)
        .is_some_and(|word| matches!(normalized_command(word).as_str(), "command" | "builtin"))
    {
        index += 1;
    }
    let command = words.get(index)?;
    Some((command, &words[index + 1..]))
}

fn normalized_command(command: &str) -> String {
    let basename = command
        .rsplit_once(['/', '\\'])
        .map_or(command, |(_, basename)| basename)
        .to_ascii_lowercase();
    basename
        .strip_suffix(".exe")
        .or_else(|| basename.strip_suffix(".cmd"))
        .or_else(|| basename.strip_suffix(".bat"))
        .unwrap_or(&basename)
        .to_string()
}

fn shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        })
}

fn shell_words(segment: &str) -> Option<Vec<String>> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut quote = Quote::None;
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = segment.chars().peekable();
    while let Some(character) = chars.next() {
        match quote {
            Quote::Single => {
                if character == '\'' {
                    quote = Quote::None;
                } else {
                    current.push(character);
                }
            }
            Quote::Double => {
                if character == '"' {
                    quote = Quote::None;
                } else {
                    current.push(character);
                }
            }
            Quote::None => match character {
                '\'' => quote = Quote::Single,
                '"' => quote = Quote::Double,
                ' ' | '\t' => push_word(&mut words, &mut current),
                '\\' => {
                    if let Some(next) = chars.peek().copied()
                        && matches!(next, ' ' | '\t' | '\'' | '"' | '\\')
                    {
                        current.push(chars.next().expect("peeked escaped word character"));
                    } else {
                        current.push(character);
                    }
                }
                _ => current.push(character),
            },
        }
    }
    if quote != Quote::None {
        return None;
    }
    push_word(&mut words, &mut current);
    Some(words)
}

fn push_word(words: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

fn has_file_operand(arguments: &[&str]) -> bool {
    arguments.iter().any(|argument| {
        !argument.is_empty()
            && !argument.starts_with('-')
            && !argument.starts_with('/')
            && !argument.bytes().all(|byte| byte.is_ascii_digit())
    }) || arguments
        .iter()
        .any(|argument| argument.contains(['/', '\\']) || argument.contains('.'))
}

fn simple_file_inspection(command: &str, arguments: &[&str]) -> bool {
    has_file_operand(arguments)
        && arguments
            .iter()
            .all(|argument| !argument.starts_with('-') || command == "cat" && *argument == "--")
}

fn safe_ripgrep(arguments: &[&str]) -> bool {
    // 参数在比较前已统一转为小写,因此短形式 `-f` 同时覆盖 `-f`(--file)
    // 与 `-F`(--fixed-strings),`-m` 同时覆盖 `-m`(--max-count)与
    // `-M`(--max-columns),`-u` 同时覆盖 `-u`(--unrestricted)与 `-U`(--multiline);
    // 长形式则必须逐一列出。这些 flag 的语义 FastCtx grep 无法等价表达,一律放行到 shell。
    !arguments.iter().any(|argument| {
        matches!(
            *argument,
            "-r" | "--replace"
                | "--pre"
                | "--pre-glob"
                | "-p"
                | "--pcre2"
                | "-u"
                | "-uu"
                | "-uuu"
                | "--unrestricted"
                | "--no-ignore"
                | "--no-ignore-vcs"
                | "--no-ignore-parent"
                | "--no-ignore-global"
                | "--hidden"
                | "--follow"
                | "-a"
                | "--text"
                | "--binary"
                | "-z"
                | "--search-zip"
                | "--json"
                | "--vimgrep"
                | "--pretty"
                | "--stats"
                | "--debug"
                | "--trace"
                | "-f"
                | "--fixed-strings"
                | "--file"
                | "-v"
                | "--invert-match"
                | "-c"
                | "--count"
                | "--count-matches"
                | "--column"
                | "--byte-offset"
                | "--passthru"
                | "--files-without-match"
                | "--type-list"
                | "--engine"
                | "--encoding"
                | "--sort"
                | "--sortr"
                | "--max-count"
                | "--max-filesize"
                | "-m"
                | "--max-columns"
                | "-q"
                | "--quiet"
                | "-0"
                | "--null"
        ) || argument.starts_with("--replace=")
            || argument.starts_with("--pre=")
            || argument.starts_with("--pre-glob=")
            || argument.starts_with("--engine=")
            || argument.starts_with("--encoding=")
            || argument.starts_with("--sort=")
            || argument.starts_with("--sortr=")
            || argument.starts_with("--max-count=")
            || argument.starts_with("--max-filesize=")
            || argument.starts_with("--file=")
            || argument.starts_with("--max-columns=")
            || argument.starts_with("--multiline")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook_input(command: &str) -> HookInput {
        tool_hook_input("Bash", json!({ "command": command }))
    }

    fn tool_hook_input(tool_name: &str, tool_input: Value) -> HookInput {
        HookInput {
            hook_event_name: "PreToolUse".to_string(),
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(tool_input),
        }
    }

    fn assert_denied(output: &Value) -> &str {
        assert_eq!(
            output["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        output["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
    }

    #[test]
    fn routes_only_semantically_portable_file_commands() {
        for (command, expected) in [
            ("rg -n needle src", FastctxRoute::Search),
            ("/usr/bin/rg --files", FastctxRoute::Discover),
            ("cat src/main.rs", FastctxRoute::Inspect),
            ("cd C:\\repo && rg --files", FastctxRoute::Discover),
            ("Get-Content .\\src\\main.rs", FastctxRoute::Inspect),
        ] {
            assert_eq!(route_for_command(command), Some(expected), "{command}");
        }
    }

    #[test]
    fn allows_non_file_and_potentially_mutating_shell_commands() {
        for command in [
            "cargo test",
            "git grep needle",
            "npm run build",
            "find . -delete",
            "find . -type f -exec rm {} +",
            "find . -type f -printf '%p %s\\n'",
            "fd -t f --exec rm {}",
            "rg --replace new old src",
            "sed -i 's/old/new/' file.rs",
            "cat input.txt > output.txt",
            "rg needle src && cargo test",
            "powershell -Command Get-Content file.rs",
            "nl -ba src/main.rs | sed -n '1,80p'",
            "Get-ChildItem -Recurse -File | Select-String needle",
            "find . -type f -name '*.rs'",
            "fd -t f",
            "ls -la src",
            "tail -f app.log",
            "wc -l src/main.rs",
            "grep -n needle src/main.rs",
            "bat src/main.rs",
            "cat -n src/main.rs",
            "Get-Content -Raw .\\src\\main.rs",
            "rg -P '(?=needle)' src",
            "rg -uu needle src",
            "rg --no-ignore needle src",
            "rg --encoding=shift_jis needle src",
            "rg --count needle src",
            "rg -F a.b src",
            "rg --fixed-strings a.b src",
            "rg -f patterns.txt src",
            "rg --file patterns.txt src",
            "rg --file=patterns.txt src",
            "rg -m 5 needle src",
            "rg -M 80 needle src",
            "rg --max-columns 80 needle src",
            "rg -q needle src",
            "rg --null -l needle src",
            "rg -U 'a\\nb' src",
            "rg --multiline 'a\\nb' src",
            "rg --multiline-dotall 'a.*b' src",
        ] {
            assert_eq!(route_for_command(command), None, "{command}");
        }
    }

    #[test]
    fn explicit_fallback_marker_allows_terminal_retry() {
        assert_eq!(
            route_for_command("# codey-fastctx-fallback\nrg needle src"),
            None
        );
    }

    #[test]
    fn pre_tool_use_denial_is_concise_and_names_the_safe_fallback() {
        let output = handle_hook(&hook_input("rg -n needle src"));
        let reason = assert_denied(&output);
        assert_eq!(
            reason,
            "Codey FastCtx：请改用 `mcp__codey_fastctx__grep`；仅当该工具不可用时，才以 `# codey-fastctx-fallback` 作为命令首行重试。"
        );
    }

    #[test]
    fn blocks_placeholder_and_plain_path_resource_reads() {
        for (server, uri) in [
            ("x", "x"),
            ("none", "none"),
            ("filesystem", "/workspace/src/main.rs"),
            ("filesystem", "C:\\workspace\\src\\main.rs"),
        ] {
            let output = handle_hook(&tool_hook_input(
                "read_mcp_resource",
                json!({ "server": server, "uri": uri }),
            ));
            let reason = assert_denied(&output);
            assert!(reason.contains("mcp__codey_fastctx__inspect_local_file"));
            assert!(reason.contains("成功资源发现返回的真实值"));
        }
    }

    #[test]
    fn blocks_fastctx_resource_aliases() {
        for tool_name in [
            "read_mcp_resource",
            "list_mcp_resources",
            "list_mcp_resource_templates",
        ] {
            let input = if tool_name == "read_mcp_resource" {
                json!({ "server": "mcp__codey_fastctx", "uri": "file:///workspace/src/main.rs" })
            } else {
                json!({ "server": "codey_fastctx" })
            };
            let output = handle_hook(&tool_hook_input(tool_name, input));
            let reason = assert_denied(&output);
            assert!(reason.contains("不能作为资源服务器使用"));
        }
    }

    #[test]
    fn allows_global_discovery_and_valid_remote_resource_uris() {
        assert_eq!(
            handle_hook(&tool_hook_input("list_mcp_resources", json!({}))),
            json!({})
        );
        assert_eq!(
            handle_hook(&tool_hook_input(
                "read_mcp_resource",
                json!({ "server": "docs", "uri": "docs://guide/getting-started" }),
            )),
            json!({})
        );
    }

    #[test]
    fn blocks_file_uri_resource_reads_instead_of_bypassing_direct_fastctx_tools() {
        for (server, uri) in [
            ("filesystem", "file:///workspace/src/main.rs"),
            ("filesystem", "FILE:///workspace/src/main.rs"),
            ("another-local-server", "file:///workspace/src/main.rs"),
        ] {
            let output = handle_hook(&tool_hook_input(
                "read_mcp_resource",
                json!({ "server": server, "uri": uri }),
            ));
            let reason = assert_denied(&output);
            assert!(reason.contains("direct-only 边界"), "{reason}");
            assert!(reason.contains("mcp__codey_fastctx__inspect_local_file"));
        }
    }

    #[test]
    fn unrelated_hook_events_and_tools_are_ignored() {
        let mut input = hook_input("rg needle src");
        input.tool_name = Some("apply_patch".to_string());
        assert_eq!(handle_hook(&input), json!({}));
        input.tool_name = Some("Bash".to_string());
        input.hook_event_name = "PostToolUse".to_string();
        assert_eq!(handle_hook(&input), json!({}));
    }
}
