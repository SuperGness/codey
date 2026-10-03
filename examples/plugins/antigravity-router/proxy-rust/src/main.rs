// antigravity-proxy: single-binary Antigravity provider bridge.
// Subcommands: serve (default) | login | accounts | refresh | token | models | usage | doctor
mod auth;
mod catalog;
mod client_util;
mod config;
mod convert;
mod discovery;
mod imagegen;
mod images_in;
mod proxy_core;
mod security;
mod server;
mod stream;
mod upstream;
mod usage;
mod websearch;
mod writer;
mod ws;

use config::{arg_value, serve_args};
use std::path::PathBuf;
use std::sync::Arc;

// Crate-root re-exports used across modules.
pub use config::default_model;
pub use proxy_core::{log_info, log_warn};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h"))
        || args.first().map(String::as_str) == Some("help")
    {
        println!("{}", CLI_USAGE);
        return;
    }
    if args
        .first()
        .map(String::as_str)
        .map(|s| matches!(s, "--version" | "-V" | "version"))
        .unwrap_or(false)
    {
        println!("antigravity-proxy {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let cmd = args.first().map(String::as_str).unwrap_or("serve");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = runtime.block_on(async_main(
        cmd,
        &args[if args.is_empty() { 0 } else { 1 }..],
    ));
    std::process::exit(code);
}

const CLI_USAGE: &str = "antigravity-proxy [serve [--port PORT]] | login [--manual] | accounts [list|switch SELECTOR|remove SELECTOR] | refresh | token | models [--all] | usage | doctor | search [--thinking] [--url URL] [--instruction TEXT] QUERY\nCommon: --auth PATH --accounts PATH. Help: --help. Version: --version.";

async fn async_main(cmd: &str, args: &[String]) -> i32 {
    if let Err(e) = validate_cli_args(cmd, args) {
        eprintln!("{e}\n{CLI_USAGE}");
        return 2;
    }
    let serve = serve_args(args);
    match cmd {
        "serve" => {
            let mirror = images_in::BedMirror::new().map(Arc::new);
            let state = Arc::new(server::AppState {
                port: serve.port,
                ctx: proxy_core::RequestContext {
                    auth_path: serve.auth_path.clone(),
                    accounts_path: serve.accounts_path.clone(),
                    image_mirror: mirror,
                },
            });
            // Graceful shutdown on Ctrl+C.
            tokio::spawn(async {
                let _ = tokio::signal::ctrl_c().await;
                std::process::exit(0);
            });
            if let Err(e) = server::run(state, serve.port).await {
                eprintln!("[proxy] {e}");
                return 1;
            }
            0
        }
        "login" => {
            if arg_value(args, "--callback-url").is_some() {
                eprintln!("A callback belongs to its original PKCE flow. Use login --manual and paste the callback into the same running process.");
                return 2;
            }
            let manual = args.iter().any(|a| a == "--manual");
            match auth::login(&serve.auth_path, &serve.accounts_path, manual).await {
                Ok(outcome) => {
                    println!(
                        "Login complete. email={} project={}",
                        outcome.email.as_deref().unwrap_or("(unknown)"),
                        outcome
                            .project_id
                            .as_deref()
                            .unwrap_or("(discovery deferred)")
                    );
                    0
                }
                Err(e) => {
                    eprintln!("[login] {}", security::safe_error(e));
                    1
                }
            }
        }
        "accounts" => accounts_cmd(&serve, args).await,
        "refresh" => match auth::fresh_credential(&serve.auth_path, &serve.accounts_path).await {
            Ok((cred, _)) => {
                println!(
                    "refreshed; expires {}",
                    chrono::DateTime::from_timestamp_millis(cred.expires_ms)
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "?".into())
                );
                0
            }
            Err(e) => {
                eprintln!("[refresh] {}", security::safe_error(e));
                1
            }
        },
        "token" => match auth::fresh_credential(&serve.auth_path, &serve.accounts_path).await {
            Ok((_, token)) => {
                println!("{token}");
                0
            }
            Err(e) => {
                eprintln!("[token] {}", security::safe_error(e));
                1
            }
        },
        "models" => models_cmd(&serve, args.contains(&"--all".to_string())).await,
        "usage" => usage_cmd(&serve).await,
        "search" => search_cmd(&serve, args).await,
        "doctor" => {
            let snap = usage::snapshot_json();
            println!(
                "{}",
                serde_json::to_string_pretty(&snap).unwrap_or_default()
            );
            0
        }
        other => {
            eprintln!(
                "Unknown command: {other}\nUsage: antigravity-proxy [serve|--port 8787] | login [--callback-url URL] | accounts [switch|remove <sel>] | refresh | token | models [--all] | usage | doctor"
            );
            1
        }
    }
}

fn validate_cli_args(cmd: &str, args: &[String]) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            if !matches!(cmd, "search" | "accounts") {
                return Err(format!("unexpected argument: {a}"));
            }
            i += 1;
            continue;
        }
        if (cmd == "search" && a == "--thinking")
            || (cmd == "models" && a == "--all")
            || (cmd == "login" && a == "--manual")
        {
            i += 1;
            continue;
        }
        if matches!(a.as_str(), "--auth" | "--accounts")
            || (cmd == "serve" && a == "--port")
            || (cmd == "login" && a == "--callback-url")
            || (cmd == "search" && matches!(a.as_str(), "--url" | "--instruction"))
        {
            let value = args
                .get(i + 1)
                .filter(|s| !s.starts_with("--"))
                .ok_or_else(|| format!("{a} requires a value"))?;
            if a == "--port" && value.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
                return Err("--port must be 1-65535".into());
            }
            i += 2;
        } else {
            return Err(format!("unknown option: {a}"));
        }
    }
    Ok(())
}

async fn search_cmd(serve: &config::ServeArgs, args: &[String]) -> i32 {
    let mut options = serde_json::json!({"query":"","urls":[],"thinking":false});
    let mut query = vec![];
    let mut urls = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--thinking" => options["thinking"] = serde_json::json!(true),
            "--url" => {
                i += 1;
                urls.push(args[i].clone());
            }
            "--instruction" => {
                i += 1;
                options["instruction"] = serde_json::json!(args[i]);
            }
            "--auth" | "--accounts" => {
                i += 1;
            }
            s => query.push(s.to_string()),
        }
        i += 1;
    }
    options["query"] = serde_json::json!(query.join(" "));
    options["urls"] = serde_json::json!(urls);
    let result = async {
        let opts = websearch::SearchOptions::from_value(&options)?;
        let (cred, token) = auth::fresh_credential(&serve.auth_path, &serve.accounts_path).await?;
        let project =
            discovery::resolve_project_id(&token, None, cred.project_id, cred.email.as_deref());
        websearch::execute_search(
            &token,
            &project,
            &opts,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    }
    .await;
    match result {
        Ok(v) => {
            println!("{}", v["result"].as_str().unwrap_or(""));
            0
        }
        Err(e) => {
            eprintln!("[search] {}", security::safe_error(e));
            1
        }
    }
}

#[cfg(test)]
mod cli_tests {
    #[test]
    fn flags_fail_fast() {
        assert!(super::validate_cli_args("serve", &["--bogus".into()]).is_err());
        assert!(super::validate_cli_args("serve", &["--port".into(), "oops".into()]).is_err());
        assert!(super::validate_cli_args("search", &["--url".into()]).is_err());
        assert!(super::validate_cli_args("login", &["--manual".into()]).is_ok());
    }
}

async fn accounts_cmd(serve: &config::ServeArgs, args: &[String]) -> i32 {
    auth::sync_current_auth(&serve.auth_path, &serve.accounts_path);
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" | "" => {
            let (accounts, active) = auth::list_accounts(&serve.accounts_path);
            if accounts.is_empty() {
                println!("No linked accounts. Run `antigravity-proxy login` first.");
                return 0;
            }
            for (i, account) in accounts.iter().enumerate() {
                let marker = if active.as_deref() == Some(account.account_id.as_str()) {
                    "*"
                } else {
                    " "
                };
                let email = account
                    .email
                    .as_deref()
                    .and_then(|e| security::mask_email(Some(e)))
                    .unwrap_or_else(|| account.account_id.clone());
                println!("{marker} {} {} ({})", i + 1, account.account_id, email);
            }
            0
        }
        "switch" => match args.get(1) {
            Some(sel) => {
                match auth::activate_account(&serve.auth_path, &serve.accounts_path, sel).await {
                    Ok(account) => {
                        println!("switched to {}", account.account_id);
                        0
                    }
                    Err(e) => {
                        eprintln!("[accounts] {}", security::safe_error(e));
                        1
                    }
                }
            }
            None => {
                eprintln!("[accounts] switch requires an index or email");
                1
            }
        },
        "remove" => match args.get(1) {
            Some(sel) => {
                match auth::remove_account(&serve.auth_path, &serve.accounts_path, sel).await {
                    Ok(next) => {
                        println!(
                            "removed; active account is now {}",
                            next.map(|n| n.account_id)
                                .unwrap_or_else(|| "(none)".into())
                        );
                        0
                    }
                    Err(e) => {
                        eprintln!("[accounts] {}", security::safe_error(e));
                        1
                    }
                }
            }
            None => {
                eprintln!("[accounts] remove requires an index or email");
                1
            }
        },
        other => {
            eprintln!(
                "[accounts] unknown subcommand: {other} (use list | switch <sel> | remove <sel>)"
            );
            1
        }
    }
}

async fn models_cmd(serve: &config::ServeArgs, all: bool) -> i32 {
    match auth::fresh_credential(&serve.auth_path, &serve.accounts_path).await {
        Ok((cred, token)) => {
            let project = discovery::resolve_project_id(
                &token,
                None,
                cred.project_id.clone(),
                cred.email.as_deref(),
            );
            match discovery::fetch_available_models_catalog(&token, &project).await {
                Ok(catalog) => match usage::fetch_account_usage(&token, &project).await {
                    Ok(usage_json) => {
                        println!("{}", usage::format_models_list(&usage_json, all));
                        0
                    }
                    Err(_) => {
                        // Fall back to a plain id list.
                        let fallback = catalog::fallback_catalog();
                        let grouped = catalog::build_catalog(
                            catalog
                                .pointer("/models")
                                .unwrap_or(&serde_json::Value::Null),
                            &fallback,
                        );
                        for m in grouped.models {
                            println!("{}", m.id);
                        }
                        0
                    }
                },
                Err(e) => {
                    eprintln!("[models] {}", security::safe_error(e));
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("[models] {}", security::safe_error(e));
            1
        }
    }
}

async fn usage_cmd(serve: &config::ServeArgs) -> i32 {
    match auth::fresh_credential(&serve.auth_path, &serve.accounts_path).await {
        Ok((cred, token)) => {
            let project = discovery::resolve_project_id(
                &token,
                None,
                cred.project_id.clone(),
                cred.email.as_deref(),
            );
            match usage::fetch_account_usage(&token, &project).await {
                Ok(usage_json) => {
                    println!("{}", usage::format_usage_summary(&usage_json));
                    0
                }
                Err(e) => {
                    eprintln!("[usage] {}", security::safe_error(e));
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("[usage] {}", security::safe_error(e));
            1
        }
    }
}

// Keep PathBuf import used on all platforms.
#[allow(dead_code)]
fn _typecheck(_: PathBuf) {}
