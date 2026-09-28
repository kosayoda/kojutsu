use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::PendingSelection;

use crate::input::Action;

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::OpLogFilterWorkspace => {
            let mut workspaces: Vec<String> = app
                .op_log
                .entries
                .iter()
                .filter_map(|e| e.workspace.as_ref().map(|w| w.to_string()))
                .collect();
            workspaces.sort_unstable();
            workspaces.dedup();
            if workspaces.is_empty() {
                app.set_error("no workspace info in operation log");
                return Action::None;
            }
            app.mode = AppMode::select_from_list(
                "filter by workspace",
                workspaces,
                true,
                PendingSelection::OpLogWorkspaceFilter,
                false,
            );
            Action::None
        }
        AppAction::OpLogRestore => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.clone();
            Action::run(JJCommand {
                kind: JJCommandKind::OpRestore { op_id },
                flags,
            })
        }
        AppAction::OpLogRevert => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.clone();
            Action::run(JJCommand {
                kind: JJCommandKind::OpRevert { op_id },
                flags,
            })
        }
        AppAction::OpLogAbandon => {
            let Some(entry) = app.selected_op_log_entry() else {
                return Action::None;
            };
            let op_id = entry.id.clone();
            Action::run(JJCommand {
                kind: JJCommandKind::OpAbandon { op_id },
                flags,
            })
        }
        other => unreachable!("{other:?} is not routed to this view"),
    }
}
