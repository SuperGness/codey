use std::collections::{BTreeMap, HashSet};

pub(crate) fn key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub(crate) fn equal(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// Call only after checking current aliases and raw ids. Model names may
/// contain slashes, so only selectors recorded in the alias history qualify.
pub(crate) fn historical_source<'a>(
    model: &'a str,
    aliases: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    aliases.get(&key(model.trim())).map(String::as_str)
}

/// A route-qualified model selector: `<encoded provider>/<upstream model>`.
/// The provider segment is percent-encoded by [`model_alias`] and therefore
/// never contains `/`, so the first slash always separates the two parts even
/// when the upstream model id itself contains slashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteAlias<'a> {
    pub provider_key: &'a str,
    pub upstream_model: &'a str,
}

pub(crate) fn parse_alias(alias: &str) -> Option<RouteAlias<'_>> {
    let (provider_key, upstream_model) = alias.trim().split_once('/')?;
    let provider_key = provider_key.trim();
    let upstream_model = upstream_model.trim();
    (!provider_key.is_empty() && !upstream_model.is_empty()).then_some(RouteAlias {
        provider_key,
        upstream_model,
    })
}

/// Build the stable route-qualified selector shown in the model picker.
pub(crate) fn model_alias(provider_id: &str, model: &str) -> String {
    format!("{}/{}", encode_alias_component(provider_id), model.trim())
}

fn encode_alias_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

/// 按线路保存的模型顺序排列条目；不在顺序表里的条目保持原有相对顺序排在最后。
pub(crate) fn sort_by_selection_order<T>(
    items: &mut [T],
    selected_models: &[String],
    slug: impl Fn(&T) -> Option<&str>,
) {
    let mut positions = std::collections::HashMap::new();
    for (index, model) in selected_models.iter().enumerate() {
        positions.entry(key(model)).or_insert(index);
    }
    items.sort_by_key(|item| {
        slug(item)
            .and_then(|slug| positions.get(&key(slug)).copied())
            .unwrap_or(usize::MAX)
    });
}

pub(crate) fn dedupe_preserving_first<'a>(
    models: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter_map(|model| {
            let model = model.trim();
            let key = key(model);
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(model.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_case_insensitive_but_keep_first_spelling() {
        assert!(equal(" Provider-A ", "provider-a"));
        assert_eq!(key(" Provider-A "), "provider-a");
        assert_eq!(
            dedupe_preserving_first([" Provider-A ", "provider-a", "Provider-B"]),
            ["Provider-A", "Provider-B"]
        );
    }

    #[test]
    fn selection_order_puts_configured_models_first_and_keeps_the_rest_stable() {
        let mut items = vec!["c", "x", "a", "y", "b"];
        sort_by_selection_order(
            &mut items,
            &["A".into(), "b".into(), "c".into(), "b".into()],
            |item| Some(*item),
        );
        assert_eq!(items, ["a", "b", "c", "x", "y"]);
    }

    #[test]
    fn aliases_split_on_the_first_slash_so_upstream_models_may_contain_slashes() {
        let alias = model_alias("my route", "org/model-v1");
        assert_eq!(alias, "my%20route/org/model-v1");
        let parsed = parse_alias(&alias).unwrap();
        assert_eq!(parsed.provider_key, "my%20route");
        assert_eq!(parsed.upstream_model, "org/model-v1");
        assert_eq!(parse_alias("provider/"), None);
        assert_eq!(parse_alias("/model"), None);
        assert_eq!(parse_alias("plain-model"), None);
    }

    #[test]
    fn historical_selectors_require_a_recorded_alias() {
        let aliases = BTreeMap::from([("old%2froute/vendor/model".into(), "vendor/model".into())]);
        for (input, expected) in [
            (" Old%2FRoute/vendor/model ", Some("vendor/model")),
            ("CoDeY/vendor/model", None),
            ("vendor/model", None),
            ("unknown/vendor/model", None),
            ("codey/", None),
            ("路由/model", None),
        ] {
            assert_eq!(historical_source(input, &aliases), expected, "{input}");
        }
    }
}
