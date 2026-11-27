mod audio;
mod capture;
mod config;
mod detect;
mod rtmp;
mod ui;
pub mod win_enum;

fn main() -> eframe::Result<()> {
    // Force DirectX 12 rendering backend to bypass broken/hanging AMD Vulkan layer registry lookups
    if std::env::var("WGPU_BACKEND").is_err() {
        unsafe {
            std::env::set_var("WGPU_BACKEND", "dx12");
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("============================================================");
    println!("          ObsLite v0.5 Stable - Broadcast Studio            ");
    println!("============================================================");

    ui::launch_gui()
}
