use std::collections::HashSet;

use anyhow::{Context, Result};
use reqwest::header::ACCEPT;
use serde_json::Value;

use crate::config::ProviderProfile;

pub async fn fetch(profile: &ProviderProfile) -> Result<Vec<String>> {
    let base = profile.normalized_base_url();
    if base.is_empty() {
        anyhow::bail!("API 地址不能为空");
    }
    let endpoints = model_endpoints(&base)?;
    let client = reqwest::Client::builder().user_agent("Codey/0.1").build()?;

    for (index, endpoint) in endpoints.iter().enumerate() {
        let mut request = client.get(endpoint).header(ACCEPT, "application/json");
        if !profile.api_key.trim().is_empty() {
            request = request.bearer_auth(profile.api_key.trim());
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("获取上游模型失败：{endpoint}"))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .with_context(|| format!("读取上游模型列表失败：{endpoint}"))?;
        let has_fallback = index + 1 < endpoints.len();
        if matches!(
            status,
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
        ) && has_fallback
        {
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("获取上游模型失败：{endpoint} 返回 HTTP {status}");
        }
        match model_ids(&body) {
            Ok(models) => return Ok(models),
            Err(_) if has_fallback => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("解析上游模型列表失败：{endpoint}"));
            }
        }
    }
    anyhow::bail!("上游没有返回可用的模型列表")
}

fn model_endpoints(base: &str) -> Result<Vec<String>> {
    let mut url = reqwest::Url::parse(base).context("API 地址格式无效")?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("API 地址仅支持 HTTP 或 HTTPS");
    }
    url.set_query(None);
    url.set_fragment(None);
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let base = url.as_str().trim_end_matches('/');
    Ok(match last_segment {
        "models" => vec![base.to_string()],
        "v1" => vec![format!("{base}/models")],
        _ => vec![format!("{base}/v1/models"), format!("{base}/models")],
    })
}

fn model_ids(body: &[u8]) -> Result<Vec<String>> {
    let value = serde_json::from_slice::<Value>(body).context("模型列表不是有效 JSON")?;
    let items = value
        .as_array()
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("models").and_then(Value::as_array))
        .ok_or_else(|| anyhow::anyhow!("上游模型列表格式不受支持"))?;
    let mut seen = HashSet::new();
    Ok(items
        .iter()
        .filter_map(|item| {
            item.as_str().or_else(|| {
                item.get("id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("name").and_then(Value::as_str))
                    .or_else(|| item.get("slug").and_then(Value::as_str))
                    .or_else(|| item.get("model").and_then(Value::as_str))
            })
        })
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .filter(|model| seen.insert((*model).to_string()))
        .map(ToString::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_compatible_model_endpoints() {
        assert_eq!(
            model_endpoints("https://relay.example/v1").unwrap(),
            vec!["https://relay.example/v1/models"]
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
        let models = model_ids(br#"{"data":[{"id":"a"},{"name":"b"},{"id":"a"}]}"#).unwrap();
        assert_eq!(models, vec!["a", "b"]);
    }
}
