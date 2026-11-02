//! Session-owned Wayland overlay host. Tool arguments remain the existing Rust types.

#[cfg(target_os = "linux")]
mod host;
#[cfg(all(test, feature = "live-tests", target_os = "linux"))]
mod live_tests;
#[cfg(target_os = "linux")]
mod render;
#[cfg(target_os = "linux")]
mod scene;
#[cfg(all(test, target_os = "linux"))]
mod tests;

fn main() {
    #[cfg(target_os = "linux")]
    if let Err(error) = host::run() {
        eprintln!("grok-overlay-host: {error:#}");
        let _ = serde_json::to_writer(
            std::io::stdout().lock(),
            &serde_json::json!({"error":format!("{error:#}"),"kind":"fatal"}),
        );
        println!();
        std::process::exit(1);
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Desktop overlays currently require Linux/Wayland");
        std::process::exit(1);
    }
}
