#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    if let Err(error) = run() {
        eprintln!("Codey 运行失败：{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let fastctx_server = std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--codey-fastctx-mcp");
    let mut builder = if fastctx_server {
        tokio::runtime::Builder::new_current_thread()
    } else {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        // Codey is an I/O coordinator. Blocking filesystem/SQLite work already
        // runs on Tokio's blocking pool, so two async workers avoid creating a
        // CPU-count-sized thread team for every helper instance.
        builder.worker_threads(2);
        builder
    };
    builder.enable_all().build()?.block_on(codey_lib::run())
}
