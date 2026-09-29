use crate::app::App;
use crate::keymap::{AppAction, CommandFlags};

use crate::input::Action;

pub(in crate::input) fn dispatch(app: &mut App, action: AppAction, _flags: CommandFlags) -> Action {
    match action {
        AppAction::AnnotateTimeTravel => {
            if let Some(line) = app.selected_annotate_line() {
                let new_commit = line.commit_id.clone();
                let line_number = line.line_number;
                let same_commit = app
                    .annotate
                    .target
                    .as_ref()
                    .is_some_and(|t| t.commit_id == new_commit);
                if !same_commit {
                    if let Some(current_commit) =
                        app.annotate.target.as_ref().map(|t| t.commit_id.clone())
                    {
                        app.annotate.history.push((current_commit, line_number));
                    }
                    app.annotate_navigate(new_commit, line_number);
                }
            }
            Action::None
        }
        AppAction::ToggleSeparators => {
            match app.active_view {
                crate::types::ActiveView::Annotate => {
                    app.annotate.show_commit_separators = !app.annotate.show_commit_separators;
                    let label = if app.annotate.show_commit_separators {
                        "on"
                    } else {
                        "off"
                    };
                    app.set_status(format!("commit separators: {label}"));
                }
                crate::types::ActiveView::Bookmarks => {
                    app.views.show_bookmark_separators = !app.views.show_bookmark_separators;
                    app.rebuild_rows();
                    let label = if app.views.show_bookmark_separators {
                        "on"
                    } else {
                        "off"
                    };
                    app.set_status(format!("bookmark separators: {label}"));
                }
                _ => {}
            }
            Action::None
        }
        AppAction::AnnotateForward => {
            if let Some((prev_commit, prev_line)) = app.annotate.history.pop() {
                app.annotate_navigate(prev_commit, prev_line);
            }
            Action::None
        }
        other => unreachable!("{other:?} is not routed to this view"),
    }
}
