use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::host::WorkflowHostRequest;
use crate::journal::Journal;

pub struct WorkflowRunParams {
    pub script: String,
    pub args: serde_json::Value,
    pub journal: Journal,
    pub host_tx: mpsc::UnboundedSender<WorkflowHostRequest>,
    pub cancel: CancellationToken,
    pub max_ops: u64,
}

impl WorkflowRunParams {
    pub const DEFAULT_MAX_OPS: u64 = 100_000_000;
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::AsRefStr, strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PauseKind {
    User,
    BackOff,
    NoProgress,
    Verification,
    Infra,
}
impl std::str::FromStr for PauseKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "back_off" | "backoff" => Ok(Self::BackOff),
            "no_progress" => Ok(Self::NoProgress),
            "verification" | "blocked" => Ok(Self::Verification),
            "infra" => Ok(Self::Infra),
            other => Err(format!("unknown pause kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkflowOutcome {
    Completed { result: serde_json::Value },
    Paused { kind: PauseKind, message: String },
    BudgetExceeded { message: String },
    Cancelled,
    Failed { error: String },
}
