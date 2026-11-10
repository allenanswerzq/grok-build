//! The Hyprland desktop that `screen_read` and `screen_draw` work with.
//!
//! [`Hyprland`] lists monitors and windows with `hyprctl`, captures windows with `grim`, and
//! reads Omarchy's lock state. Every program runs through [`command`], which bounds its
//! output and run time and kills its process group when the call is canceled.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use xai_tool_runtime::{ToolError, ToolErrorKind};

use crate::util::{ProcessGroup, global_process_scope};

const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 8192;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// The running Hyprland session.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hyprland;

impl Hyprland {
    /// The current session, or `None` outside a Hyprland session on Linux.
    pub(crate) fn session() -> Option<Self> {
        let running =
            cfg!(target_os = "linux") && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
        running.then_some(Self)
    }

    /// The focused, powered-on monitor, with valid geometry.
    pub(crate) async fn active_monitor(self) -> Result<Monitor, ToolError> {
        active(self.query("monitors").await?)
    }

    /// Every window on every workspace.
    pub(crate) async fn clients(self) -> Result<Vec<Client>, ToolError> {
        self.query("clients").await
    }

    /// A numeric option such as `general:border_size`, if Hyprland reports one.
    pub(crate) async fn option(self, name: &str) -> Option<f64> {
        let json = command("hyprctl", &["-j", "getoption", name], 4096)
            .await
            .ok()?;
        serde_json::from_slice::<serde_json::Value>(&json)
            .ok()?
            .get("int")?
            .as_f64()
    }

    /// Whether Omarchy's lock screen is up. Other setups count as unlocked.
    pub(crate) async fn locked(self) -> bool {
        let state = tokio::time::timeout(
            LOCK_TIMEOUT,
            command("omarchy-shell", &["lock", "isLocked"], 64),
        )
        .await;
        matches!(state, Ok(Ok(bytes)) if String::from_utf8_lossy(&bytes).trim() == "true")
    }

    /// A PNG of one window by itself, without the windows above it or the wallpaper.
    pub(crate) async fn capture(self, client: &Client) -> Result<Vec<u8>, ToolError> {
        let args = ["-T", client.stable_id.as_str(), "-t", "png", "-"];
        command("grim", &args, MAX_CAPTURE_BYTES).await
    }

    async fn query<T: DeserializeOwned>(self, request: &str) -> Result<T, ToolError> {
        let json = command("hyprctl", &["-j", request], MAX_JSON_BYTES).await?;
        serde_json::from_slice(&json).map_err(failure)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub(crate) id: i64,
}

/// A monitor as `hyprctl monitors` reports it. Position and size are in pixels.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Monitor {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) scale: f64,
    pub(crate) transform: u8,
    pub(crate) focused: bool,
    pub(crate) dpms_status: bool,
    pub(crate) active_workspace: Workspace,
    pub(crate) special_workspace: Workspace,
}

impl Monitor {
    /// Size in logical units: pixels divided by scale, swapped for rotated outputs.
    pub(crate) fn logical_size(&self) -> (f64, f64) {
        let (width, height) = if self.transform % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        (width / self.scale, height / self.scale)
    }

    /// Whether a point in logical units from the top-left corner is on the monitor.
    /// Points on the edge count.
    pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
        let (width, height) = self.logical_size();
        (0.0..=width).contains(&x) && (0.0..=height).contains(&y)
    }

    /// Windows shown on this monitor, sorted top to bottom, then left to right. A
    /// fullscreen window hides the other windows of its workspace; windows on the open
    /// special workspace and pinned windows stay visible.
    pub(crate) fn windows<'a>(&self, clients: &'a [Client]) -> Vec<&'a Client> {
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
pub(crate) struct Client {
    pub(crate) stable_id: String,
    pub(crate) pid: i64,
    pub(crate) mapped: bool,
    pub(crate) hidden: bool,
    pub(crate) pinned: bool,
    /// Nonzero for maximized or fullscreen windows, which cover their workspace.
    pub(crate) fullscreen: u8,
    pub(crate) workspace: Workspace,
    pub(crate) monitor: i64,
    pub(crate) at: [f64; 2],
    pub(crate) size: [f64; 2],
    pub(crate) class: String,
    pub(crate) title: String,
}

fn active(monitors: Vec<Monitor>) -> Result<Monitor, ToolError> {
    let mut focused = monitors
        .into_iter()
        .filter(|monitor| monitor.focused && monitor.dpms_status);
    let monitor = focused
        .next()
        .ok_or_else(|| failure("No powered-on active monitor"))?;
    if focused.next().is_some() {
        return Err(failure("Desktop reported more than one active monitor"));
    }
    if !monitor.valid() {
        return Err(failure("Invalid monitor geometry"));
    }
    Ok(monitor)
}

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(ToolErrorKind::Execution, format!("Screen: {error}"))
}

/// Kills a command's process group when a canceled call drops it.
struct CommandGroup(Option<Arc<ProcessGroup>>);

impl Drop for CommandGroup {
    fn drop(&mut self) {
        if let Some(group) = &self.0 {
            let _ = group.kill();
        }
    }
}

/// Run a fixed desktop program with internal arguments. Output is bounded and the
/// process group is killed on timeout or cancellation.
pub(crate) async fn command(
    program: &str,
    args: &[&str],
    limit: usize,
) -> Result<Vec<u8>, ToolError> {
    tokio::time::timeout(COMMAND_TIMEOUT, async {
        let mut executable = Command::new(program);
        executable
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (mut child, group) = global_process_scope()
            .spawn(executable)
            .map_err(|error| failure(format!("Cannot run {program}: {error}")))?;
        let mut group = CommandGroup(Some(group));
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("Missing stdout"))?
            .take((limit + 1) as u64);
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| failure("Missing stderr"))?
            .take(MAX_STDERR_BYTES as u64 + 1);
        let mut data = Vec::new();
        let mut errors = Vec::new();
        tokio::try_join!(
            stdout.read_to_end(&mut data),
            stderr.read_to_end(&mut errors)
        )
        .map_err(failure)?;
        if data.len() > limit || errors.len() > MAX_STDERR_BYTES {
            return Err(failure(format!("{program} output exceeds its limit")));
        }
        let status = child.wait().await.map_err(failure)?;
        // The leader is reaped; release the group so a reused PID is never signaled.
        group.0 = None;
        if !status.success() {
            return Err(failure(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&errors).trim()
            )));
        }
        Ok(data)
    })
    .await
    .map_err(|_| ToolError::new(ToolErrorKind::Timeout, format!("{program} timed out")))?
}

/// Sample desktop data for tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::json;

    use super::{Client, Monitor};

    /// A 2560 × 1600 monitor at scale 2, so 1280 × 800 logical units, at x = 1000.
    pub(crate) fn monitor() -> Monitor {
        serde_json::from_value(json!({
            "id": 0, "name": "eDP-1", "x": 1000.0, "y": 0.0, "width": 2560.0, "height": 1600.0,
            "scale": 2.0, "transform": 0, "focused": true, "dpmsStatus": true,
            "activeWorkspace": {"id": 1}, "specialWorkspace": {"id": 0}
        }))
        .unwrap()
    }

    /// A shown 600 × 760 window on monitor 0.
    pub(crate) fn client(id: &str, class: &str, workspace: i64, at: [f64; 2]) -> Client {
        serde_json::from_value(json!({
            "stableId": id, "pid": 1, "mapped": true, "hidden": false, "pinned": false, "fullscreen": 0,
            "workspace": {"id": workspace}, "monitor": 0, "at": at, "size": [600.0, 760.0],
            "class": class, "title": id
        }))
        .unwrap()
    }

    pub(crate) fn ids(windows: &[&Client]) -> Vec<String> {
        windows
            .iter()
            .map(|window| window.stable_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{client, ids, monitor};
    use super::{active, command};

    #[test]
    fn windows_follow_hyprland_visibility_in_reading_order() {
        let clients = [
            client("right", "dev.zed.Zed", 1, [1640.0, 36.0]),
            client("other-workspace", "google-chrome", 2, [1004.0, 36.0]),
            client("left", "google-chrome", 1, [1004.0, 36.0]),
        ];
        assert_eq!(ids(&monitor().windows(&clients)), ["left", "right"]);

        let mut output = monitor();
        output.special_workspace.id = -98;
        let mut pinned = client("pinned", "mpv", 3, [1100.0, 50.0]);
        pinned.pinned = true;
        let mut full = client("full", "google-chrome", 1, [1000.0, 0.0]);
        full.fullscreen = 2;
        let clients = [
            client("covered", "dev.zed.Zed", 1, [1004.0, 36.0]),
            client("special", "org.gnome.Nautilus", -98, [1200.0, 100.0]),
            pinned,
            full,
        ];
        assert_eq!(
            ids(&output.windows(&clients)),
            ["full", "pinned", "special"]
        );
    }

    #[test]
    fn active_monitor_has_logical_size_and_power() {
        let mut output = monitor();
        assert_eq!(output.logical_size(), (1280.0, 800.0));
        assert!(output.contains(1280.0, 0.0) && !output.contains(1280.1, 0.0));
        output.transform = 1;
        assert_eq!(output.logical_size(), (800.0, 1280.0));
        output.scale = f64::NAN;
        assert!(active(vec![output]).is_err());
        let mut asleep = monitor();
        asleep.dpms_status = false;
        assert!(active(vec![asleep]).is_err());
        assert!(active(vec![monitor()]).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_rejects_oversized_output_and_failure() {
        assert_eq!(
            command("/usr/bin/printf", &["%s", "ok"], 2).await.unwrap(),
            b"ok"
        );
        assert!(
            command("/usr/bin/printf", &["%s", "oversized"], 2)
                .await
                .is_err()
        );
        assert!(command("/bin/sh", &["-c", "exit 7"], 64).await.is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_kills_the_command_and_its_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("descendant.pid");
        let script = "sleep 60 >/dev/null 2>&1 & printf '%s' \"$!\" > \"$1\"; wait";
        let task = {
            let pid_file = pid_file.clone();
            tokio::spawn(async move {
                command(
                    "/bin/sh",
                    &["-c", script, "fixture", pid_file.to_str().unwrap()],
                    64,
                )
                .await
            })
        };
        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let pid = std::fs::read_to_string(pid_file).expect("fixture starts its descendant");
        let stat = format!("/proc/{pid}/stat");
        let running = || {
            std::fs::read_to_string(&stat).is_ok_and(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.split_whitespace().next() != Some("Z"))
            })
        };
        for _ in 0..100 {
            if !running() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        command("/usr/bin/kill", &["-KILL", &pid], 64)
            .await
            .unwrap();
        panic!("canceled command left descendant {pid} running");
    }
}
