//! The desktop: Hyprland's monitors and windows, window captures with `grim`, Omarchy's lock
//! screen, and the look drawings follow. Every program runs through [`run`], which bounds its
//! output and run time.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::draw::model::{Palette, Style, hex};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_QUERY: u64 = 1024 * 1024;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CAPTURE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: i64,
}

/// A monitor as `hyprctl monitors` reports it. Position and size are in pixels.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub transform: u8,
    pub focused: bool,
    pub dpms_status: bool,
    pub active_workspace: Workspace,
    pub special_workspace: Workspace,
}

impl Monitor {
    /// Size in logical units: pixels divided by scale, swapped for rotated outputs.
    pub fn logical_size(&self) -> (f64, f64) {
        let (width, height) = if self.transform % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        (width / self.scale, height / self.scale)
    }

    /// Windows shown on this monitor, sorted top to bottom, then left to right. A
    /// fullscreen window hides the other windows of its workspace; windows on the open
    /// special workspace and pinned windows stay visible.
    pub fn windows<'a>(&self, clients: &'a [Client]) -> Vec<&'a Client> {
        let shown = |client: &&Client| {
            client.monitor == self.id
                && client.mapped
                && !client.hidden
                && client.at.iter().all(|value| value.is_finite())
                && client
                    .size
                    .iter()
                    .all(|value| value.is_finite() && *value > 0.0)
        };
        let special = |client: &Client| {
            self.special_workspace.id != 0 && client.workspace == self.special_workspace
        };
        let regular = |client: &Client| client.workspace == self.active_workspace;
        let covered = clients
            .iter()
            .filter(shown)
            .any(|client| regular(client) && client.fullscreen != 0);
        let mut windows: Vec<_> = clients
            .iter()
            .filter(shown)
            .filter(|client| {
                special(client)
                    || client.pinned
                    || (regular(client) && (!covered || client.fullscreen != 0))
            })
            .collect();
        windows.sort_by(|a, b| {
            a.at[1]
                .total_cmp(&b.at[1])
                .then(a.at[0].total_cmp(&b.at[0]))
        });
        windows
    }

    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height, self.scale]
            .iter()
            .all(|value| value.is_finite())
            && self.scale > 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.transform <= 7
    }
}

/// A window as `hyprctl clients` reports it. Position and size are in logical units
/// across the whole desktop.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub stable_id: String,
    pub pid: i64,
    pub mapped: bool,
    pub hidden: bool,
    pub pinned: bool,
    /// Nonzero for maximized or fullscreen windows, which cover their workspace.
    pub fullscreen: u8,
    pub workspace: Workspace,
    pub monitor: i64,
    pub at: [f64; 2],
    pub size: [f64; 2],
    pub class: String,
    pub title: String,
}

/// The focused, powered-on monitor, with valid geometry.
pub fn active_monitor() -> Result<Monitor> {
    active(query("monitors")?)
}

/// Every window on every workspace.
pub fn clients() -> Result<Vec<Client>> {
    query("clients")
}

/// Whether Omarchy's lock screen is up. Other setups count as unlocked.
pub fn locked() -> bool {
    run(
        "omarchy-shell",
        &["lock", "isLocked"],
        MAX_QUERY,
        QUERY_TIMEOUT,
    )
    .is_ok_and(|state| String::from_utf8_lossy(&state).trim() == "true")
}

/// A PNG of one window by itself, without the windows above it or the wallpaper.
pub fn capture(client: &Client) -> Result<Vec<u8>> {
    let args = ["-T", client.stable_id.as_str(), "-t", "png", "-"];
    run("grim", &args, MAX_CAPTURE, CAPTURE_TIMEOUT)
}

/// The look drawings follow, read on every request so theme changes apply at once. Settings
/// that cannot be read keep their defaults.
pub fn style() -> Style {
    let default = Style::default();
    Style {
        line_width: option("general:border_size")
            .map_or(default.line_width, |width| width.clamp(1.0, 6.0)),
        rounding: option("decoration:rounding")
            .map_or(default.rounding, |radius| radius.clamp(0.0, 20.0)),
        font: monospace_font().unwrap_or(default.font),
        font_size: default.font_size,
        palette: theme_palette().unwrap_or(default.palette),
    }
}

fn query<T: DeserializeOwned>(request: &str) -> Result<T> {
    ensure!(
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
        "The screen tools require a Hyprland session"
    );
    let json = run("hyprctl", &["-j", request], MAX_QUERY, QUERY_TIMEOUT)?;
    serde_json::from_slice(&json).with_context(|| format!("Cannot read Hyprland's {request}"))
}

fn active(monitors: Vec<Monitor>) -> Result<Monitor> {
    let mut focused = monitors
        .into_iter()
        .filter(|monitor| monitor.focused && monitor.dpms_status);
    let monitor = focused.next().context("No powered-on active monitor")?;
    ensure!(
        focused.next().is_none(),
        "Hyprland reported more than one active monitor"
    );
    ensure!(monitor.valid(), "Invalid monitor geometry");
    Ok(monitor)
}

/// A numeric Hyprland option such as `general:border_size`.
fn option(name: &str) -> Option<f64> {
    let json = run(
        "hyprctl",
        &["-j", "getoption", name],
        MAX_QUERY,
        QUERY_TIMEOUT,
    )
    .ok()?;
    serde_json::from_slice::<serde_json::Value>(&json)
        .ok()?
        .get("int")?
        .as_f64()
}

fn theme_palette() -> Option<Palette> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
    let colors = std::fs::read_to_string(state.join("omarchy/current/theme/colors.toml")).ok()?;
    Some(parse_palette(&colors))
}

fn monospace_font() -> Option<String> {
    let args = ["-f", "%{family[0]}", "monospace"];
    let name = run("fc-match", &args, MAX_QUERY, QUERY_TIMEOUT).ok()?;
    let name = String::from_utf8(name).ok()?.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Read `name = "#rrggbb"` lines of an Omarchy `colors.toml`. Colors it does not define keep
/// their defaults.
fn parse_palette(colors: &str) -> Palette {
    let mut palette = Palette::default();
    for line in colors.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let Some(rgb) = hex(value.trim().trim_matches('"')) else {
            continue;
        };
        let slot = match name.trim() {
            "accent" => &mut palette.accent,
            "red" => &mut palette.red,
            "yellow" => &mut palette.yellow,
            "green" => &mut palette.green,
            "blue" => &mut palette.blue,
            "background" => &mut palette.background,
            "foreground" => &mut palette.foreground,
            _ => continue,
        };
        *slot = rgb;
    }
    palette
}

/// Run a desktop program and return what it prints. It is killed when it runs past
/// `timeout` or prints more than `limit` bytes.
fn run(program: &str, args: &[&str], limit: u64, timeout: Duration) -> Result<Vec<u8>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "children join the screen helper's process group, which the tool enrolls in its session"
    )]
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Cannot run {program}"))?;
    let mut stdout = child.stdout.take().context("Missing stdout")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let read = (&mut stdout).take(limit + 1).read_to_end(&mut output);
        let _ = sender.send(read.map(|_| output));
    });
    let output = receiver.recv_timeout(timeout);
    if !matches!(&output, Ok(Ok(output)) if output.len() as u64 <= limit) {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let output = output.map_err(|_| anyhow!("{program} timed out"))??;
    ensure!(
        output.len() as u64 <= limit,
        "{program} output exceeds its limit"
    );
    ensure!(status.success(), "{program} failed: {status}");
    Ok(output)
}
