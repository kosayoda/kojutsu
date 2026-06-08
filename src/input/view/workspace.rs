use smallvec::smallvec;

use crate::app::App;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};

use crate::input::action::jump_to_commit_in_dag;
use crate::input::Action;

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::WorkspaceViewForget => {
            let Some(entry) = app.selected_workspace_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::WorkspaceForget {
                    names: smallvec![name],
                },
                flags,
            })
        }
        AppAction::WorkspaceViewJumpToCommit => {
            let Some(entry) = app.selected_workspace_entry() else {
                return Action::None;
            };
            let commit_id = entry.commit_id.clone();
            let change_id = entry.change_id.clone();
            jump_to_commit_in_dag(
                app,
                commit_id.as_ref(),
                change_id.as_ref(),
                "workspace has no associated commit",
            );
            Action::None
        }
        _ => Action::None,
    }
}
