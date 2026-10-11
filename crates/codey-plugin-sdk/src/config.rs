//! 与宿主共用的配置格式检查。只处理传入文本，不读写文件或执行插件代码。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum ConfigError {
    TooLarge,
    InvalidJson(serde_json::Error),
    InvalidRoot,
    InvalidCommentsObject { path: String },
    InvalidCommentValue { path: String },
}

// 保留宿主已有的错误描述；路径可能包含配置键名，预检查接口不返回此文本。
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "配置文件超过 1 MiB"),
            Self::InvalidJson(error) => {
                write!(f, "config.json 不是有效 JSON，请打开配置文件修复：{error}")
            }
            Self::InvalidRoot => write!(f, "config.json 的根值必须是 JSON 对象"),
            Self::InvalidCommentsObject { path } => {
                write!(f, "{path} 必须是对象，每项说明必须是字符串")
            }
            Self::InvalidCommentValue { path } => write!(f, "{path} 说明必须是字符串"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorCode {
    TooLarge,
    InvalidJson,
    InvalidRoot,
    InvalidCommentsObject,
    InvalidCommentValue,
}

/// 不含配置键、值或原始解析错误；语法错误提供 1 起始行号及字节列号。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub code: ValidationErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationResult {
    pub valid: bool,
    pub byte_length: usize,
    pub max_bytes: usize,
    pub error: Option<ValidationIssue>,
}

impl ConfigError {
    fn validation_issue(&self) -> ValidationIssue {
        use ValidationErrorCode as Code;
        let (code, message) = match self {
            Self::TooLarge => (Code::TooLarge, "配置文件超过 1 MiB"),
            Self::InvalidJson(_) => (Code::InvalidJson, "配置不是有效 JSON"),
            Self::InvalidRoot => (Code::InvalidRoot, "配置根值必须是 JSON 对象"),
            Self::InvalidCommentsObject { .. } => {
                (Code::InvalidCommentsObject, "_comments 必须是对象")
            }
            Self::InvalidCommentValue { .. } => {
                (Code::InvalidCommentValue, "_comments 中的说明必须是字符串")
            }
        };
        let (line, column) = match self {
            Self::InvalidJson(error) => (
                (error.line() > 0).then_some(error.line()),
                (error.column() > 0).then_some(error.column()),
            ),
            _ => (None, None),
        };
        ValidationIssue {
            code,
            message: message.into(),
            line,
            column,
        }
    }
}

/// 检查格式并递归移除 `_comments`，返回插件实际接收的业务配置。
/// 不检查插件特定的业务约束。错误文本可能包含配置键名。
pub fn parse(content: &str) -> Result<Value, ConfigError> {
    if content.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let mut value: Value = serde_json::from_str(content).map_err(ConfigError::InvalidJson)?;
    if !value.is_object() {
        return Err(ConfigError::InvalidRoot);
    }
    remove_comments(&mut value, "$")?;
    Ok(value)
}

/// 格式预检查，不回传配置或业务字段；通过不代表插件业务配置有效。
///
/// ```
/// use codey_plugin_sdk::config;
/// let report = config::validate(r#"{"_comments":{"enabled":"开关"},"enabled":true}"#);
/// assert!(report.valid);
/// assert!(report.error.is_none());
/// assert_eq!(config::parse(r#"{"_comments":{},"enabled":true}"#).unwrap(),
///            serde_json::json!({"enabled":true}));
/// ```
pub fn validate(content: &str) -> ValidationResult {
    let error = parse(content).err().map(|error| error.validation_issue());
    ValidationResult {
        valid: error.is_none(),
        byte_length: content.len(),
        max_bytes: MAX_CONFIG_BYTES,
        error,
    }
}

fn remove_comments(value: &mut Value, path: &str) -> Result<(), ConfigError> {
    match value {
        Value::Object(object) => {
            if let Some(comments) = object.remove("_comments") {
                let comments_path = format!("{path}[\"_comments\"]");
                let entries =
                    comments
                        .as_object()
                        .ok_or_else(|| ConfigError::InvalidCommentsObject {
                            path: comments_path.clone(),
                        })?;
                for (key, description) in entries {
                    if !description.is_string() {
                        return Err(ConfigError::InvalidCommentValue {
                            path: format!(
                                "{comments_path}[{}]",
                                serde_json::to_string(key).unwrap()
                            ),
                        });
                    }
                }
            }
            for (key, child) in object {
                remove_comments(
                    child,
                    &format!("{path}[{}]", serde_json::to_string(key).unwrap()),
                )?;
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                remove_comments(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parsing_removes_only_comments_and_validation_preserves_the_input() {
        let input = json!({
            "_comments": {"note":"说明"}, "_private":7,
            "nested":[{"_comments":{},"value":"_comments"}],
            "_commentsLike":true
        })
        .to_string();
        let original = input.clone();
        assert!(validate(&input).valid);
        assert_eq!(input, original);
        assert_eq!(
            parse(&input).unwrap(),
            json!({
                "_private":7, "nested":[{"value":"_comments"}], "_commentsLike":true
            })
        );
    }

    #[test]
    fn validation_never_echoes_business_keys_values_or_parser_messages() {
        for (input, code) in [
            (
                r#"{"private-key-secret":{"_comments":false}}"#,
                ValidationErrorCode::InvalidCommentsObject,
            ),
            (
                r#"{"_comments":{"private-key-secret":123}}"#,
                ValidationErrorCode::InvalidCommentValue,
            ),
            (
                r#"{"private-key-secret":"private-value-secret",}"#,
                ValidationErrorCode::InvalidJson,
            ),
            (
                r#""private-value-secret""#,
                ValidationErrorCode::InvalidRoot,
            ),
        ] {
            let result = validate(input);
            assert!(!result.valid);
            assert_eq!(result.error.as_ref().unwrap().code, code);
            let output = serde_json::to_string(&result).unwrap();
            assert!(!output.contains("private-key-secret"), "{output}");
            assert!(!output.contains("private-value-secret"), "{output}");
            assert_eq!(
                serde_json::from_str::<ValidationResult>(&output).unwrap(),
                result
            );
        }
    }

    #[test]
    fn invalid_json_reports_position_and_rejects_comments_and_deep_nesting() {
        let result = validate("{\n  \"value\":\n}");
        let issue = result.error.unwrap();
        assert_eq!(issue.code, ValidationErrorCode::InvalidJson);
        assert_eq!((issue.line, issue.column), (Some(3), Some(1)));
        for input in [
            "{\"value\":true // note\n}".to_string(),
            format!("{{\"value\":{}0{}}}", "[".repeat(130), "]".repeat(130)),
        ] {
            assert_eq!(
                validate(&input).error.unwrap().code,
                ValidationErrorCode::InvalidJson
            );
        }
    }

    #[test]
    fn utf8_size_limit_includes_comments_and_accepts_exact_boundary() {
        let input = format!("{{}}{}", " ".repeat(MAX_CONFIG_BYTES - 2));
        let result = validate(&input);
        assert!(result.valid);
        assert_eq!(result.byte_length, MAX_CONFIG_BYTES);
        assert_eq!(
            validate(&(input + " ")).error.unwrap().code,
            ValidationErrorCode::TooLarge
        );
        let input = format!(
            "{{\"_comments\":{{\"note\":\"{}\"}}}}",
            "说".repeat(MAX_CONFIG_BYTES / 3)
        );
        let result = validate(&input);
        assert_eq!(result.byte_length, input.len());
        assert_eq!(result.error.unwrap().code, ValidationErrorCode::TooLarge);
    }
}
