//! Team task-board history cell for the Agent Teams feature.
//!
//! Renders the shared board as a checkbox-style list that, unlike the plan
//! cell, also shows *who* claimed each task and whether a task is *blocked* by
//! unfinished dependencies — the two signals a teammate needs to pick up work.
//!
//! The cell consumes the client-facing [`TeamTask`] type (what the app-server
//! forwards), and [`TeamBoardModel`] is the live state the chat widget keeps as
//! `TeamTaskUpdated` notifications stream in.

use super::*;
use codex_app_server_protocol::TeamTask;
use codex_app_server_protocol::TeamTaskStatus;
use std::collections::HashSet;

/// Build a board cell from a snapshot of the team's tasks.
#[allow(dead_code)]
pub(crate) fn new_team_board(tasks: Vec<TeamTask>) -> TeamBoardCell {
    TeamBoardCell { tasks }
}

#[derive(Debug)]
pub(crate) struct TeamBoardCell {
    tasks: Vec<TeamTask>,
}

/// Live board state the chat widget maintains as `TeamTaskUpdated` notifications
/// stream in. Applying a delta replaces a task in place (so the rendered board
/// never reorders as work progresses) and appends unseen tasks in arrival
/// order. `cell()` snapshots the current state for the transcript.
#[derive(Debug, Default, Clone)]
pub(crate) struct TeamBoardModel {
    tasks: Vec<TeamTask>,
}

impl TeamBoardModel {
    /// Insert a new task or replace an existing one (matched by id) in place.
    pub(crate) fn apply(&mut self, task: TeamTask) {
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == task.id) {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Snapshot the current board as a renderable history cell.
    pub(crate) fn cell(&self) -> TeamBoardCell {
        TeamBoardCell {
            tasks: self.tasks.clone(),
        }
    }
}

impl HistoryCell for TeamBoardCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let completed: HashSet<&str> = self
            .tasks
            .iter()
            .filter(|task| task.status == TeamTaskStatus::Completed)
            .map(|task| task.id.as_str())
            .collect();

        let mut lines: Vec<Line<'static>> = vec![vec!["• ".dim(), "Team Tasks".bold()].into()];

        if self.tasks.is_empty() {
            let placeholder = vec![Line::from("(no tasks yet)".dim().italic())];
            lines.extend(prefix_lines(placeholder, "  └ ".dim(), "    ".into()));
            return lines;
        }

        let mut indented: Vec<Line<'static>> = vec![];
        for task in &self.tasks {
            let (glyph, style) = match task.status {
                TeamTaskStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
                TeamTaskStatus::InProgress => ("▸ ", Style::default().cyan().bold()),
                TeamTaskStatus::Cancelled => ("✗ ", Style::default().crossed_out().dim()),
                TeamTaskStatus::Pending => ("□ ", Style::default().dim()),
            };

            let mut suffix = String::new();
            if let Some(assignee) = &task.assignee {
                suffix.push_str(&format!("  @{assignee}"));
            }
            let blocked = task.status == TeamTaskStatus::Pending
                && task
                    .depends_on
                    .iter()
                    .any(|dep| !completed.contains(dep.as_str()));
            if blocked {
                suffix.push_str("  (blocked)");
            }

            let opts = RtOptions::new(width.saturating_sub(4).max(1) as usize)
                .initial_indent(glyph.into())
                .subsequent_indent("  ".into());
            let line = Line::from(format!("{}{}", task.title, suffix).set_style(style));
            let wrapped = adaptive_wrap_line(&line, opts);
            push_owned_lines(&wrapped, &mut indented);
        }
        lines.extend(prefix_lines(indented, "  └ ".dim(), "    ".into()));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("Team Tasks")];
        for task in &self.tasks {
            let assignee = task
                .assignee
                .as_ref()
                .map(|a| format!(" @{a}"))
                .unwrap_or_default();
            lines.push(Line::from(format!(
                "{:?}: {}{}",
                task.status, task.title, assignee
            )));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str, status: TeamTaskStatus) -> TeamTask {
        TeamTask {
            id: id.to_string(),
            title: title.to_string(),
            status,
            assignee: None,
            depends_on: vec![],
        }
    }

    fn rendered_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_status_assignee_and_blocked() {
        let mut done = task("t1", "investigate api", TeamTaskStatus::Completed);
        done.assignee = Some("researcher".to_string());
        let mut blocked = task("t2", "write client", TeamTaskStatus::Pending);
        blocked.depends_on = vec!["t9".to_string()]; // t9 not completed -> blocked
        let mut active = task("t3", "review", TeamTaskStatus::InProgress);
        active.assignee = Some("reviewer".to_string());

        let cell = new_team_board(vec![done, blocked, active]);
        let text = rendered_text(&cell.display_lines(80));

        assert!(text.contains("Team Tasks"));
        assert!(text.contains("investigate api"));
        assert!(text.contains("@researcher"));
        assert!(text.contains("write client"));
        assert!(text.contains("(blocked)"));
        assert!(text.contains("@reviewer"));
    }

    #[test]
    fn dependency_met_is_not_blocked() {
        let done = task("t1", "dep", TeamTaskStatus::Completed);
        let mut ready = task("t2", "next", TeamTaskStatus::Pending);
        ready.depends_on = vec!["t1".to_string()]; // dependency completed
        let cell = new_team_board(vec![done, ready]);
        let text = rendered_text(&cell.display_lines(80));
        assert!(!text.contains("(blocked)"));
    }

    #[test]
    fn empty_board_renders_placeholder() {
        let cell = new_team_board(vec![]);
        let text = rendered_text(&cell.display_lines(80));
        assert!(text.contains("(no tasks yet)"));
    }

    #[test]
    fn model_appends_new_tasks_in_arrival_order() {
        let mut model = TeamBoardModel::default();
        assert!(model.is_empty());
        model.apply(task("t1", "first", TeamTaskStatus::Pending));
        model.apply(task("t2", "second", TeamTaskStatus::Pending));
        let text = rendered_text(&model.cell().display_lines(80));
        let first = text.find("first").expect("first present");
        let second = text.find("second").expect("second present");
        assert!(first < second, "tasks render in arrival order");
        assert!(!model.is_empty());
    }

    #[test]
    fn model_replaces_task_in_place_on_update() {
        let mut model = TeamBoardModel::default();
        model.apply(task("t1", "a", TeamTaskStatus::Pending));
        model.apply(task("t2", "b", TeamTaskStatus::Pending));
        let mut updated = task("t1", "a", TeamTaskStatus::InProgress);
        updated.assignee = Some("worker".to_string());
        model.apply(updated);

        let text = rendered_text(&model.cell().display_lines(80));
        assert!(text.contains("@worker"));
        let a = text.find(" a").expect("a present");
        let b = text.find(" b").expect("b present");
        assert!(a < b, "updating a task must not reorder the board");
        assert_eq!(model.cell().display_lines(80).len(), {
            let mut fresh = TeamBoardModel::default();
            fresh.apply(task("t1", "a", TeamTaskStatus::InProgress));
            fresh.apply(task("t2", "b", TeamTaskStatus::Pending));
            fresh.cell().display_lines(80).len()
        });
    }
}
