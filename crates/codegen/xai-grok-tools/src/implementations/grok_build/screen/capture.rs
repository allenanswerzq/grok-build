//! Active-display capture on Hyprland, with optional accessibility from visible Chrome windows.

use std::io::Cursor;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use xai_tool_runtime::{ToolError, ToolErrorKind};

use super::chrome::ChromeApplication;
use super::{AccessibilityTree, ReadScreenOutput, ScreenReader, Screenshot, Viewport};
use crate::types::output::ImageContent;
use crate::util::{ProcessGroup, global_process_scope};

const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_TEXT: usize = 60_000;
const MAX_NODES: usize = 1500;
const MAX_WINDOWS: usize = 8;

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(ToolErrorKind::Execution, format!("Desktop screen: {error}"))
}

struct CommandGroup {
    group: Option<Arc<ProcessGroup>>,
}

impl Drop for CommandGroup {
    fn drop(&mut self) {
        if let Some(group) = &self.group {
            let _ = group.kill();
        }
    }
}

// These are trusted executable names and internally selected arguments, never
// model-supplied shell commands. Bound both pipes and kill canceled processes.
async fn command(program: &str, args: &[&str], limit: usize) -> Result<Vec<u8>, ToolError> {
    tokio::time::timeout(Duration::from_secs(8), async {
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
        let mut group = CommandGroup { group: Some(group) };
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure("Missing stdout"))?
            .take((limit + 1) as u64);
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| failure("Missing stderr"))?
            .take(8193);
        let mut data = Vec::new();
        let mut errors = Vec::new();
        tokio::try_join!(
            stdout.read_to_end(&mut data),
            stderr.read_to_end(&mut errors)
        )
        .map_err(failure)?;
        if data.len() > limit || errors.len() > 8192 {
            return Err(failure(format!(
                "{program} output exceeds the capture limit"
            )));
        }
        let status = child.wait().await.map_err(failure)?;
        // The leader has been reaped; release the registered group immediately
        // so neither cancellation nor shutdown can signal a reused PID.
        group.group = None;
        if !status.success() {
            return Err(failure(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&errors)
            )));
        }
        Ok(data)
    })
    .await
    .map_err(|_| ToolError::new(ToolErrorKind::Timeout, format!("{program} timed out")))?
}

#[derive(Debug, Deserialize, PartialEq)]
struct Workspace {
    id: i64,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Monitor {
    id: i64,
    name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    transform: u8,
    focused: bool,
    dpms_status: bool,
    active_workspace: Workspace,
    special_workspace: Workspace,
}

impl Monitor {
    fn viewport(&self) -> Result<Viewport, ToolError> {
        if ![self.x, self.y, self.width, self.height, self.scale]
            .iter()
            .all(|value| value.is_finite())
            || self.scale <= 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.transform > 7
        {
            return Err(failure("Invalid display geometry"));
        }
        let (width, height) = if self.transform % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let viewport = Viewport {
            width: width / self.scale,
            height: height / self.scale,
        };
        if !viewport.width.is_finite() || !viewport.height.is_finite() {
            return Err(failure("Invalid logical display geometry"));
        }
        Ok(viewport)
    }
}

#[derive(Debug, Deserialize)]
struct Window {
    mapped: bool,
    hidden: bool,
    pinned: bool,
    workspace: Workspace,
    monitor: i64,
    at: [f64; 2],
    size: [f64; 2],
    class: String,
    title: String,
}

fn visible(window: &Window, output: &Monitor, monitors: &[Monitor]) -> bool {
    let Some(owner) = monitors.iter().find(|monitor| monitor.id == window.monitor) else {
        return false;
    };
    let workspace_visible = window.pinned
        || window.workspace == owner.active_workspace
        || (owner.special_workspace.id != 0 && window.workspace == owner.special_workspace);
    let Ok(viewport) = output.viewport() else {
        return false;
    };
    window.mapped
        && !window.hidden
        && workspace_visible
        && owner.dpms_status
        && window
            .size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        && window.at.iter().all(|value| value.is_finite())
        && window.at[0] < output.x + viewport.width
        && window.at[1] < output.y + viewport.height
        && window.at[0] + window.size[0] > output.x
        && window.at[1] + window.size[1] > output.y
}

async fn monitors() -> Result<Vec<Monitor>, ToolError> {
    serde_json::from_slice(&command("hyprctl", &["-j", "monitors"], 1024 * 1024).await?)
        .map_err(failure)
}

fn active(monitors: &[Monitor]) -> Result<&Monitor, ToolError> {
    let mut focused = monitors
        .iter()
        .filter(|monitor| monitor.focused && monitor.dpms_status);
    let output = focused
        .next()
        .ok_or_else(|| failure("No powered-on active display"))?;
    if focused.next().is_some() {
        return Err(failure("Desktop reported more than one active display"));
    }
    Ok(output)
}

fn append(text: &mut String, value: &str) -> bool {
    let remaining = MAX_TEXT.saturating_sub(text.len());
    let mut end = value.len().min(remaining);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    text.push_str(&value[..end]);
    end < value.len()
}

#[derive(Debug)]
pub(super) struct DesktopScreen;

impl DesktopScreen {
    async fn read(&self) -> Result<ReadScreenOutput, ToolError> {
        let before = monitors().await?;
        let output = active(&before)?;
        let viewport = output.viewport()?;
        let scale = output.scale.to_string();
        let bytes = command("grim", &["-o", &output.name, "-s", &scale, "-"], MAX_IMAGE).await?;
        let decoder = image::ImageReader::with_format(Cursor::new(&bytes), image::ImageFormat::Png);
        let (pixel_width, pixel_height) = decoder.into_dimensions().map_err(failure)?;
        if pixel_width == 0 || pixel_height == 0 {
            return Err(failure("Empty screenshot"));
        }
        let mut coverage = vec![
            "Screenshot is the active display, including applications, browser controls, panels, and overlays".to_owned(),
            "Screenshot and overlay coordinates use output-local logical units; desktop origin is reported in the window context".to_owned(),
            "Accessibility currently covers visible Chrome windows only. It may include controls covered by other windows; screenshot pixels are authoritative. Chrome bounds remain page-local CSS coordinates, not desktop coordinates".to_owned(),
            "Screenshot, window context, and accessibility are sampled separately; animated content and occlusion are not atomic".to_owned(),
        ];
        let mut tree = AccessibilityTree {
            text: String::new(),
            node_count: 0,
            truncated: false,
        };
        append(
            &mut tree.text,
            &format!(
                "Display {:?}: desktop origin ({}, {}), scale {}\n",
                output.name, output.x, output.y, output.scale
            ),
        );
        // Do not expose application semantics behind an Omarchy lock screen.
        let locked = match tokio::time::timeout(
            Duration::from_secs(2),
            command("omarchy-shell", &["lock", "isLocked"], 64),
        )
        .await
        {
            Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).trim() == "true",
            _ => false,
        };
        if locked {
            coverage.push(
                "Desktop is locked; application context and accessibility were omitted".to_owned(),
            );
        } else {
            let context = self
                .accessibility(output, &before, &mut tree, &mut coverage)
                .await;
            if let Err(error) = context {
                coverage.push(format!("Application context unavailable: {error}"));
            }
        }
        let after = monitors().await?;
        if active(&after)? != output {
            return Err(failure(
                "Active display or workspace changed during capture; call read_screen again",
            ));
        }
        Ok(ReadScreenOutput {
            observation_id: uuid::Uuid::now_v7().to_string(),
            application: "desktop".to_owned(),
            target: output.name.clone(),
            title: format!("Display {}", output.name),
            viewport,
            accessibility: tree,
            screenshot: Screenshot {
                pixel_width,
                pixel_height,
                image: ImageContent {
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    mime_type: "image/png".to_owned(),
                    annotations: None,
                    uri: None,
                    meta: None,
                },
            },
            coverage,
        })
    }

    async fn accessibility(
        &self,
        output: &Monitor,
        monitors: &[Monitor],
        tree: &mut AccessibilityTree,
        coverage: &mut Vec<String>,
    ) -> Result<(), ToolError> {
        let clients: Vec<Window> =
            serde_json::from_slice(&command("hyprctl", &["-j", "clients"], 1024 * 1024).await?)
                .map_err(failure)?;
        let windows: Vec<_> = clients
            .iter()
            .filter(|window| visible(window, output, monitors))
            .collect();
        let mut titles = Vec::new();
        for window in windows.iter().take(MAX_WINDOWS) {
            tree.truncated |= append(
                &mut tree.text,
                &format!(
                    "Window {:?} {:?}: desktop bounds ({}, {}, {}, {})\n",
                    window.class,
                    window.title,
                    window.at[0],
                    window.at[1],
                    window.size[0],
                    window.size[1]
                ),
            );
            if window.class == "google-chrome" || window.class.starts_with("google-chrome-") {
                let title = window
                    .title
                    .strip_suffix(" - Google Chrome")
                    .unwrap_or(&window.title)
                    .to_owned();
                if !titles.contains(&title) {
                    titles.push(title);
                }
            }
        }
        tree.truncated |= windows.len() > MAX_WINDOWS;
        if titles.is_empty() {
            return Ok(());
        }
        let chrome = ChromeApplication::new("http://127.0.0.1:9222")?;
        let pages =
            tokio::time::timeout(Duration::from_secs(5), chrome.visible_pages(&titles)).await;
        let (pages, page_coverage) = match pages {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                coverage.push(format!("Chrome accessibility unavailable: {error}"));
                return Ok(());
            }
            Err(_) => {
                coverage.push(
                    "Chrome accessibility timed out; the display screenshot is still available"
                        .to_owned(),
                );
                return Ok(());
            }
        };
        coverage.extend(page_coverage);
        for page in pages {
            tree.truncated |= append(
                &mut tree.text,
                &format!(
                    "Chrome page {:?} ({}): CSS viewport {} × {}; bounds below are page-local\n",
                    page.title, page.target, page.viewport.width, page.viewport.height
                ),
            );
            for line in page.accessibility.text.lines() {
                if tree.node_count >= MAX_NODES || tree.text.len() >= MAX_TEXT {
                    tree.truncated = true;
                    break;
                }
                tree.truncated |= append(&mut tree.text, &format!("{line}\n"));
                tree.node_count += 1;
            }
            tree.truncated |= page.accessibility.truncated;
            coverage.extend(
                page.coverage
                    .into_iter()
                    .filter(|note| !note.starts_with("Chrome page viewport only")),
            );
        }
        Ok(())
    }
}

#[async_trait]
impl ScreenReader for DesktopScreen {
    async fn read_screen(&self) -> Result<ReadScreenOutput, ToolError> {
        if !cfg!(target_os = "linux") {
            return Err(failure(
                "Desktop capture currently requires Linux with Hyprland and grim",
            ));
        }
        tokio::time::timeout(Duration::from_secs(20), self.read())
            .await
            .map_err(|_| ToolError::new(ToolErrorKind::Timeout, "Desktop screen read timed out"))?
    }
}

#[cfg(test)]
mod tests {
    use super::{Monitor, Window, Workspace, active, append, command, visible};

    fn monitor() -> Monitor {
        Monitor {
            id: 0,
            name: "display".to_owned(),
            x: -1280.0,
            y: 0.0,
            width: 2560.0,
            height: 1600.0,
            scale: 2.0,
            transform: 0,
            focused: true,
            dpms_status: true,
            active_workspace: Workspace { id: 1 },
            special_workspace: Workspace { id: 0 },
        }
    }

    fn window() -> Window {
        Window {
            mapped: true,
            hidden: false,
            pinned: false,
            workspace: Workspace { id: 1 },
            monitor: 0,
            at: [-1200.0, 20.0],
            size: [600.0, 400.0],
            class: "google-chrome".to_owned(),
            title: "Visible page - Google Chrome".to_owned(),
        }
    }

    #[test]
    fn excludes_hidden_and_other_workspace_windows_without_using_dom_focus() {
        let outputs = [monitor()];
        let mut candidate = window();
        assert!(visible(&candidate, &outputs[0], &outputs));
        candidate.workspace.id = 2;
        assert!(!visible(&candidate, &outputs[0], &outputs));
        candidate.pinned = true;
        assert!(visible(&candidate, &outputs[0], &outputs));
        candidate.hidden = true;
        assert!(!visible(&candidate, &outputs[0], &outputs));
    }

    #[test]
    fn handles_special_workspaces_monitor_intersections_and_rotation() {
        let mut output = monitor();
        output.special_workspace.id = -2;
        let mut candidate = window();
        candidate.workspace.id = -2;
        assert!(visible(&candidate, &output, std::slice::from_ref(&output)));
        candidate.at[0] = 0.0;
        assert!(!visible(&candidate, &output, &[monitor()]));
        output.transform = 1;
        let viewport = output.viewport().unwrap();
        assert_eq!((viewport.width, viewport.height), (800.0, 1280.0));
        output.scale = f64::NAN;
        assert!(output.viewport().is_err());
    }

    #[test]
    fn powered_off_display_is_not_captured() {
        let mut output = monitor();
        output.dpms_status = false;
        assert!(active(&[output]).is_err());
    }

    #[test]
    fn text_budget_preserves_utf8_boundaries() {
        let mut text = "x".repeat(59_999);
        assert!(append(&mut text, "中文"));
        assert_eq!(text.len(), 59_999);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_command_rejects_oversized_output_and_nonzero_exit() {
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
    async fn cancellation_terminates_the_capture_command_and_its_descendants() {
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
        let pid = std::fs::read_to_string(pid_file).expect("Fixture must start its descendant");
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
        // Always clean up this fixture before reporting the regression.
        command("/usr/bin/kill", &["-KILL", &pid], 64)
            .await
            .unwrap();
        panic!("Cancelled capture left its descendant running");
    }
}
