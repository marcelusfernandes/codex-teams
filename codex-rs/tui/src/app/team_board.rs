//! App-level Agent Teams board state.
//!
//! `TeamTaskUpdated` notifications are emitted by the thread that touched the
//! shared board, but the board itself is scoped to the team. Keeping the live
//! model here lets the visible chat widget render one sticky panel across
//! thread switches instead of appending per-thread transcript cells.

use super::*;

impl App {
    pub(super) fn note_team_task_notification(&mut self, notification: &ServerNotification) {
        let ServerNotification::TeamTaskUpdated(notification) = notification else {
            return;
        };

        if notification.team_id.trim().is_empty() {
            return;
        }

        let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) else {
            return;
        };

        self.thread_team_ids
            .insert(thread_id, notification.team_id.clone());
        self.team_boards
            .entry(notification.team_id.clone())
            .or_default()
            .apply(notification.task.clone());
        self.sync_team_board_panel_for_current_thread();
    }

    pub(super) fn sync_team_board_panel_for_current_thread(&mut self) {
        let team_board = self.team_board_for_current_thread().cloned();
        self.chat_widget.set_team_board_model(team_board);
    }

    fn team_board_for_current_thread(&self) -> Option<&history_cell::TeamBoardModel> {
        let thread_id = self.current_displayed_thread_id()?;

        if let Some(team_id) = self.thread_team_ids.get(&thread_id)
            && let Some(board) = self.team_boards.get(team_id)
        {
            return Some(board);
        }

        let root_team_id = thread_id.to_string();
        self.team_boards.get(&root_team_id)
    }
}
