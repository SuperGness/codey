#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    codey_lib::install_crash_log_hook("codey", "runtime.codey");
    if let Err(error) = run() {
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

fn run() -> anyhow::Result<()> {
    if codey_lib::run_fastctx_route_hook_if_requested()? {
        return Ok(());
    }
    if codey_lib::run_subagent_gate_hook_if_requested()? {
        return Ok(());
    }
    if codey_lib::run_error_log_helper_if_requested()? {
        return Ok(());
    }
    if codey_lib::run_update_helper_if_requested()? {
        return Ok(());
    }
    codey_lib::run_desktop_application()
}
