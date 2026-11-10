//! Entry point for `grok-screen-overlay`, which shows `screen_draw` drawings on a Wayland
//! layer that lets clicks through, except on scenes, which orbit when dragged and zoom when
//! scrolled.
//!
//! Each stdin line is one `OverlayRequest` in JSON that replaces the drawing. The overlay
//! answers `{}` once the compositor shows it, or `{"error": "..."}`. Closing stdin or
//! killing the process removes the drawing.

#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod paint;
#[cfg(target_os = "linux")]
mod wayland;

/// Runs the overlay process and exits with status 1 after reporting a fatal error.
pub fn main() {
    #[cfg(target_os = "linux")]
    let result = wayland::run();
    #[cfg(not(target_os = "linux"))]
    let result: anyhow::Result<()> = Err(anyhow::anyhow!("Drawing requires Linux and Wayland"));
    if let Err(error) = result {
        println!("{}", serde_json::json!({ "error": format!("{error:#}") }));
        std::process::exit(1);
    }
}
