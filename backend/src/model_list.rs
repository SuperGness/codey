use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelEndpointError {
    InvalidUrl,
    UnsupportedSchemeOrHost,
}

pub(crate) fn model_endpoints(
    base_url: &str,
    hash_skips_version_prefix: bool,
    require_host: bool,
) -> Result<Vec<String>, ModelEndpointError> {
    let skip_version_prefix = hash_skips_version_prefix && base_url.trim().ends_with('#');
    let cleaned_base = if hash_skips_version_prefix {
        base_url.trim().trim_end_matches('#').trim_end_matches('/')
    } else {
        base_url.trim().trim_end_matches('/')
    };
    let mut url = reqwest::Url::parse(cleaned_base).map_err(|_| ModelEndpointError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || require_host && url.host_str().is_none() {
        return Err(ModelEndpointError::UnsupportedSchemeOrHost);
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url.path().trim_end_matches('/').to_string();
    let mut base = url.as_str().trim_end_matches('/').to_string();
    const ENDPOINT_SUFFIXES: &[&str] = &["/chat/completions", "/responses", "/messages"];
    if let Some(suffix) = ENDPOINT_SUFFIXES
        .iter()
        .find(|suffix| path.to_ascii_lowercase().ends_with(**suffix))
    {
        path.truncate(path.len() - suffix.len());
        base.truncate(base.len() - suffix.len());
    }
    let last_segment = path.rsplit('/').next().unwrap_or_default();
    Ok(if last_segment.eq_ignore_ascii_case("models") {
        vec![base]
    } else if skip_version_prefix || has_version_suffix(last_segment) {
        vec![format!("{base}/models")]
    } else {
        vec![format!("{base}/v1/models"), format!("{base}/models")]
    })
}

fn has_version_suffix(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .or_else(|| segment.strip_prefix('V'))
        .is_some_and(|version| version.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
}

pub(crate) fn visit_model_ids<E>(
    value: &Value,
    visitor: &mut impl FnMut(&str) -> Result<bool, E>,
) -> Result<bool, E> {
    visit_model_entries(value, &mut |id, _| visitor(id))
}

pub(crate) fn visit_model_entries<E>(
    value: &Value,
    visitor: &mut impl FnMut(&str, &Value) -> Result<bool, E>,
) -> Result<bool, E> {
    match value {
        Value::Array(items) => {
            visit_model_items(items, visitor)?;
            Ok(true)
        }
        Value::Object(object) => {
            let mut recognized = false;
            for key in ["data", "models", "items"] {
                if let Some(items) = object.get(key).and_then(Value::as_array) {
                    recognized = true;
                    if !visit_model_items(items, visitor)? {
                        return Ok(true);
                    }
                }
            }
            if !recognized && let Some(model) = model_id_value(value) {
                visitor(model, value)?;
                recognized = true;
            }
            Ok(recognized)
        }
        _ => Ok(false),
    }
}

fn visit_model_items<E>(
    items: &[Value],
    visitor: &mut impl FnMut(&str, &Value) -> Result<bool, E>,
) -> Result<bool, E> {
    for item in items {
        if let Some(model) = model_id_value(item)
            && !visitor(model, item)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn model_id_value(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        let object = value.as_object()?;
        ["id", "name", "slug", "model"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
    })
}
