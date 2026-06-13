//! Client-facing Agent Teams types: the shared task board surfaced to UIs.
//!
//! These mirror `codex_protocol::team` but are owned by the app-server protocol
//! so the client API stays stable independently of the core types (the same
//! pattern as `TurnPlanStep` vs. the core plan types).

use codex_protocol::team::Task as CoreTask;
use codex_protocol::team::TaskStatus as CoreTaskStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum TeamTaskStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl From<CoreTaskStatus> for TeamTaskStatus {
    fn from(status: CoreTaskStatus) -> Self {
        match status {
            CoreTaskStatus::Pending => Self::Pending,
            CoreTaskStatus::InProgress => Self::InProgress,
            CoreTaskStatus::Completed => Self::Completed,
            CoreTaskStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    pub status: TeamTaskStatus,
    pub assignee: Option<String>,
    pub depends_on: Vec<String>,
}

impl From<CoreTask> for TeamTask {
    fn from(task: CoreTask) -> Self {
        Self {
            id: task.id.to_string(),
            title: task.title,
            status: task.status.into(),
            assignee: task.assignee.map(|name| name.to_string()),
            depends_on: task
                .depends_on
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        }
    }
}

/// Notification that a task on a team's shared board was created or changed.
/// Carries the full task so the client can render the board incrementally.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TeamTaskUpdatedNotification {
    pub thread_id: String,
    pub team_id: String,
    pub task: TeamTask,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::team::Task;
    use codex_protocol::team::TaskId;
    use codex_protocol::team::TaskStatus;
    use codex_protocol::team::TeammateName;

    #[test]
    fn converts_core_task_into_client_task() {
        let core = Task {
            id: TaskId::from("t1"),
            title: "investigate".to_string(),
            details: Some("ignored by the client view".to_string()),
            status: TaskStatus::InProgress,
            assignee: Some(TeammateName::from("researcher")),
            depends_on: vec![TaskId::from("t0")],
            created_by: TeammateName::from("lead"),
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let client: TeamTask = core.into();
        assert_eq!(client.id, "t1");
        assert_eq!(client.title, "investigate");
        assert_eq!(client.status, TeamTaskStatus::InProgress);
        assert_eq!(client.assignee.as_deref(), Some("researcher"));
        assert_eq!(client.depends_on, vec!["t0".to_string()]);
    }
}
