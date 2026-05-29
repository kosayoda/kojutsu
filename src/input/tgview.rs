use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::PendingCommand;

use super::action::jump_to_commit_in_dag;
use super::Action;

pub(super) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::TgViewDelete => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::TagDelete {
                    names: smallvec![name],
                },
                flags,
            })
        }
        AppAction::TgViewSet => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let name = entry.name.clone();
            app.mode = AppMode::text_input(
                format!("set {name} to (change id): "),
                "",
                PendingCommand::TagSetByName { name, flags },
            );
            Action::None
        }
        AppAction::TgViewJumpToCommit => {
            let Some(ids) = app
                .selected_tag_entry()
                .map(|e| (e.commit_id.clone(), e.change_id.clone()))
            else {
                return Action::None;
            };
            jump_to_commit_in_dag(
                app,
                ids.0.as_ref(),
                ids.1.as_ref(),
                "tag has no associated commit",
            );
            Action::None
        }
        _ => Action::None,
    }
}
