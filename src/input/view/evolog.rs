use crate::app::App;
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap::{AppAction, CommandFlags};
use crate::types::RevisionArg;

use crate::input::Action;
use crate::input::action::build_change_selection;

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, flags: CommandFlags) -> Action {
    match action {
        AppAction::EvoLogRestore => {
            let Some(entry) = app.selected_evolog_entry() else {
                return Action::None;
            };
            if entry.is_current {
                app.set_error("already on current version");
                return Action::None;
            }
            let from = RevisionArg::new(entry.commit_id.as_str());
            let current = app.evolog.entries.iter().find(|e| e.is_current);
            // The DAG's file or line selection narrows the restore, but only
            // when made in the commit being restored into.
            if let Some(owner) = app.selection_owner()
                && current.is_none_or(|e| e.commit_id != *owner)
            {
                app.set_error("the selection is in another commit than this change's current one");
                return Action::None;
            }
            let into = current.map(|e| RevisionArg::new(e.commit_id.as_str()));
            Action::run(JJCommand {
                kind: JJCommandKind::Restore {
                    from: Some(from),
                    into,
                    changes_in: None,
                    selection: build_change_selection(app),
                },
                flags,
            })
        }
        other => unreachable!("{other:?} is not routed to this view"),
    }
}
