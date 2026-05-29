use crate::app::App;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::{ChangeId, CommitId, Str};

use super::action::build_change_selection;
use super::Action;

pub(super) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::EvoLogEdit => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Edit { change_id },
                flags,
            })
        }
        AppAction::EvoLogNew => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            let change_id = ChangeId::new(entry.commit_id.as_str());
            Action::RunJj(JJCommand {
                kind: JJCommandKind::New {
                    change_ids: smallvec::smallvec![change_id],
                    insert: None,
                },
                flags,
            })
        }
        AppAction::EvoLogInterdiff => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_error("already on current version");
                return Action::None;
            }
            let from_id = CommitId::new(entry.commit_id.as_str());
            let from_label = Str::from(entry.change_id.display.as_str());
            let Some(current) = app.evolog.entries.iter().find(|e| e.is_current) else {
                app.set_error("no current version found");
                return Action::None;
            };
            let to_id = CommitId::new(current.commit_id.as_str());
            let to_label = Str::from(current.change_id.display.as_str());
            app.enter_interdiff_view(from_id, to_id, from_label, to_label);
            Action::None
        }
        AppAction::EvoLogRestore => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_error("already on current version");
                return Action::None;
            }
            let from = ChangeId::new(entry.commit_id.as_str());
            let into = app
                .evolog
                .entries
                .iter()
                .find(|e| e.is_current)
                .map(|e| ChangeId::new(e.commit_id.as_str()));
            Action::RunJj(JJCommand {
                kind: JJCommandKind::Restore {
                    from: Some(from),
                    into,
                    changes_in: None,
                    selection: build_change_selection(app),
                },
                flags,
            })
        }
        _ => Action::None,
    }
}
