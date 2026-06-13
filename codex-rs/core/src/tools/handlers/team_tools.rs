//! Model-visible tools for the Agent Teams shared task board (PR-4).
//!
//! These let the lead and teammates coordinate through the board carried by
//! `AgentControl` (PR-3): create tasks, claim them (compare-and-set, race-free),
//! update status (unblocking dependents), list the board, and read the roster.
//! They are registered behind the MultiAgentV2 gate alongside the other collab
//! tools (see `spec_plan.rs`).

use crate::function_tool::FunctionCallError;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::function_arguments;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::protocol::EventMsg;
use codex_protocol::team::Task;
use codex_protocol::team::TaskCreatedEvent;
use codex_protocol::team::TaskId;
use codex_protocol::team::TaskStatus;
use codex_protocol::team::TaskUnblockedEvent;
use codex_protocol::team::TaskUpdatedEvent;
use codex_protocol::team::TeamId;
use codex_protocol::team::TeammateName;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Bound on free-text task fields, enforced at the tool boundary so unbounded
/// content never reaches the board or, later, a teammate's context (AGENTS.md
/// model-visible-context rule).
const MAX_TITLE_LEN: usize = 200;
const MAX_DETAILS_LEN: usize = 4096;

/// Team-local name of the calling agent, used as task author/assignee.
fn caller_name(turn: &TurnContext) -> TeammateName {
    turn.session_source
        .get_nickname()
        .or_else(|| turn.session_source.get_agent_path().map(|p| p.to_string()))
        .map(TeammateName::from)
        .unwrap_or_else(|| TeammateName::from("lead"))
}

/// Identifier for the team (the spawn subtree), shared by all its agents.
fn team_id(invocation: &ToolInvocation) -> TeamId {
    TeamId::from(
        invocation
            .session
            .services
            .agent_control
            .session_id()
            .to_string(),
    )
}

fn task_json(task: &Task) -> Result<String, FunctionCallError> {
    serde_json::to_string(task)
        .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize task: {err}")))
}

fn parse_status(raw: &str) -> Result<TaskStatus, FunctionCallError> {
    match raw {
        "pending" => Ok(TaskStatus::Pending),
        "in_progress" => Ok(TaskStatus::InProgress),
        "completed" => Ok(TaskStatus::Completed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => Err(FunctionCallError::RespondToModel(format!(
            "invalid status '{other}'; expected pending|in_progress|completed|cancelled"
        ))),
    }
}

// ---------------------------------------------------------------------------
// task_create
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskCreateArgs {
    title: String,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
}

pub struct TaskCreateHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TaskCreateHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("task_create")
    }

    fn spec(&self) -> ToolSpec {
        let properties = BTreeMap::from([
            (
                "title".to_string(),
                JsonSchema::string(Some("Short summary of the task (<=200 chars).".to_string())),
            ),
            (
                "details".to_string(),
                JsonSchema::string(Some("Optional longer description.".to_string())),
            ),
            (
                "depends_on".to_string(),
                JsonSchema::array(
                    JsonSchema::string(Some("Task id this task depends on.".to_string())),
                    Some(
                        "Task ids that must be completed before this task is claimable."
                            .to_string(),
                    ),
                ),
            ),
        ]);
        ToolSpec::Function(ResponsesApiTool {
            name: "task_create".to_string(),
            description: "Add a task to the shared team board.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["title".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let team_id = team_id(&invocation);
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: TaskCreateArgs = parse_arguments(&function_arguments(payload)?)?;
        if args.title.trim().is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "task title must not be empty".to_string(),
            ));
        }
        if args.title.len() > MAX_TITLE_LEN {
            return Err(FunctionCallError::RespondToModel(format!(
                "task title exceeds {MAX_TITLE_LEN} characters"
            )));
        }
        if let Some(details) = &args.details {
            if details.len() > MAX_DETAILS_LEN {
                return Err(FunctionCallError::RespondToModel(format!(
                    "task details exceed {MAX_DETAILS_LEN} characters"
                )));
            }
        }

        let depends_on = args.depends_on.into_iter().map(TaskId::from).collect();
        let task = session
            .services
            .agent_control
            .team_board()
            .create(
                args.title,
                args.details,
                caller_name(turn.as_ref()),
                depends_on,
            )
            .await;

        session
            .send_event(
                turn.as_ref(),
                EventMsg::TaskCreated(TaskCreatedEvent {
                    team_id,
                    task: task.clone(),
                }),
            )
            .await;

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            task_json(&task)?,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for TaskCreateHandler {}

// ---------------------------------------------------------------------------
// task_claim
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdArgs {
    task_id: String,
}

pub struct TaskClaimHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TaskClaimHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("task_claim")
    }

    fn spec(&self) -> ToolSpec {
        single_task_id_spec(
            "task_claim",
            "Claim a pending, unblocked task for yourself.",
        )
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let team_id = team_id(&invocation);
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: TaskIdArgs = parse_arguments(&function_arguments(payload)?)?;
        let id = TaskId::from(args.task_id);
        let task = session
            .services
            .agent_control
            .team_board()
            .claim(&id, caller_name(turn.as_ref()))
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        session
            .send_event(
                turn.as_ref(),
                EventMsg::TaskUpdated(TaskUpdatedEvent {
                    team_id,
                    task: task.clone(),
                }),
            )
            .await;

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            task_json(&task)?,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for TaskClaimHandler {}

// ---------------------------------------------------------------------------
// task_update
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskUpdateArgs {
    task_id: String,
    status: String,
}

pub struct TaskUpdateHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TaskUpdateHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("task_update")
    }

    fn spec(&self) -> ToolSpec {
        let properties = BTreeMap::from([
            (
                "task_id".to_string(),
                JsonSchema::string(Some("Id of the task to update.".to_string())),
            ),
            (
                "status".to_string(),
                JsonSchema::string_enum(
                    vec![
                        serde_json::json!("pending"),
                        serde_json::json!("in_progress"),
                        serde_json::json!("completed"),
                        serde_json::json!("cancelled"),
                    ],
                    Some("New status for the task.".to_string()),
                ),
            ),
        ]);
        ToolSpec::Function(ResponsesApiTool {
            name: "task_update".to_string(),
            description: "Update a task's status. Completing a task unblocks its dependents."
                .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["task_id".to_string(), "status".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let team_id = team_id(&invocation);
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: TaskUpdateArgs = parse_arguments(&function_arguments(payload)?)?;
        let status = parse_status(&args.status)?;
        let id = TaskId::from(args.task_id);
        let (task, unblocked) = session
            .services
            .agent_control
            .team_board()
            .update(&id, status)
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        session
            .send_event(
                turn.as_ref(),
                EventMsg::TaskUpdated(TaskUpdatedEvent {
                    team_id: team_id.clone(),
                    task: task.clone(),
                }),
            )
            .await;
        for task_id in unblocked {
            session
                .send_event(
                    turn.as_ref(),
                    EventMsg::TaskUnblocked(TaskUnblockedEvent {
                        team_id: team_id.clone(),
                        task_id,
                    }),
                )
                .await;
        }

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            task_json(&task)?,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for TaskUpdateHandler {}

// ---------------------------------------------------------------------------
// task_list
// ---------------------------------------------------------------------------

pub struct TaskListHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TaskListHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("task_list")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "task_list".to_string(),
            description: "List all tasks on the shared team board.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(BTreeMap::new(), None, Some(false.into())),
            output_schema: None,
        })
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let tasks = invocation
            .session
            .services
            .agent_control
            .team_board()
            .list()
            .await;
        let body = serde_json::to_string(&tasks)
            .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize board: {err}")))?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            body,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for TaskListHandler {}

// ---------------------------------------------------------------------------
// team_roster
// ---------------------------------------------------------------------------

pub struct TeamRosterHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TeamRosterHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("team_roster")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "team_roster".to_string(),
            description: "List the live teammates on this team and their status.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(BTreeMap::new(), None, Some(false.into())),
            output_schema: None,
        })
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolInvocation { session, turn, .. } = invocation;
        let agents = session
            .services
            .agent_control
            .list_agents(&turn.session_source, /*path_prefix*/ None)
            .await
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let body = serde_json::to_string(&agents).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize roster: {err}"))
        })?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            body,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for TeamRosterHandler {}

// ---------------------------------------------------------------------------
// shared spec helper
// ---------------------------------------------------------------------------

fn single_task_id_spec(name: &str, description: &str) -> ToolSpec {
    let properties = BTreeMap::from([(
        "task_id".to_string(),
        JsonSchema::string(Some("Id of the task.".to_string())),
    )]);
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["task_id".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session::Session;
    use crate::session::tests::make_session_and_context;
    use crate::tools::context::ToolCallSource;
    use crate::tools::context::ToolPayload;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn invoke(
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        name: &'static str,
        args: serde_json::Value,
    ) -> ToolInvocation {
        ToolInvocation {
            session: Arc::clone(session),
            turn: Arc::clone(turn),
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
            call_id: format!("call-{name}"),
            tool_name: ToolName::plain(name),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: args.to_string(),
            },
        }
    }

    /// Drives the real Agent Teams tool handlers (as the model would invoke
    /// them) through a real `Session`: create -> dependency-gated claim ->
    /// complete -> unblocked claim, asserting the shared board state.
    #[tokio::test]
    async fn task_tools_drive_the_board_end_to_end() {
        let (session, turn) = make_session_and_context().await;
        let session = Arc::new(session);
        let turn = Arc::new(turn);

        TaskCreateHandler
            .handle(invoke(
                &session,
                &turn,
                "task_create",
                json!({"title": "investigate"}),
            ))
            .await
            .expect("create a");
        let a_id = session.services.agent_control.team_board().list().await[0]
            .id
            .clone();

        TaskCreateHandler
            .handle(invoke(
                &session,
                &turn,
                "task_create",
                json!({"title": "build", "depends_on": [a_id.as_str()]}),
            ))
            .await
            .expect("create b");
        let b_id = session
            .services
            .agent_control
            .team_board()
            .list()
            .await
            .into_iter()
            .find(|t| t.title == "build")
            .expect("b present")
            .id;

        // Claiming b is rejected while a is unfinished.
        assert!(
            TaskClaimHandler
                .handle(invoke(
                    &session,
                    &turn,
                    "task_claim",
                    json!({"task_id": b_id.as_str()}),
                ))
                .await
                .is_err()
        );

        TaskClaimHandler
            .handle(invoke(
                &session,
                &turn,
                "task_claim",
                json!({"task_id": a_id.as_str()}),
            ))
            .await
            .expect("claim a");
        TaskUpdateHandler
            .handle(invoke(
                &session,
                &turn,
                "task_update",
                json!({"task_id": a_id.as_str(), "status": "completed"}),
            ))
            .await
            .expect("complete a");

        // Now b is claimable through the tool.
        TaskClaimHandler
            .handle(invoke(
                &session,
                &turn,
                "task_claim",
                json!({"task_id": b_id.as_str()}),
            ))
            .await
            .expect("claim b after unblock");

        TaskListHandler
            .handle(invoke(&session, &turn, "task_list", json!({})))
            .await
            .expect("list");

        let tasks = session.services.agent_control.team_board().list().await;
        assert_eq!(tasks.len(), 2);
        let b = tasks
            .into_iter()
            .find(|t| t.id == b_id)
            .expect("b present after flow");
        assert_eq!(b.status, TaskStatus::InProgress);
        assert_eq!(b.assignee, Some(TeammateName::from("lead")));
    }
}
