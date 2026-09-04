use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{
    Client,
    header::{ACCEPT, AUTHORIZATION},
};
use serde_json::Value;

use crate::config::{ProviderProfile, UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES};
use crate::model_id;
use crate::model_list::{self, ModelEndpointError};

const PROVIDER_MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_PROVIDER_MODEL_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_MODELS: usize = 10_000;
pub(crate) const MAX_PROVIDER_MODEL_ID_BYTES: usize = 512;

#[derive(Debug)]
enum ModelListError {
    InvalidJson(serde_json::Error),
    UnsupportedFormat,
    Empty,
    TooManyModels { limit: usize },
    ModelIdTooLong { limit: usize },
}

impl ModelListError {
    fn allows_endpoint_fallback(&self) -> bool {
        matches!(
            self,
            Self::InvalidJson(_) | Self::UnsupportedFormat | Self::Empty
        )
    }
}

impl fmt::Display for ModelListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "模型列表不是有效 JSON：{error}"),
            Self::UnsupportedFormat => formatter.write_str("上游模型列表格式不受支持"),
            Self::Empty => formatter.write_str("上游返回空模型列表"),
            Self::TooManyModels { limit } => {
                write!(formatter, "上游模型数量超过安全上限 {limit}")
            }
            Self::ModelIdTooLong { limit } => {
                write!(formatter, "上游模型 ID 超过安全上限 {limit} 字节")
            }
        }
    }
}

impl std::error::Error for ModelListError {}

pub async fn fetch(profile: &ProviderProfile, client: &Client) -> Result<Vec<String>> {
    let base = profile.normalized_base_url();
    if base.is_empty() {
        anyhow::bail!("API 地址不能为空");
    }
    let endpoints = model_endpoints(&base)?;
    for (index, endpoint) in endpoints.iter().enumerate() {
        let mut request = client.get(endpoint).header(ACCEPT, "application/json");
        let anthropic_messages = profile.upstream_protocol == UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES;
        let has_custom_authorization = profile.model_request_headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case(AUTHORIZATION.as_str()) && !value.trim().is_empty()
        });
        let has_custom_anthropic_key = profile.model_request_headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-api-key") && !value.trim().is_empty()
        });
        let has_custom_anthropic_version =
            profile.model_request_headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("anthropic-version") && !value.trim().is_empty()
            });
        if anthropic_messages && !profile.api_key.trim().is_empty() && !has_custom_anthropic_key {
            request = request.header("x-api-key", profile.api_key.trim());
        } else if !anthropic_messages
            && !profile.api_key.trim().is_empty()
            && !has_custom_authorization
        {
            request = request.bearer_auth(profile.api_key.trim());
        }
        if anthropic_messages && !has_custom_anthropic_version {
            request = request.header("anthropic-version", "2023-06-01");
        }
        for (name, value) in &profile.model_request_headers {
            if anthropic_messages && name.eq_ignore_ascii_case(AUTHORIZATION.as_str()) {
                continue;
            }
            if anthropic_messages
                && (name.eq_ignore_ascii_case("x-api-key")
                    || name.eq_ignore_ascii_case("anthropic-version"))
                && value.trim().is_empty()
            {
                continue;
            }
            if name.eq_ignore_ascii_case(AUTHORIZATION.as_str()) && value.trim().is_empty() {
                continue;
            }
            request = request.header(name, value);
        }
        let response = request
            .timeout(PROVIDER_MODEL_REQUEST_TIMEOUT)
            .send()
            .await
            .with_context(|| format!("获取上游模型失败：{endpoint}"))?;
        let status = response.status();
        let has_fallback = index + 1 < endpoints.len();
        if has_fallback
            && (matches!(
                status,
                reqwest::StatusCode::NOT_FOUND
                    | reqwest::StatusCode::METHOD_NOT_ALLOWED
                    | reqwest::StatusCode::REQUEST_TIMEOUT
                    | reqwest::StatusCode::TOO_MANY_REQUESTS
            ) || status.is_server_error())
        {
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("获取上游模型失败：{endpoint} 返回 HTTP {status}");
        }
        let body = crate::http_response::read_bounded_body(
            response,
            MAX_PROVIDER_MODEL_RESPONSE_BYTES,
            "上游模型列表响应",
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}：{endpoint}"))?;
        match model_ids(&body) {
            Ok(models) => return Ok(models),
            Err(error) if has_fallback && error.allows_endpoint_fallback() => continue,
            Err(error) => {
                return Err(anyhow::Error::new(error))
                    .with_context(|| format!("解析上游模型列表失败：{endpoint}"));
            }
        }
    }
    anyhow::bail!("上游没有返回可用的模型列表")
}

fn model_endpoints(base: &str) -> Result<Vec<String>> {
    model_list::model_endpoints(base, true, false).map_err(|error| match error {
        ModelEndpointError::InvalidUrl => anyhow::anyhow!("API 地址格式无效"),
        ModelEndpointError::UnsupportedSchemeOrHost => {
            anyhow::anyhow!("API 地址仅支持 HTTP 或 HTTPS")
        }
    })
}

fn model_ids(body: &[u8]) -> std::result::Result<Vec<String>, ModelListError> {
    model_ids_with_limits(body, MAX_PROVIDER_MODELS, MAX_PROVIDER_MODEL_ID_BYTES)
}

fn model_ids_with_limits(
    body: &[u8],
    max_models: usize,
    max_model_id_bytes: usize,
) -> std::result::Result<Vec<String>, ModelListError> {
    let value = serde_json::from_slice::<Value>(body).map_err(ModelListError::InvalidJson)?;
    let mut models = Vec::new();
    let mut seen = HashSet::<String>::new();
    let recognized = model_list::visit_model_ids(&value, &mut |model| {
        push_model_id(
            model,
            &mut models,
            &mut seen,
            max_models,
            max_model_id_bytes,
        )?;
        Ok(true)
    })?;
    if !recognized {
        return Err(ModelListError::UnsupportedFormat);
    }
    if models.is_empty() {
        return Err(ModelListError::Empty);
    }
    Ok(models)
}

fn push_model_id(
    model: &str,
    models: &mut Vec<String>,
    seen: &mut HashSet<String>,
    max_models: usize,
    max_model_id_bytes: usize,
) -> std::result::Result<(), ModelListError> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(());
    }
    if model.len() > max_model_id_bytes {
        return Err(ModelListError::ModelIdTooLong {
            limit: max_model_id_bytes,
        });
    }
    if !seen.insert(model_id::key(model)) {
        return Ok(());
    }
    if models.len() >= max_models {
        return Err(ModelListError::TooManyModels { limit: max_models });
    }
    models.push(model.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn builds_compatible_model_endpoints() {
        assert_eq!(
            model_endpoints("https://relay.example/v1").unwrap(),
            vec!["https://relay.example/v1/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/v1/responses").unwrap(),
            vec!["https://relay.example/v1/models"]
        );
        assert_eq!(
            model_endpoints("https://api.anthropic.com/v1/messages").unwrap(),
            vec!["https://api.anthropic.com/v1/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/api/coding/v3").unwrap(),
            vec!["https://relay.example/api/coding/v3/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/api%20space/v1/responses").unwrap(),
            vec!["https://relay.example/api%20space/v1/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/api/v1/v1beta").unwrap(),
            vec!["https://relay.example/api/v1/v1beta/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/api#").unwrap(),
            vec!["https://relay.example/api/models"]
        );
        assert_eq!(
            model_endpoints("https://relay.example/api").unwrap(),
            vec![
                "https://relay.example/api/v1/models",
                "https://relay.example/api/models"
            ]
        );
    }

    #[test]
    fn parses_common_model_list_shapes() {
        let models =
            model_ids(br#"{"data":[{"id":"Provider-A"},{"name":"b"},{"id":"provider-a"}]}"#)
                .unwrap();
        assert_eq!(models, vec!["Provider-A", "b"]);

        assert_eq!(
            model_ids(br#"{"items":[{"model":"item-model"}]}"#).unwrap(),
            vec!["item-model"]
        );
        assert_eq!(
            model_ids(br#"{"slug":"single-model"}"#).unwrap(),
            vec!["single-model"]
        );
    }

    #[test]
    fn rejects_empty_model_lists() {
        let error = model_ids(br#"{"data":[]}"#).unwrap_err();
        assert!(matches!(error, ModelListError::Empty));

        let error = model_ids(br#"{"data":[[["not-a-model"]]]}"#).unwrap_err();
        assert!(matches!(error, ModelListError::Empty));
    }

    #[test]
    fn enforces_unique_model_count_without_counting_duplicates() {
        let models =
            model_ids_with_limits(br#"{"data":[{"id":"a"},{"id":"a"},{"id":"b"}]}"#, 2, 16)
                .unwrap();
        assert_eq!(models, vec!["a", "b"]);

        let error = model_ids_with_limits(br#"{"data":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#, 2, 16)
            .unwrap_err();
        assert!(matches!(error, ModelListError::TooManyModels { limit: 2 }));
    }

    #[test]
    fn rejects_model_ids_over_the_byte_limit() {
        let error = model_ids_with_limits(br#"{"models":["abcd"]}"#, 4, 3).unwrap_err();
        assert!(matches!(error, ModelListError::ModelIdTooLong { limit: 3 }));
    }

    #[tokio::test]
    async fn rejects_declared_oversized_responses_before_reading_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_PROVIDER_MODEL_RESPONSE_BYTES + 1
            )
            .unwrap();
        });
        let mut profile = ProviderProfile::new("test");
        profile.base_url = format!("http://{address}/v1");
        let client = Client::builder().no_proxy().build().unwrap();

        let error = fetch(&profile, &client).await.unwrap_err();

        let detail = error.to_string();
        assert!(detail.contains("响应超过安全上限"));
        assert!(detail.contains(&format!("http://{address}/v1/models")));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn sends_custom_provider_headers_without_overwriting_authorization() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.contains("authorization: custom secret"));
            assert!(!request.contains("authorization: bearer fallback-key"));
            assert!(request.contains("x-route: manual"));
            let body = r#"{"data":[{"id":"custom-model"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut profile = ProviderProfile::new("test");
        profile.base_url = format!("http://{address}/v1");
        profile.api_key = "fallback-key".to_string();
        profile
            .model_request_headers
            .insert("Authorization".to_string(), "Custom secret".to_string());
        profile
            .model_request_headers
            .insert("X-Route".to_string(), "manual".to_string());
        let client = Client::builder().no_proxy().build().unwrap();

        let models = fetch(&profile, &client).await.unwrap();

        assert_eq!(models, vec!["custom-model"]);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn empty_custom_authorization_does_not_suppress_bearer_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer fallback-key"));
            assert_eq!(
                request
                    .lines()
                    .filter(|line| line.starts_with("authorization:"))
                    .count(),
                1
            );
            let body = r#"{"data":[{"id":"bearer-model"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut profile = ProviderProfile::new("test");
        profile.base_url = format!("http://{address}/v1");
        profile.api_key = "fallback-key".to_string();
        profile
            .model_request_headers
            .insert("Authorization".to_string(), " ".to_string());
        let client = Client::builder().no_proxy().build().unwrap();

        let models = fetch(&profile, &client).await.unwrap();

        assert_eq!(models, vec!["bearer-model"]);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn anthropic_model_sync_uses_x_api_key_and_version_without_bearer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
            assert!(request.starts_with("get /v1/models http/1.1"));
            assert!(request.contains("x-api-key: anthropic-key"));
            assert!(request.contains("anthropic-version: 2023-06-01"));
            assert!(!request.contains("authorization:"));
            let body = r#"{"data":[{"id":"claude-sonnet-test"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut profile = ProviderProfile::new("Anthropic");
        profile.base_url = format!("http://{address}/v1/messages");
        profile.api_key = "anthropic-key".to_string();
        profile.upstream_protocol = UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.to_string();
        let client = Client::builder().no_proxy().build().unwrap();

        let models = fetch(&profile, &client).await.unwrap();

        assert_eq!(models, vec!["claude-sonnet-test"]);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn falls_back_when_the_first_endpoint_returns_an_empty_list() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (expected_path, body) in [
                ("/api/v1/models", r#"{"data":[]}"#),
                ("/api/models", r#"{"models":["fallback-model"]}"#),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.starts_with(&format!("GET {expected_path} HTTP/1.1")));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let mut profile = ProviderProfile::new("test");
        profile.base_url = format!("http://{address}/api");
        let client = Client::builder().no_proxy().build().unwrap();

        let models = fetch(&profile, &client).await.unwrap();

        assert_eq!(models, vec!["fallback-model"]);
        server.join().unwrap();
    }
}
