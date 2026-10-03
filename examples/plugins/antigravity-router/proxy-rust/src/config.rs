// Shared config: CLI args, env helpers, paths, wire constants.
use std::path::PathBuf;

pub const DEFAULT_UA: &str =
    "antigravity/cli/1.2.4 (aidev_client; os_type=linux; arch=amd64; cl=982146307; auth_method=consumer)";

pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
pub const OAUTH_CALLBACK_PORT: u16 = 51121;
pub const OAUTH_CALLBACK_TIMEOUT_MS: u64 = 5 * 60 * 1000;
pub const SCOPES: &str = "https://www.googleapis.com/auth/aicode https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";

pub const ENDPOINT_FALLBACKS: [&str; 3] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

pub const DEFAULT_MODEL: &str = "gemini-3.8-flash-medium";
pub const DISCOVERY_TIMEOUT_MS: u64 = 8000;
pub const STREAM_HEADER_TIMEOUT_DEFAULT_MS: u64 = 180_000;
pub const STREAM_STALL_TIMEOUT_DEFAULT_MS: u64 = 120_000;
pub const MAX_SEARCH_GROUNDS: usize = 4;
pub const PLACEHOLDER_THOUGHT_SIGNATURE: &str = "context_engineering_is_the_way_to_go";
pub const CONTINUATION_TEXT: &str =
    "Continue the active task using the available instructions and context.";
pub const DEFAULT_IMAGE_MODEL: &str = "gemini-3-pro-image";
pub const IMAGE_MODEL_FALLBACKS: [&str; 3] = [
    DEFAULT_IMAGE_MODEL,
    "gemini-3.1-flash-image",
    "gemini-3-pro-image-preview",
];
pub const IMAGE_SYSTEM_INSTRUCTION: &str = "You are an AI image generator. Generate images based on user descriptions. Focus on creating high-quality, visually appealing images that match the user's request.";
pub const MAX_PROMPT_CHARS: usize = 8000;

pub fn oauth_client_credentials() -> Result<(String, String), String> {
    oauth_client_from_values(ag_env("CLIENT_ID"), ag_env("CLIENT_SECRET"))
}

fn oauth_client_from_values(
    id: Option<String>,
    secret: Option<String>,
) -> Result<(String, String), String> {
    let required = |value: Option<String>, name: &str| {
        value
            .filter(|v| !v.trim().is_empty() && !v.chars().any(char::is_control))
            .ok_or_else(|| format!("Set ANTIGRAVITY_{name} for your authorized Google OAuth client; no client credentials are bundled"))
    };
    Ok((
        required(id, "CLIENT_ID")?,
        required(secret, "CLIENT_SECRET")?,
    ))
}

/// ANTIGRAVITY_<NAME> with the legacy NOAGY_<NAME> prefix also accepted.
pub fn ag_env(name: &str) -> Option<String> {
    std::env::var(&format!("ANTIGRAVITY_{name}"))
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var(&format!("NOAGY_{name}"))
                .ok()
                .filter(|v| !v.is_empty())
        })
}

pub fn extra_tool_enabled(tool: &str) -> bool {
    extra_tool_policy(
        ag_env("NO_EXTRA_TOOLS").as_deref(),
        ag_env(&format!("NO_{tool}_TOOL")).as_deref(),
    )
}

fn extra_tool_policy(all: Option<&str>, specific: Option<&str>) -> bool {
    all != Some("1") && specific != Some("1")
}

#[cfg(test)]
mod tests {
    #[test]
    fn oauth_client_requires_explicit_values_without_exposing_them() {
        use super::oauth_client_from_values;
        assert!(oauth_client_from_values(None, None).is_err());
        assert!(oauth_client_from_values(Some("mock-client".into()), None).is_err());
        let err =
            oauth_client_from_values(Some("mock-client".into()), Some(" \n".into())).unwrap_err();
        assert!(err.contains("ANTIGRAVITY_CLIENT_SECRET"));
        assert!(!err.contains("mock-client"));
        assert_eq!(
            oauth_client_from_values(Some("mock-client".into()), Some("mock-secret".into()))
                .unwrap(),
            ("mock-client".into(), "mock-secret".into())
        );
    }

    #[test]
    fn tools_can_be_disabled_together_or_separately() {
        assert!(super::extra_tool_policy(None, None));
        assert!(!super::extra_tool_policy(Some("1"), None));
        assert!(!super::extra_tool_policy(None, Some("1")));
        assert!(super::extra_tool_policy(Some("0"), Some("0")));
    }
}

pub fn agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var("PI_CODING_AGENT_DIR")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return PathBuf::from(dir);
    }
    dirs_home().join(".pi").join("agent")
}

pub fn dirs_home() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Default runtime model when the client sends none.
pub fn default_model() -> String {
    ag_env("RUNTIME_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub port: u16,
    pub auth_path: PathBuf,
    pub accounts_path: PathBuf,
}

pub fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub fn serve_args(args: &[String]) -> ServeArgs {
    let port = arg_value(args, "--port")
        .and_then(|v| v.parse().ok())
        .or_else(|| ag_env("PROXY_PORT").and_then(|v| v.parse().ok()))
        .unwrap_or(8787);
    let auth_path = arg_value(args, "--auth")
        .or_else(|| ag_env("AUTH_PATH"))
        .map(PathBuf::from)
        .unwrap_or_else(|| agent_dir().join("auth.json"));
    let accounts_path = arg_value(args, "--accounts")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            auth_path
                .parent()
                .map(|p| p.join("antigravity-accounts.json"))
                .unwrap_or_else(|| PathBuf::from("antigravity-accounts.json"))
        });
    ServeArgs {
        port,
        auth_path,
        accounts_path,
    }
}
