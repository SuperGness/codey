use std::path::Path;

use anyhow::Result;
use codey_runtime_core::config_manager::ConfigManager;
use toml_edit::{Item, value};

const SERVER_ID: &str = "codey_computer_use";
const CLIENT_PATH: &str = "Codex Computer Use.app/Contents/SharedSupport/SkyComputerUseClient.app/Contents/MacOS/SkyComputerUseClient";

pub(super) fn preserve_enabled_legacy_server(home: &Path) -> Result<bool> {
    let manager = ConfigManager::for_home(home);
    let snapshot = manager.load()?;
    let mut document = snapshot.document().clone();
    let plugins = document.get("plugins").and_then(Item::as_table_like);
    let plugin_enabled = |name| {
        plugins
            .and_then(|plugins| plugins.get(name))
            .and_then(Item::as_table_like)
            .and_then(|plugin| plugin.get("enabled"))
            .and_then(Item::as_bool)
    };
    if plugin_enabled("unified-computer-use@openai-bundled") != Some(true)
        || plugin_enabled("computer-use@openai-bundled") == Some(false)
    {
        return Ok(false);
    }
    let Some(servers) = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(false);
    };
    if servers.contains_key(SERVER_ID) {
        return Ok(false);
    }
    let Some(mut server) = servers.get("computer-use").cloned() else {
        return Ok(false);
    };
    let Some(table) = server.as_table_like_mut() else {
        return Ok(false);
    };
    let client_home = std::path::absolute(home.join("computer-use"))?;
    let client = client_home.join(CLIENT_PATH);
    let command = table.get("command").and_then(Item::as_str);
    if table.get("enabled").and_then(Item::as_bool) != Some(true)
        || table.get("disabled").and_then(Item::as_bool) == Some(true)
        || !table
            .get("args")
            .and_then(Item::as_array)
            .is_some_and(|args| {
                args.len() == 1 && args.get(0).and_then(|arg| arg.as_str()) == Some("mcp")
            })
        || !command.is_some_and(|command| {
            command == format!("./{CLIENT_PATH}") || Path::new(command) == client
        })
        || !client.is_file()
    {
        return Ok(false);
    }
    // Codex rewrites the legacy ID in both startup and per-thread config.
    // Preserve an explicitly enabled service under an independent ID instead.
    table.insert("command", value(client.to_string_lossy().into_owned()));
    table.insert("cwd", value(client_home.to_string_lossy().into_owned()));
    servers.insert(SERVER_ID, server);
    manager.replace_document(
        Some(snapshot.revision()),
        document,
        "preserve enabled Computer Use service across desktop config sync",
        "codex_config.computer_use.preserve_enabled_legacy_server",
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use toml_edit::DocumentMut;

    #[test]
    fn computer_use_migration_preserves_choices_and_survives_legacy_reset() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let config = home.join("config.toml");
        let client = home.join("computer-use").join(CLIENT_PATH);
        fs::create_dir_all(client.parent().unwrap()).unwrap();
        fs::write(&client, b"test client").unwrap();
        let source = format!(
            "[plugins.\"unified-computer-use@openai-bundled\"]\nenabled = true\n\n\
             [mcp_servers.computer-use]\ncommand = './{CLIENT_PATH}'\nargs = ['mcp']\n\
             cwd = '.'\nenabled = true\nstartup_timeout_sec = 42\nenabled_tools = ['list_apps']\n\n\
             [mcp_servers.node_repl]\ncommand = 'node_repl'\n"
        );
        for untouched in [
            source.replacen("enabled = true", "enabled = false", 1),
            source.replace("cwd = '.'\nenabled = true", "cwd = '.'\nenabled = false"),
            source.replace("cwd = '.'", "cwd = '.'\ndisabled = true"),
            source.replace(
                "[plugins.\"unified-computer-use@openai-bundled\"]\nenabled = true\n\n",
                "",
            ),
            source.replace("command = './", "command = '/custom/"),
            source.replace("args = ['mcp']", "args = ['custom']"),
            source.replace("[mcp_servers.computer-use]", "[mcp_servers.custom]"),
            format!("{source}\n[plugins.\"computer-use@openai-bundled\"]\nenabled = false\n"),
            format!(
                "{source}\n[mcp_servers.codey_computer_use]\ncommand = 'custom'\nenabled = false\n"
            ),
        ] {
            fs::write(&config, &untouched).unwrap();
            assert!(!preserve_enabled_legacy_server(home).unwrap());
            assert_eq!(fs::read_to_string(&config).unwrap(), untouched);
        }

        fs::write(&config, &source).unwrap();
        fs::remove_file(&client).unwrap();
        assert!(!preserve_enabled_legacy_server(home).unwrap());
        fs::write(&client, b"test client").unwrap();
        #[cfg(target_os = "macos")]
        super::super::apply_isolated_runtime_router_config(
            home,
            super::super::RouterApplyOptions {
                local_router: None,
                use_official_catalog: false,
                default_model: None,
                fastctx_command: None,
                subagent_optimization: false,
                subagent_model: super::super::DEFAULT_SUBAGENT_MODEL,
                subagent_reasoning_effort: super::super::DEFAULT_SUBAGENT_REASONING_EFFORT,
                subagent_roles: None,
                marker: &home.join("lease.json"),
                backup_root: &home.join("backups"),
            },
        )
        .unwrap();
        #[cfg(not(target_os = "macos"))]
        assert!(preserve_enabled_legacy_server(home).unwrap());
        let mut migrated = fs::read_to_string(&config)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let legacy = source.parse::<DocumentMut>().unwrap();
        assert_eq!(
            migrated["mcp_servers"]["computer-use"].to_string(),
            legacy["mcp_servers"]["computer-use"].to_string()
        );
        assert_eq!(
            migrated["mcp_servers"][SERVER_ID]["command"].as_str(),
            client.to_str()
        );
        assert_eq!(
            migrated["mcp_servers"][SERVER_ID]["startup_timeout_sec"].as_integer(),
            Some(42)
        );
        assert_eq!(
            migrated["mcp_servers"][SERVER_ID]["enabled_tools"]
                .as_array()
                .unwrap()
                .get(0)
                .unwrap()
                .as_str(),
            Some("list_apps")
        );
        migrated["mcp_servers"]["computer-use"]["enabled"] = value(false);
        fs::write(&config, migrated.to_string()).unwrap();
        assert!(!preserve_enabled_legacy_server(home).unwrap());
        assert_eq!(
            migrated["mcp_servers"][SERVER_ID]["enabled"].as_bool(),
            Some(true)
        );
        migrated["mcp_servers"][SERVER_ID]["enabled"] = value(false);
        fs::write(&config, migrated.to_string()).unwrap();
        assert!(!preserve_enabled_legacy_server(home).unwrap());
        assert_eq!(fs::read_to_string(&config).unwrap(), migrated.to_string());
    }
}
