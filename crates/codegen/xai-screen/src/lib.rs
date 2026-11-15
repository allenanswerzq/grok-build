//! All desktop work for the `screen_read` and `screen_draw` tools, run as the `--screen` helper
//! of the binary that registers [`ARG0_ENTRY`] (see `xai-arg0`).
//!
//! - `--screen read` prints the visible windows of the focused monitor as one JSON line.
//! - `--screen draw` reads drawings as JSON lines and shows each above all windows.
//! - `--screen clear` draws with no shapes.
//!
//! Fatal errors print `{"error": "..."}` and exit with status 1.
//!
//! - [`hyprland`]: the only code that talks to the desktop.
//! - [`read`]: window screenshots and Chrome accessibility text.
//! - [`draw`]: the drawing model and the Wayland overlay that shows it.

#![cfg(target_os = "linux")]

use std::ffi::OsString;

use xai_arg0::Entry;

mod draw;
mod hyprland;
mod read;

/// The screen helper's arg0 entry; register it at the top of `main` with `xai_arg0::dispatch`.
pub const ARG0_ENTRY: Entry = Entry {
    name: "--screen",
    run,
};

fn run(args: Vec<OsString>) -> i32 {
    let result = match args.first().and_then(|mode| mode.to_str()) {
        Some("read") => read::run(),
        Some("draw") => draw::run(),
        Some("clear") => draw::clear(),
        _ => Err(anyhow::anyhow!("Usage: --screen read|draw|clear")),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            println!("{}", serde_json::json!({ "error": format!("{error:#}") }));
            1
        }
    }
}
