//! `--screen read`: print the visible windows of the focused monitor as one JSON line.
//!
//! Every window shown on the monitor is captured by itself with `grim -T`, so the wallpaper
//! and overlapping windows never appear in its screenshot. Terminals are left out.
//! Positions are logical units from the monitor's top-left corner. Chrome windows also carry
//! the visible page's accessibility text; see [`chrome`].

mod chrome;

use std::io::Write;

use anyhow::{Result, ensure};
use base64::Engine as _;
use serde::Serialize;

use crate::hyprland::{self, Client, Monitor};

/// Window classes of terminal emulators, which are never captured. A class also matches
/// with a dash suffix, such as `Alacritty-scratchpad`.
const TERMINALS: &[&str] = &[
    "Alacritty",
    "foot",
    "footclient",
    "kitty",
    "com.mitchellh.ghostty",
    "org.wezfurlong.wezterm",
    "org.gnome.Ptyxis",
    "org.gnome.Console",
    "org.gnome.Terminal",
    "org.kde.konsole",
    "XTerm",
    "URxvt",
    "st-256color",
    "Terminator",
    "com.gexperts.Tilix",
];
const MAX_WINDOWS: usize = 8;

/// The visible windows of the focused monitor, in reading order. It may be empty.
#[derive(Serialize)]
struct Screen {
    monitor: String,
    /// Logical monitor size.
    width: f64,
    height: f64,
    windows: Vec<Window>,
    /// Omissions and partial results.
    notes: Vec<String>,
}

/// One captured window.
#[derive(Serialize)]
struct Window {
    id: String,
    class: String,
    title: String,
    bounds: Bounds,
    /// The screenshot, a base64 PNG in output pixels.
    png: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    accessibility: Option<String>,
}

/// Position and size in logical units from the monitor's top-left corner.
#[derive(Serialize)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

pub fn run() -> Result<()> {
    let screen = read()?;
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &screen)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn read() -> Result<Screen> {
    let monitor = hyprland::active_monitor()?;
    let (width, height) = monitor.logical_size();
    let mut screen = Screen {
        monitor: monitor.name.clone(),
        width,
        height,
        windows: Vec::new(),
        notes: Vec::new(),
    };
    if hyprland::locked() {
        let note = "The screen is locked, so no windows were captured";
        screen.notes.push(note.to_owned());
        return Ok(screen);
    }
    let clients = hyprland::clients()?;
    let visible = readable(&monitor, &clients);
    if visible.len() > MAX_WINDOWS {
        screen.notes.push(format!(
            "Only the first {MAX_WINDOWS} of {} windows were captured",
            visible.len()
        ));
    }
    let captured = &visible[..visible.len().min(MAX_WINDOWS)];
    for client in captured {
        screen.windows.push(capture(client, &monitor)?);
    }
    chrome::describe(captured, &mut screen);

    // Windows are captured one by one, and bounds come from the first snapshot.
    // Reject a read during which the workspace, window set, or any geometry changed.
    let monitor_after = hyprland::active_monitor()?;
    let clients_after = hyprland::clients()?;
    ensure!(
        monitor_after == monitor && same_layout(&visible, &readable(&monitor, &clients_after)),
        "Windows changed during capture; call screen_read again"
    );
    Ok(screen)
}

/// Windows shown on `monitor`, minus terminals, in reading order.
fn readable<'a>(monitor: &Monitor, clients: &'a [Client]) -> Vec<&'a Client> {
    monitor
        .windows(clients)
        .into_iter()
        .filter(|client| !terminal(&client.class))
        .collect()
}

/// Whether `class` names a terminal, ignoring case.
fn terminal(class: &str) -> bool {
    TERMINALS.iter().any(|name| {
        class
            .get(..name.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(name))
            && matches!(class.as_bytes().get(name.len()), None | Some(b'-'))
    })
}

/// Whether two snapshots list the same windows at the same positions and sizes.
/// Titles may change freely; they do not affect the captured pixels' placement.
fn same_layout(before: &[&Client], after: &[&Client]) -> bool {
    fn layout<'a>(client: &&'a Client) -> (&'a str, [f64; 2], [f64; 2]) {
        (&client.stable_id, client.at, client.size)
    }
    before.iter().map(layout).eq(after.iter().map(layout))
}

fn capture(client: &Client, monitor: &Monitor) -> Result<Window> {
    let png = hyprland::capture(client)?;
    Ok(Window {
        id: client.stable_id.clone(),
        class: client.class.clone(),
        title: client.title.clone(),
        bounds: Bounds {
            x: client.at[0] - monitor.x,
            y: client.at[1] - monitor.y,
            width: client.size[0],
            height: client.size[1],
        },
        png: base64::engine::general_purpose::STANDARD.encode(png),
        accessibility: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{readable, same_layout};
    use crate::hyprland::{Client, Monitor};

    /// A 2560 × 1600 monitor at scale 2, so 1280 × 800 logical units, at x = 1000.
    fn monitor() -> Monitor {
        serde_json::from_value(json!({
            "id": 0, "name": "eDP-1", "x": 1000.0, "y": 0.0, "width": 2560.0, "height": 1600.0,
            "scale": 2.0, "transform": 0, "focused": true, "dpmsStatus": true,
            "activeWorkspace": {"id": 1}, "specialWorkspace": {"id": 0}
        }))
        .unwrap()
    }

    /// A shown 600 × 760 window on monitor 0.
    fn client(id: &str, class: &str, workspace: i64, at: [f64; 2]) -> Client {
        serde_json::from_value(json!({
            "stableId": id, "pid": 1, "mapped": true, "hidden": false, "pinned": false,
            "fullscreen": 0, "workspace": {"id": workspace}, "monitor": 0, "at": at,
            "size": [600.0, 760.0], "class": class, "title": id
        }))
        .unwrap()
    }

    fn ids<'a>(windows: &[&'a Client]) -> Vec<&'a str> {
        windows
            .iter()
            .map(|window| window.stable_id.as_str())
            .collect()
    }

    #[test]
    fn terminals_are_never_read() {
        let clients = [
            client("right", "dev.zed.Zed", 1, [1640.0, 36.0]),
            client("terminal", "Alacritty", 1, [1004.0, 36.0]),
            client("scratchpad", "Alacritty-scratchpad", 1, [1004.0, 400.0]),
            client("foot", "foot", 1, [1004.0, 400.0]),
            client("kitty", "kitty", 1, [1004.0, 400.0]),
            client("ghostty", "com.mitchellh.ghostty", 1, [1004.0, 400.0]),
            client("steam", "steam", 1, [1004.0, 600.0]),
            client("left", "google-chrome", 1, [1004.0, 36.0]),
        ];
        assert_eq!(
            ids(&readable(&monitor(), &clients)),
            ["left", "right", "steam"]
        );

        let mut terminal = client("terminal", "Alacritty", 1, [1000.0, 0.0]);
        terminal.fullscreen = 1;
        let clients = [
            client("covered", "dev.zed.Zed", 1, [1004.0, 36.0]),
            terminal,
        ];
        assert!(readable(&monitor(), &clients).is_empty());
    }

    #[test]
    fn moved_or_resized_windows_invalidate_a_capture() {
        let before = [client("a", "dev.zed.Zed", 1, [4.0, 36.0])];
        let mut renamed = client("a", "dev.zed.Zed", 1, [4.0, 36.0]);
        renamed.title = "new title".to_owned();
        assert!(same_layout(&[&before[0]], &[&renamed]));
        let moved = client("a", "dev.zed.Zed", 1, [8.0, 36.0]);
        assert!(!same_layout(&[&before[0]], &[&moved]));
        let mut resized = client("a", "dev.zed.Zed", 1, [4.0, 36.0]);
        resized.size = [700.0, 760.0];
        assert!(!same_layout(&[&before[0]], &[&resized]));
        assert!(!same_layout(&[&before[0]], &[]));
    }
}
