use anyhow::{Context, Result};
use std::{collections::BTreeSet, path::PathBuf};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

pub struct SkillConfigSummary {
    pub disabled: Vec<PathBuf>,
    pub named: BTreeSet<String>,
    pub unknown_paths: BTreeSet<PathBuf>,
}

fn for_each_rule(doc: &DocumentMut, mut visit: impl FnMut(&dyn TableLike)) -> Result<()> {
    let Some(config) = doc.get("skills").and_then(|v| v.get("config")) else {
        return Ok(());
    };
    if let Some(tables) = config.as_array_of_tables() {
        for table in tables {
            visit(table);
        }
        return Ok(());
    }
    for value in config.as_array().context("skills.config 必须为表数组")? {
        let inline = value
            .as_inline_table()
            .context("skills.config 必须为表数组")?;
        visit(inline);
    }
    Ok(())
}

pub fn summary(doc: &DocumentMut) -> Result<SkillConfigSummary> {
    let mut disabled = Vec::new();
    let mut named = BTreeSet::new();
    let mut unknown_paths = BTreeSet::new();
    for_each_rule(doc, |rule| {
        if let Some(name) = rule.get("name").and_then(Item::as_str) {
            named.insert(name.to_owned());
        }
        if let Some(path) = rule.get("path").and_then(Item::as_str) {
            let path_buf = PathBuf::from(path);
            disabled.retain(|p| p != &path_buf);
            if let Some(enabled) = rule.get("enabled") {
                match enabled.as_bool() {
                    Some(false) => disabled.push(path_buf),
                    None => {
                        unknown_paths.insert(path_buf);
                    }
                    Some(true) => {}
                }
            }
        }
    })?;
    Ok(SkillConfigSummary {
        disabled,
        named,
        unknown_paths,
    })
}

pub fn disabled(doc: &DocumentMut) -> Result<(Vec<PathBuf>, BTreeSet<String>)> {
    let parsed = summary(doc)?;
    Ok((parsed.disabled, parsed.named))
}

pub fn set_enabled(doc: &mut DocumentMut, path: &str, enabled: bool) -> Result<()> {
    for_each_rule(doc, |_| {})?;
    if !doc.contains_key("skills") {
        doc["skills"] = Item::Table(Table::new());
    }
    let inline = doc["skills"].is_inline_table();
    let skills = doc["skills"]
        .as_table_like_mut()
        .context("skills 配置必须为表")?;
    if !skills.contains_key("config") {
        skills.insert(
            "config",
            if inline {
                Item::Value(Value::Array(Array::new()))
            } else {
                Item::ArrayOfTables(ArrayOfTables::new())
            },
        );
    }
    let config = skills.get_mut("config").unwrap();
    let mut matched = false;
    if let Some(tables) = config.as_array_of_tables_mut() {
        for rule in tables.iter_mut() {
            if rule.get("path").and_then(Item::as_str) == Some(path) {
                rule["enabled"] = toml_edit::value(enabled);
                matched = true;
            }
        }
        if !matched {
            let mut rule = Table::new();
            rule["path"] = toml_edit::value(path);
            rule["enabled"] = toml_edit::value(enabled);
            tables.push(rule);
        }
    } else {
        let array = config
            .as_array_mut()
            .context("skills.config 必须为表数组")?;
        for rule in array.iter_mut() {
            let rule = rule
                .as_inline_table_mut()
                .context("skills.config 必须为表数组")?;
            if rule.get("path").and_then(Value::as_str) == Some(path) {
                rule.insert("enabled", Value::from(enabled));
                matched = true;
            }
        }
        if !matched {
            let mut rule = InlineTable::new();
            rule.insert("path", Value::from(path));
            rule.insert("enabled", Value::from(enabled));
            array.push(Value::InlineTable(rule));
        }
    }
    Ok(())
}
