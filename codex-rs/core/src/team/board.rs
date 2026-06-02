//! In-process shared task board.
//!
//! Concurrency model (see RFC 0001 §5.3 as reconciled by the execution plan
//! §2): the board is an `Arc<tokio::sync::Mutex<BoardInner>>`. Because
//! `TaskBoard` is `Clone` over that `Arc`, every teammate in a spawn subtree
//! shares one board instance, exactly as `AgentControl` shares its registry.
//! The claim is a compare-and-set performed entirely under the async lock, so
//! two teammates racing to claim the same task cannot both win.
//!
//! On-disk persistence is intentionally not handled here; `TeamControl` (PR-3)
//! snapshots the board and writes it atomically. Keeping the board in-memory
//! keeps this module dependency-free and trivially testable.

use codex_protocol::team::Task;
use codex_protocol::team::TaskId;
use codex_protocol::team::TaskStatus;
use codex_protocol::team::TeammateName;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;

/// Reasons a board mutation can be rejected. Callers (PR-4 tools) translate
/// these into model-facing errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoardError {
    /// No task with the given id exists on the board.
    NotFound,
    /// The task is not `Pending` (already in progress, completed, or cancelled).
    NotPending,
    /// The task already has an assignee.
    AlreadyClaimed,
    /// The task cannot be claimed yet: these dependencies are not `Completed`.
    Blocked(Vec<TaskId>),
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoardError::NotFound => write!(f, "no such task"),
            BoardError::NotPending => write!(f, "task is not pending"),
            BoardError::AlreadyClaimed => write!(f, "task is already claimed"),
            BoardError::Blocked(deps) => {
                let deps = deps
                    .iter()
                    .map(TaskId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "task is blocked by unfinished dependencies: {deps}")
            }
        }
    }
}

impl std::error::Error for BoardError {}

#[derive(Default)]
struct BoardInner {
    tasks: Vec<Task>,
    /// Monotonic counter feeding task ids; also persisted by `TeamControl`.
    seq: u64,
}

/// A shared, conflict-free task board. Cheap to clone (shares one `Arc`).
#[derive(Clone, Default)]
pub(crate) struct TaskBoard {
    inner: Arc<Mutex<BoardInner>>,
}

impl TaskBoard {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Replace the board contents (used by `TeamControl` when loading from disk
    /// on resume).
    pub(crate) async fn load(&self, tasks: Vec<Task>, seq: u64) {
        let mut guard = self.inner.lock().await;
        guard.tasks = tasks;
        guard.seq = seq;
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Append a new pending task and return it.
    pub(crate) async fn create(
        &self,
        title: String,
        details: Option<String>,
        created_by: TeammateName,
        depends_on: Vec<TaskId>,
    ) -> Task {
        let mut guard = self.inner.lock().await;
        guard.seq += 1;
        let now = Self::now_ms();
        let task = Task {
            id: TaskId::from(format!("task-{}-{}", now, guard.seq)),
            title,
            details,
            status: TaskStatus::Pending,
            assignee: None,
            depends_on,
            created_by,
            created_at_ms: now,
            updated_at_ms: now,
        };
        guard.tasks.push(task.clone());
        task
    }

    /// Compare-and-set claim. Succeeds only if the task is still `Pending`,
    /// unclaimed, and every dependency is `Completed`. The whole check+set runs
    /// under the lock so concurrent claimers cannot both win.
    pub(crate) async fn claim(
        &self,
        id: &TaskId,
        who: TeammateName,
    ) -> Result<Task, BoardError> {
        let mut guard = self.inner.lock().await;
        let completed = Self::completed_ids(&guard.tasks);

        let task = guard
            .tasks
            .iter_mut()
            .find(|t| &t.id == id)
            .ok_or(BoardError::NotFound)?;

        if task.assignee.is_some() {
            return Err(BoardError::AlreadyClaimed);
        }
        if task.status != TaskStatus::Pending {
            return Err(BoardError::NotPending);
        }
        let unmet: Vec<TaskId> = task
            .depends_on
            .iter()
            .filter(|dep| !completed.contains(*dep))
            .cloned()
            .collect();
        if !unmet.is_empty() {
            return Err(BoardError::Blocked(unmet));
        }

        task.assignee = Some(who);
        task.status = TaskStatus::InProgress;
        task.updated_at_ms = Self::now_ms();
        Ok(task.clone())
    }

    /// Transition a task to a terminal/other status. When a task becomes
    /// `Completed`, returns the ids of tasks that just became claimable so the
    /// caller can emit `TaskUnblocked` events.
    pub(crate) async fn update(
        &self,
        id: &TaskId,
        status: TaskStatus,
    ) -> Result<(Task, Vec<TaskId>), BoardError> {
        let mut guard = self.inner.lock().await;
        if !guard.tasks.iter().any(|t| &t.id == id) {
            return Err(BoardError::NotFound);
        }

        let now = Self::now_ms();
        if let Some(task) = guard.tasks.iter_mut().find(|t| &t.id == id) {
            task.status = status;
            task.updated_at_ms = now;
        }

        let unblocked = if status == TaskStatus::Completed {
            let completed = Self::completed_ids(&guard.tasks);
            guard
                .tasks
                .iter()
                .filter(|t| {
                    t.status == TaskStatus::Pending
                        && t.assignee.is_none()
                        && !t.depends_on.is_empty()
                        && t.depends_on.iter().all(|dep| completed.contains(dep))
                })
                .map(|t| t.id.clone())
                .collect()
        } else {
            Vec::new()
        };

        let task = guard
            .tasks
            .iter()
            .find(|t| &t.id == id)
            .cloned()
            .expect("task present after mutation");
        Ok((task, unblocked))
    }

    /// Snapshot of all tasks, in creation order.
    pub(crate) async fn list(&self) -> Vec<Task> {
        self.inner.lock().await.tasks.clone()
    }

    fn completed_ids(tasks: &[Task]) -> HashSet<TaskId> {
        tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_claim_has_exactly_one_winner() {
        let board = TaskBoard::new();
        let task = board
            .create("ship".to_string(), None, TeammateName::from("lead"), vec![])
            .await;

        let mut handles = Vec::new();
        for i in 0..32 {
            let board = board.clone();
            let id = task.id.clone();
            handles.push(tokio::spawn(async move {
                board.claim(&id, TeammateName::from(format!("w{i}"))).await
            }));
        }

        let mut ok = 0;
        let mut rejected = 0;
        for handle in handles {
            match handle.await.expect("join") {
                Ok(_) => ok += 1,
                Err(BoardError::AlreadyClaimed) | Err(BoardError::NotPending) => rejected += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert_eq!(ok, 1, "exactly one claimer must win");
        assert_eq!(rejected, 31, "all other claimers must be rejected");
    }

    #[tokio::test]
    async fn blocked_until_dependency_completes_then_unblocks() {
        let board = TaskBoard::new();
        let a = board
            .create("a".to_string(), None, TeammateName::from("lead"), vec![])
            .await;
        let b = board
            .create(
                "b".to_string(),
                None,
                TeammateName::from("lead"),
                vec![a.id.clone()],
            )
            .await;

        let err = board
            .claim(&b.id, TeammateName::from("w1"))
            .await
            .unwrap_err();
        assert_eq!(err, BoardError::Blocked(vec![a.id.clone()]));

        board.claim(&a.id, TeammateName::from("w1")).await.unwrap();
        let (_done, unblocked) = board.update(&a.id, TaskStatus::Completed).await.unwrap();
        assert_eq!(unblocked, vec![b.id.clone()]);

        let claimed = board.claim(&b.id, TeammateName::from("w2")).await.unwrap();
        assert_eq!(claimed.assignee, Some(TeammateName::from("w2")));
        assert_eq!(claimed.status, TaskStatus::InProgress);
    }

    #[tokio::test]
    async fn claim_missing_task_errors() {
        let board = TaskBoard::new();
        let err = board
            .claim(&TaskId::from("nope"), TeammateName::from("w1"))
            .await
            .unwrap_err();
        assert_eq!(err, BoardError::NotFound);
    }
}
