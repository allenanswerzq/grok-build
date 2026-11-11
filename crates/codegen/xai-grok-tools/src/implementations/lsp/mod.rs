pub mod capabilities;
pub mod client;
pub mod config;
pub mod diagnostics;
pub mod dispatch;
pub mod documents;
pub mod format;
pub mod manager;
pub mod pending;
pub mod pull;
pub mod refresh;
pub mod restart;
mod types;
mod watched_files;
pub mod workspace_open;

#[cfg(test)]
mod tests;

pub use dispatch::LspBackendAdapter;
pub use manager::{DiagnosticsSummary, LspManager, drain_lsp_diagnostics};
pub use restart::restart_monitor;
pub use types::{
    DiagnosticEntry, DiagnosticSeverityLevel, DiskChangeKind, FileDiagnosticEntry, LspBackend,
    LspConfig, LspOperation, LspToolInput, LspToolResult,
};

// ── Shared types used across submodules ─────────────────────────────────

use std::path::Path;
use std::sync::Arc;

use async_lsp::lsp_types::{Position, TextDocumentIdentifier, TextDocumentPositionParams, Url};

/// How long a reader will wait for diagnostics to arrive after an edit before reporting what it has. This is the budget the whole after-edit
/// diagnostics path is sized against: anything scheduled to happen later than this — a pull retry, say — answers after the reader has already
/// given up. Kept here, next to the pieces that have to agree on it, rather than as a number at the call site.
pub const DIAGNOSTICS_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("failed to spawn LSP server: {0}")]
    SpawnFailed(String),
    #[error("LSP server '{0}' timed out after {1:?}")]
    Timeout(String, std::time::Duration),
    #[error("LSP initialization failed: {0}")]
    InitFailed(String),
    #[error("LSP request failed: {0}")]
    RequestFailed(String),
    #[error("invalid file path")]
    InvalidPath,
}

pub type DiagnosticsNotify = Arc<tokio::sync::Notify>;
pub type LspMainLoop = async_lsp::MainLoop<async_lsp::router::Router<()>>;

/// Starts language servers for `servers` in the background, or returns `None` when there are none
/// or this build has no LSP client (the `lsp` feature).
pub fn start(
    servers: std::collections::BTreeMap<String, config::LspServerConfig>,
    workspace_root: std::path::PathBuf,
    process_scope: Option<crate::util::ProcessScope>,
) -> Option<Arc<dyn LspBackend>> {
    if !cfg!(feature = "lsp") || servers.is_empty() {
        return None;
    }
    let manager = LspManager::new(
        servers,
        workspace_root,
        true,
        crate::notification::ToolNotificationHandle::noop(),
    )
    .with_process_scope(process_scope);
    let adapter = Arc::new(LspBackendAdapter::new(Arc::new(tokio::sync::Mutex::new(
        manager,
    ))));
    adapter.ensure_started_background();
    Some(adapter)
}

pub fn file_uri(path: &Path) -> Result<Url, LspError> {
    Url::from_file_path(path).map_err(|_| LspError::InvalidPath)
}

pub fn text_document_position(
    path: &Path,
    line: u32,
    column: u32,
) -> Result<TextDocumentPositionParams, LspError> {
    Ok(TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: file_uri(path)?,
        },
        position: Position {
            line,
            character: column,
        },
    })
}
