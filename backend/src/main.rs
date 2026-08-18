#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app_server_proxy;

fn main() {
    codey_lib::install_crash_log_hook("codey", "runtime.codey");
    match run() {
        Ok(Some(exit_code)) if exit_code != 0 => std::process::exit(exit_code),
        Ok(_) => {}
        Err(error) => {
            let error = format!("{error:#}");
            codey_lib::record_process_failure(
                "process_failed",
                "run_codey",
                error.clone(),
                "runtime.codey",
            );
            eprintln!("Codey 运行失败：{error}");
            std::process::exit(1);
        }
    }
}

fn run() -> anyhow::Result<Option<i32>> {
    if let Some(exit_code) = app_server_proxy::run_helper_if_requested()? {
        return Ok(Some(exit_code));
    }
    if codey_lib::run_fastctx_route_hook_if_requested()? {
        return Ok(None);
    }
    if codey_lib::run_subagent_gate_hook_if_requested()? {
        return Ok(None);
    }
    if codey_lib::run_error_log_helper_if_requested()? {
        return Ok(None);
    }
    if codey_lib::run_update_helper_if_requested()? {
        return Ok(None);
    }
    codey_lib::run_desktop_application()?;
    Ok(None)
}
