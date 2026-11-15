//! The `xai-screen` program, which does all desktop work for `screen_read` and
//! `screen_draw`. The tools only talk to the model and run it: `xai-screen read` answers
//! once, and `xai-screen draw` keeps its drawing on screen while it runs. Requests and
//! replies are single JSON lines. Dropping a [`ScreenProcess`] kills its process group.
//!
//! Build it with `just build xai-screen`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use xai_tool_runtime::{ToolError, ToolErrorKind};

use crate::util::{ProcessGroup, ProcessScope, global_process_scope};

const PROGRAM: &str = "xai-screen";

/// A running `xai-screen`.
pub(crate) struct ScreenProcess {
    child: Child,
    group: Arc<ProcessGroup>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl ScreenProcess {
    /// Start `xai-screen <mode>` in the session's process scope.
    pub(crate) fn spawn(mode: &str, scope: &ProcessScope) -> Result<Self, ToolError> {
        Self::start(&executable()?, mode, scope)
    }

    fn start(executable: &Path, mode: &str, scope: &ProcessScope) -> Result<Self, ToolError> {
        let mut command = Command::new(executable);
        command
            .arg(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let (mut child, group) = scope
            .spawn(command)
            .map_err(|error| failure(format!("Cannot start {}: {error}", executable.display())))?;
        if !global_process_scope().register(&group) {
            return Err(failure("The session is closing"));
        }
        let input = child.stdin.take().ok_or_else(|| failure("Missing stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| failure("Missing stdout"))?;
        Ok(Self {
            child,
            group,
            input,
            output: BufReader::new(output),
        })
    }

    pub(crate) fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Write one request line.
    pub(crate) async fn send(&mut self, request: &impl Serialize) -> Result<(), ToolError> {
        let mut line = serde_json::to_vec(request).map_err(failure)?;
        line.push(b'\n');
        self.input.write_all(&line).await.map_err(failure)?;
        self.input.flush().await.map_err(failure)
    }

    /// Read one reply line of at most `limit` bytes: the answer, or the error `xai-screen`
    /// reports, such as a drawing that breaks a rule.
    pub(crate) async fn receive<T: DeserializeOwned>(
        &mut self,
        limit: u64,
    ) -> Result<Result<T, String>, ToolError> {
        let mut line = Vec::new();
        (&mut self.output)
            .take(limit)
            .read_until(b'\n', &mut line)
            .await
            .map_err(failure)?;
        if line.last() != Some(&b'\n') {
            return Err(failure(if line.is_empty() {
                "xai-screen stopped without answering"
            } else {
                "xai-screen's answer is too long"
            }));
        }
        match serde_json::from_slice(&line).map_err(failure)? {
            Reply::Failed { error } => Ok(Err(error)),
            Reply::Done(answer) => Ok(Ok(answer)),
        }
    }
}

impl Drop for ScreenProcess {
    fn drop(&mut self) {
        // Kill the whole group: grim, the overlay surface, and with it the drawing. Only an
        // unreaped leader holds its group id, so a finished process is left alone.
        if self.running() {
            let _ = self.group.kill();
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Reply<T> {
    Failed { error: String },
    Done(T),
}

/// `GROK_SCREEN`, else `xai-screen` beside the running binary (or one directory up, for tests
/// and examples), else on PATH.
fn executable() -> Result<PathBuf, ToolError> {
    if let Some(path) = std::env::var_os("GROK_SCREEN") {
        return Ok(path.into());
    }
    std::env::current_exe()
        .ok()
        .and_then(|binary| {
            binary
                .ancestors()
                .skip(1)
                .take(2)
                .map(|directory| directory.join(PROGRAM))
                .find(|path| path.is_file())
        })
        .or_else(|| which::which(PROGRAM).ok())
        .ok_or_else(|| {
            failure(
                "Cannot find xai-screen; build it with `just build xai-screen` or set GROK_SCREEN",
            )
        })
}

pub(crate) fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(ToolErrorKind::Execution, format!("Screen: {error}"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde::Deserialize;
    use serde_json::json;

    use super::ScreenProcess;
    use crate::util::ProcessScope;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Drawn {
        monitor: String,
    }

    #[tokio::test]
    async fn requests_and_replies_are_json_lines() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("xai-screen");
        let script = "#!/bin/sh\n[ \"$1\" = draw ] || exit 1\nread line\n\
            echo '{\"monitor\": \"eDP-1\"}'\nread line\necho '{\"error\": \"Shape 1 (box): bad\"}'\n";
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let scope = ProcessScope::new();
        let mut screen = ScreenProcess::start(&program, "draw", &scope).unwrap();
        screen.send(&json!({"shapes": []})).await.unwrap();
        let drawn = screen.receive::<Drawn>(1024).await.unwrap();
        assert_eq!(drawn.unwrap().monitor, "eDP-1");
        screen.send(&json!({"shapes": []})).await.unwrap();
        let refused = screen.receive::<Drawn>(1024).await.unwrap();
        assert_eq!(refused.unwrap_err(), "Shape 1 (box): bad");
        let stopped = screen.receive::<Drawn>(1024).await.unwrap_err();
        assert!(stopped.to_string().contains("stopped without answering"));
    }
}
