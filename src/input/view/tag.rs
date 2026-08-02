use smallvec::smallvec;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::PendingCommand;

use crate::input::Action;
use crate::input::action::jump_to_commit_in_dag;

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::TagViewDelete => {
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
        AppAction::TagViewSet => {
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
        AppAction::TagViewJumpToCommit => {
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
        AppAction::TagViewEdit => {
            let Some(entry) = app.selected_tag_entry() else {
                return Action::None;
            };
            let Some(change_id) = entry.revision.clone() else {
                app.set_error("tag has no associated commit");
                return Action::None;
            };
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Edit { change_id },
                flags,
            })
        }
        _ => Action::None,
    }
}
