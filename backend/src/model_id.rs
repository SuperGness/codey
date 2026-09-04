use std::collections::{BTreeMap, HashSet};

pub(crate) fn key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

pub(crate) fn equal(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// Call only after checking current aliases and raw ids. Model names may
/// contain slashes; only recorded selectors and the legacy Codey prefix qualify.
pub(crate) fn historical_source<'a>(
    model: &'a str,
    aliases: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    let model = model.trim();
    aliases.get(&key(model)).map(String::as_str).or_else(|| {
        model
            .get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("codey/"))
            .and_then(|_| model.get(6..))
            .map(str::trim)
            .filter(|source| !source.is_empty())
    })
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
    fn historical_selectors_require_a_record_or_the_legacy_codey_prefix() {
        let aliases = BTreeMap::from([("old%2froute/vendor/model".into(), "vendor/model".into())]);
        for (input, expected) in [
            (" Old%2FRoute/vendor/model ", Some("vendor/model")),
            ("CoDeY/vendor/model", Some("vendor/model")),
            ("vendor/model", None),
            ("unknown/vendor/model", None),
            ("codey/", None),
            ("路由/model", None),
        ] {
            assert_eq!(historical_source(input, &aliases), expected, "{input}");
        }
    }
}
