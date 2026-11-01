//! Session-owned desktop overlay process. Rendering is independent of Chrome and CDP.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use xai_tool_runtime::{ToolError, ToolErrorKind};

use super::{DrawOverlayInput, DrawOverlayOutput, OverlayAction, OverlayRenderer};
use crate::util::{ProcessGroup, ProcessScope, global_process_scope};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REQUEST: usize = 1024 * 1024;
const MAX_RESPONSE: u64 = 64 * 1024;

fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(
        ToolErrorKind::Execution,
        format!("Desktop overlay: {error}"),
    )
}

/// Owns one native helper per tool session. Cancellation or session teardown closes its surface.
pub struct DesktopOverlay {
    executable: PathBuf,
    scope: ProcessScope,
    host: Mutex<Option<Host>>,
}

impl std::fmt::Debug for DesktopOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopOverlay")
            .field("executable", &self.executable)
            .finish_non_exhaustive()
    }
}

impl DesktopOverlay {
    /// Configure the native host executable without launching it.
    pub fn new(executable: &Path) -> Self {
        Self {
            executable: executable.to_owned(),
            scope: ProcessScope::new(),
            host: Mutex::new(None),
        }
    }

    /// Enroll the host in an externally supervised session scope.
    /// Configure this before the first draw; a closed scope refuses new hosts.
    pub fn with_process_scope(mut self, scope: ProcessScope) -> Self {
        self.scope = scope;
        self
    }

    /// Locate `grok-overlay-host` through GROK_OVERLAY_HOST, PATH, or beside the current binary.
    ///
    /// # Errors
    /// Returns an error when a native host is unavailable. Linux/Wayland is supported first.
    pub fn from_environment() -> Result<Self, ToolError> {
        if !cfg!(target_os = "linux") {
            return Err(failure(
                "Native desktop overlays currently require Linux/Wayland",
            ));
        }
        if let Some(path) = std::env::var_os("GROK_OVERLAY_HOST") {
            return Ok(Self::new(Path::new(&path)));
        }
        if let Ok(path) = which::which("grok-overlay-host") {
            return Ok(Self::new(&path));
        }
        if let Ok(executable) = std::env::current_exe() {
            for directory in executable
                .parent()
                .into_iter()
                .chain(executable.parent().and_then(Path::parent))
            {
                let path = directory.join("grok-overlay-host");
                if path.is_file() {
                    return Ok(Self::new(&path));
                }
            }
        }
        Err(failure(
            "Cannot find grok-overlay-host. Build screen/native/overlay.pro with qmake6 and make (see screen/README.md), or set GROK_OVERLAY_HOST to its executable",
        ))
    }

    async fn draw(&self, input: &DrawOverlayInput) -> Result<DrawOverlayOutput, ToolError> {
        if let OverlayAction::Show { scene } = &input.action {
            scene.validate()?;
        }
        let request = serde_json::to_vec(input).map_err(failure)?;
        if request.len() > MAX_REQUEST {
            return Err(ToolError::invalid_arguments(
                "Overlay request exceeds 1 MiB",
            ));
        }
        let mut state = self.host.lock().await;
        // Ownership leaves the slot during I/O. Dropping this future on cancellation
        // drops Host, terminating the helper instead of leaving an unacknowledged drawing.
        let mut host = match state.take() {
            Some(mut host) => {
                if host.child.try_wait().map_err(failure)?.is_none() {
                    host
                } else {
                    Host::spawn(&self.executable, &self.scope)?
                }
            }
            _ => Host::spawn(&self.executable, &self.scope)?,
        };
        match host.request(&request).await? {
            Response::Success {
                output,
                state: overlay_state,
            } => {
                *state = Some(host);
                Ok(DrawOverlayOutput {
                    output,
                    state: overlay_state,
                })
            }
            Response::Error {
                error,
                kind: ErrorKind::Rejected,
            } => {
                // The host rejected the request before changing its surface. Keep
                // the acknowledged scene and protocol stream available for correction.
                *state = Some(host);
                Err(ToolError::invalid_arguments(error))
            }
            Response::Error {
                error,
                kind: ErrorKind::Fatal,
            } => Err(failure(error)),
        }
    }
}

#[async_trait]
impl OverlayRenderer for DesktopOverlay {
    async fn draw_overlay(&self, input: &DrawOverlayInput) -> Result<DrawOverlayOutput, ToolError> {
        tokio::time::timeout(TIMEOUT, self.draw(input))
            .await
            .map_err(|_| ToolError::new(ToolErrorKind::Timeout, "Desktop overlay timed out"))?
    }
}

struct Host {
    child: Child,
    group: Arc<ProcessGroup>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Host {
    fn spawn(executable: &Path, scope: &ProcessScope) -> Result<Self, ToolError> {
        let mut command = Command::new(executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("QT_QPA_PLATFORM", "wayland")
            .env("QT_FORCE_STDERR_LOGGING", "1")
            .kill_on_drop(true);
        let (mut child, group) = scope
            .spawn(command)
            .map_err(|error| failure(format!("Cannot launch {}: {error}", executable.display())))?;
        if !global_process_scope().register(&group) {
            return Err(failure("Host process scope is closed"));
        }
        let input = child
            .stdin
            .take()
            .ok_or_else(|| failure("Missing host stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("Missing host stdout"))?;
        Ok(Self {
            child,
            group,
            input,
            output: BufReader::new(output),
        })
    }

    async fn request(&mut self, request: &[u8]) -> Result<Response, ToolError> {
        self.input.write_all(request).await.map_err(failure)?;
        self.input.write_all(b"\n").await.map_err(failure)?;
        self.input.flush().await.map_err(failure)?;
        let mut response = Vec::new();
        (&mut self.output)
            .take(MAX_RESPONSE + 1)
            .read_until(b'\n', &mut response)
            .await
            .map_err(failure)?;
        if response.len() as u64 > MAX_RESPONSE || response.last() != Some(&b'\n') {
            return Err(failure(
                "Native host disconnected or returned an oversized response; verify Qt WebEngine and LayerShellQt are installed",
            ));
        }
        serde_json::from_slice(&response).map_err(failure)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Qt WebEngine owns renderer subprocesses. Kill the complete group
        // before Child's kill-on-drop reaps its leader.
        let _ = self.group.kill();
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Response {
    Success {
        output: String,
        state: super::OverlayState,
    },
    Error {
        error: String,
        #[serde(default)]
        kind: ErrorKind,
    },
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorKind {
    Rejected,
    // Old helpers and errors without an explicit recovery guarantee fail closed.
    #[default]
    Fatal,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::DesktopOverlay;
    use crate::implementations::grok_build::screen::{
        Annotation, DrawOverlayInput, OverlayAction, OverlayRenderer, OverlayScene,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn show() -> DrawOverlayInput {
        DrawOverlayInput {
            output: Some("test-output".to_owned()),
            action: OverlayAction::Show {
                scene: OverlayScene::Annotations {
                    items: vec![Annotation::Text {
                        x: 10.0,
                        y: 20.0,
                        text: "Standalone".to_owned(),
                        color: "#ffffff".to_owned(),
                    }],
                },
            },
        }
    }

    fn helper(directory: &std::path::Path, script: &str) -> std::path::PathBuf {
        let path = directory.join("host");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[tokio::test]
    async fn native_protocol_reuses_a_session_process_and_tears_it_down() {
        let directory = tempfile::tempdir().unwrap();
        let executable = helper(
            directory.path(),
            r#"#!/bin/sh
printf '%s' "$$" > "$0.pid"
while IFS= read -r request; do
  case "$request" in
    *'"action":"clear"'*) printf '%s\n' '{"output":"test-output","state":"cleared"}' ;;
    *) printf '%s\n' '{"output":"test-output","state":"visible"}' ;;
  esac
done
"#,
        );
        let overlay = DesktopOverlay::new(&executable);
        assert_eq!(
            overlay.draw_overlay(&show()).await.unwrap().output,
            "test-output"
        );
        let pid = std::fs::read_to_string(executable.with_extension("pid")).unwrap();
        assert!(matches!(
            overlay
                .draw_overlay(&DrawOverlayInput {
                    output: None,
                    action: OverlayAction::Clear
                })
                .await
                .unwrap()
                .state,
            super::super::OverlayState::Cleared
        ));
        assert_eq!(
            std::fs::read_to_string(executable.with_extension("pid")).unwrap(),
            pid,
            "Show and clear must share a session host"
        );
        drop(overlay);
        for _ in 0..50 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("Session teardown left its native helper running");
    }

    #[tokio::test]
    async fn rejected_requests_preserve_the_session_host_and_scene() {
        let directory = tempfile::tempdir().unwrap();
        let executable = helper(
            directory.path(),
            r#"#!/bin/sh
printf '%s' "$$" > "$0.pid"
while IFS= read -r request; do
  case "$request" in
    *'"output":"missing"'*) printf '%s\n' '{"error":"Unknown output","kind":"rejected"}' ;;
    *'"action":"clear"'*) printf '%s\n' '{"output":"test-output","state":"cleared"}' ;;
    *) printf '%s\n' '{"output":"test-output","state":"visible"}' ;;
  esac
done
"#,
        );
        let overlay = DesktopOverlay::new(&executable);
        overlay.draw_overlay(&show()).await.unwrap();
        let pid = std::fs::read_to_string(executable.with_extension("pid")).unwrap();
        for action in [show().action, OverlayAction::Clear] {
            let error = overlay
                .draw_overlay(&DrawOverlayInput {
                    output: Some("missing".to_owned()),
                    action,
                })
                .await
                .unwrap_err();
            assert!(error.detail.contains("Unknown output"));
            let host = overlay.host.lock().await;
            assert_eq!(
                host.as_ref()
                    .and_then(|host| host.child.id())
                    .unwrap()
                    .to_string(),
                pid
            );
        }
        overlay
            .draw_overlay(&DrawOverlayInput {
                output: None,
                action: OverlayAction::Clear,
            })
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(executable.with_extension("pid")).unwrap(),
            pid
        );
    }

    #[tokio::test]
    async fn cancellation_terminates_an_unacknowledged_native_drawing() {
        let directory = tempfile::tempdir().unwrap();
        let executable = helper(
            directory.path(),
            r#"#!/bin/sh
printf '%s' "$$" > "$0.pid"
while IFS= read -r request; do :; done
"#,
        );
        let overlay = Arc::new(DesktopOverlay::new(&executable));
        let task = {
            let overlay = overlay.clone();
            tokio::spawn(async move { overlay.draw_overlay(&show()).await })
        };
        let pid_path = executable.with_extension("pid");
        for _ in 0..100 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(pid_path).unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            overlay.host.lock().await.is_none(),
            "Cancelled request must invalidate the protocol stream"
        );
        for _ in 0..50 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("Cancellation left its native helper running");
    }

    #[tokio::test]
    async fn native_errors_and_truncated_responses_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        for (script, expected) in [
            (
                "#!/bin/sh\nread -r request\nprintf '%s\\n' '{\"error\":\"Unknown output\"}'\n",
                "Unknown output",
            ),
            (
                "#!/bin/sh\nread -r request\nprintf '%s' '{}'\n",
                "disconnected",
            ),
        ] {
            let executable = helper(directory.path(), script);
            let overlay = DesktopOverlay::new(&executable);
            assert!(
                overlay
                    .draw_overlay(&show())
                    .await
                    .unwrap_err()
                    .detail
                    .contains(expected)
            );
            assert!(overlay.host.lock().await.is_none());
        }
    }

    #[tokio::test]
    async fn supervisor_closes_a_retained_overlay_and_refuses_restart() {
        let directory = tempfile::tempdir().unwrap();
        let executable = helper(
            directory.path(),
            "#!/bin/sh\nwhile IFS= read -r request; do printf '%s\\n' '{\"output\":\"test-output\",\"state\":\"visible\"}'; done\n",
        );
        let scope = crate::util::ProcessScope::new();
        let overlay = DesktopOverlay::new(&executable).with_process_scope(scope.clone());
        overlay.draw_overlay(&show()).await.unwrap();
        assert_eq!(scope.live_count(), 1);
        scope.kill_all();
        let mut host = overlay.host.lock().await.take().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(1), host.child.wait())
            .await
            .expect("Supervisor must terminate the host")
            .unwrap();
        assert!(!status.success());
        drop(host);
        assert!(
            overlay
                .draw_overlay(&show())
                .await
                .unwrap_err()
                .detail
                .contains("scope already closed")
        );
        assert_eq!(scope.live_count(), 0);
    }
}
