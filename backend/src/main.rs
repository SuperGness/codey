#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    if let Err(error) = codey_lib::run().await {
        eprintln!("Codey 运行失败：{error:#}");
        std::process::exit(1);
    }
}
