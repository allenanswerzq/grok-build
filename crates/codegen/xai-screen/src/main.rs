//! `xai-screen` does all desktop work for the `screen_read` and `screen_draw` tools.
//!
//! - `xai-screen read` prints the visible windows of the focused monitor as one JSON line.
//! - `xai-screen draw` reads drawings as JSON lines and shows each above all windows.
//!
//! Fatal errors print `{"error": "..."}` and exit with status 1.
//!
//! - [`hyprland`]: the only code that talks to the desktop.
//! - [`read`]: window screenshots and Chrome accessibility text.
//! - [`draw`]: the drawing model and the Wayland overlay that shows it.

#![cfg(target_os = "linux")]

mod draw;
mod hyprland;
mod read;

fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        Some("read") => read::run(),
        Some("draw") => draw::run(),
        _ => Err(anyhow::anyhow!("Usage: xai-screen read|draw")),
    };
    if let Err(error) = result {
        println!("{}", serde_json::json!({ "error": format!("{error:#}") }));
        std::process::exit(1);
    }
}
