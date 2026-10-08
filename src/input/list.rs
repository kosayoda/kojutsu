use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::crossterm::event::KeyEvent;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap;
use crate::types::{
    BookmarkName, PendingSelection, RemoteCommand, RemoteName, SmallVec, TagName, TargetOperation,
    WorkspaceName,
};

use super::{Action, PAGE_SIZE};

fn recompute_list_filter(items: &[String], filter: &str) -> (Vec<usize>, Vec<Vec<usize>>) {
    if filter.is_empty() {
        return ((0..items.len()).collect(), vec![vec![]; items.len()]);
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<(usize, i64, Vec<usize>)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            matcher
                .fuzzy_indices(item, filter)
                .map(|(score, indices)| (i, score, indices))
        })
        .collect();
    scored.sort_unstable_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    let indices = scored.iter().map(|(i, _, _)| *i).collect();
    let positions = scored.into_iter().map(|(_, _, pos)| pos).collect();
    (indices, positions)
}

/// Re-run the fuzzy filter over the list's items. The custom-entry row, if
/// any, is pinned to the top regardless of how it scores against the filter.
fn refresh_list_filter(s: &mut crate::app::SelectFromListState) {
    let (mut indices, mut positions) = recompute_list_filter(&s.items, &s.filter);
    if s.custom_entry.is_some() {
        if let Some(pos) = indices.iter().position(|&i| i == 0) {
            indices.remove(pos);
            positions.remove(pos);
        }
        indices.insert(0, 0);
        positions.insert(0, Vec::new());
    }
    s.filtered_indices = indices;
    s.match_positions = positions;
}

/// Move the list cursor by `delta` rows (positive = down, negative = up).
fn list_move(app: &mut App, delta: isize) {
    if let AppMode::SelectFromList(s) = &mut app.mode {
        let max = s.filtered_indices.len().saturating_sub(1);
        s.cursor = (s.cursor as isize + delta).clamp(0, max as isize) as usize;
    }
}

/// Jump the list cursor to start (false) or end (true).
fn list_jump(app: &mut App, to_end: bool) {
    if let AppMode::SelectFromList(s) = &mut app.mode {
        s.cursor = if to_end {
            s.filtered_indices.len().saturating_sub(1)
        } else {
            0
        };
    }
}

/// Apply a movement action to the list's own cursor. The list has its own
/// notion of where it is, so it interprets the action rather than dispatching
/// it, but which key means which movement still comes from the keymap.
fn list_navigate(app: &mut App, action: crate::keymap::AppAction) {
    use crate::keymap::AppAction;
    match action {
        AppAction::MoveDown => list_move(app, 1),
        AppAction::MoveUp => list_move(app, -1),
        AppAction::PageDown => list_move(app, PAGE_SIZE as isize),
        AppAction::PageUp => list_move(app, -(PAGE_SIZE as isize)),
        AppAction::MoveToTop => list_jump(app, false),
        AppAction::MoveToBottom => list_jump(app, true),
        _ => {}
    }
}

pub(super) fn handle_select_from_list(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    keymap: &crate::keymap::Keymap,
    key: KeyEvent,
) -> Action {
    use keymap_parser::Key;

    let node = keymap::key_event_to_node(&key);
    let ctrl = node
        .as_ref()
        .is_some_and(|n| (n.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0);
    let node_key = node.as_ref().map(|n| n.key.clone());

    // While filtering, a printable key is filter text: that is the one thing
    // that has to outrank the keymap, since a movement key bound to a letter
    // must still type it here. Everything else falls through, so navigation
    // keeps working and follows whatever the user bound.
    let is_filtering = matches!(&app.mode, AppMode::SelectFromList(s) if s.filtering);
    if is_filtering {
        if let Some(c) = super::typed_char(&key) {
            if let AppMode::SelectFromList(s) = &mut app.mode {
                s.filter.push(c);
                refresh_list_filter(s);
                // Land on the best match, not the pinned custom row, unless
                // nothing matches, where Enter then opens the free input
                // prefilled with the filter text.
                s.cursor = if s.custom_entry.is_some() && s.filtered_indices.len() > 1 {
                    1
                } else {
                    0
                };
                s.scroll_offset = 0;
            }
            return Action::None;
        }
        if key.code == ratatui::crossterm::event::KeyCode::Backspace {
            if let AppMode::SelectFromList(s) = &mut app.mode {
                s.filter.pop();
                refresh_list_filter(s);
                s.cursor = s.cursor.min(s.filtered_indices.len().saturating_sub(1));
                s.scroll_offset = 0;
            }
            return Action::None;
        }
    }

    // Keys the list owns. ctrl-n/ctrl-p are the widget's own next/prev idiom
    // rather than the app's search-match actions, so they stay literal.
    match node_key {
        Some(Key::Char('n')) if ctrl => {
            list_move(app, 1);
            return Action::None;
        }
        Some(Key::Char('p')) if ctrl => {
            list_move(app, -1);
            return Action::None;
        }
        Some(Key::Tab) => {
            if let AppMode::SelectFromList(s) = &mut app.mode {
                s.filtering = !s.filtering;
            }
            return Action::None;
        }
        Some(Key::Space) => {
            if let AppMode::SelectFromList(s) = &mut app.mode
                && s.multi
                && let Some(&orig_idx) = s.filtered_indices.get(s.cursor)
            {
                if s.custom_entry.is_some() && orig_idx == 0 {
                    return Action::None;
                }
                if s.marked.contains(&orig_idx) {
                    s.marked.remove(&orig_idx);
                } else {
                    s.marked.insert(orig_idx);
                }
            }
            return Action::None;
        }
        Some(Key::Enter) => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::SelectFromList(s) = mode {
                let cursor_idx = s.filtered_indices.get(s.cursor).copied().unwrap_or(0);
                if cursor_idx == 0
                    && let Some(custom) = s.custom_entry
                {
                    // The filter text carries over as the input prefill.
                    app.mode = AppMode::text_input(custom.prompt, s.filter, custom.on_submit);
                    return Action::None;
                }
                let names: Vec<String> = if s.multi && !s.marked.is_empty() {
                    let mut indices: Vec<usize> = s.marked.into_iter().collect();
                    indices.sort_unstable();
                    indices
                        .into_iter()
                        .filter_map(|i| s.items.get(i).cloned())
                        .collect()
                } else {
                    vec![s.items.into_iter().nth(cursor_idx).unwrap_or_default()]
                };
                return resolve_selection(app, lua, s.on_select, names.into());
            }
            return Action::None;
        }
        Some(Key::Esc) => {
            // If filtering, just exit filter focus: keep the filter text.
            if let AppMode::SelectFromList(s) = &mut app.mode
                && s.filtering
            {
                s.filtering = false;
                return Action::None;
            }
            app.mode = AppMode::Normal;
            if lua.has_suspended_thread() {
                lua.cancel_suspended_thread();
            }
            return Action::None;
        }
        _ => {}
    }

    if let Some(node) = &node
        && let Some(action) = keymap.builtin_action(node)
    {
        list_navigate(app, action);
    }
    Action::None
}

/// After item(s) have been selected from a list, decide what to do next.
pub(super) fn resolve_selection(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    on_select: PendingSelection,
    names: SmallVec<String>,
) -> Action {
    match on_select {
        PendingSelection::BookmarkDelete { flags } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
            Action::run(JJCommand {
                kind: JJCommandKind::BookmarkDelete { names },
                flags,
            })
        }
        PendingSelection::BookmarkForget { flags } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
            Action::run(JJCommand {
                kind: JJCommandKind::BookmarkForget { names },
                flags,
            })
        }
        PendingSelection::WorkspaceRemove { flags } => {
            let names = names.into_iter().map(WorkspaceName::new).collect();
            super::target::confirm_workspace_remove(app, names, flags)
        }
        PendingSelection::WorkspaceForget { flags } => {
            let names = names.into_iter().map(WorkspaceName::new).collect();
            Action::run(JJCommand {
                kind: JJCommandKind::WorkspaceForget { names },
                flags,
            })
        }
        PendingSelection::BookmarkTrack { flags } => Action::run(JJCommand {
            kind: JJCommandKind::BookmarkTrack {
                bookmarks: super::bookmark::parse_remote_bookmarks(names),
            },
            flags,
        }),
        PendingSelection::BookmarkUntrack { flags } => Action::run(JJCommand {
            kind: JJCommandKind::BookmarkUntrack {
                bookmarks: super::bookmark::parse_remote_bookmarks(names),
            },
            flags,
        }),
        PendingSelection::GitPushBookmark { flags } => super::target::with_remote(
            app,
            "push bookmark to remote",
            RemoteCommand::PushBookmark {
                bookmarks: names.into_iter().map(BookmarkName::new).collect(),
            },
            flags,
        ),
        PendingSelection::GitRemote { command, flags } => {
            let remote = names.into_iter().next().map(RemoteName::new);
            Action::run(JJCommand {
                kind: command.to_kind(remote),
                flags,
            })
        }
        PendingSelection::TagTrack { flags } => Action::run(JJCommand {
            kind: JJCommandKind::TagTrack {
                tags: super::target::parse_remote_tags(names),
            },
            flags,
        }),
        PendingSelection::TagUntrack { flags } => Action::run(JJCommand {
            kind: JJCommandKind::TagUntrack {
                tags: super::target::parse_remote_tags(names),
            },
            flags,
        }),
        PendingSelection::TagDelete { flags } => {
            let names = names.into_iter().map(TagName::new).collect();
            Action::run(JJCommand {
                kind: JJCommandKind::TagDelete { names },
                flags,
            })
        }
        // Resumes with an array when the plugin asked for a multi-select, so
        // ticking several items isn't silently narrowed to the first one.
        PendingSelection::LuaResume { multi } => {
            match lua.resume_suspended(app, lua_selection_value(multi, names)) {
                crate::lua::ResumeResult::Action(a) => a,
                crate::lua::ResumeResult::DispatchAction { action, flags } => {
                    Action::DeferredDispatch { action, flags }
                }
            }
        }
        // Single-item operations: take the first name.
        PendingSelection::BookmarkMove { source, flags } => {
            let name = BookmarkName::new(names.into_iter().next().unwrap_or_default());
            // The target is picked in the DAG, wherever the bookmark was.
            if app.active_view != crate::types::ActiveView::Dag {
                app.switch_view(crate::types::ActiveView::Dag);
            }
            app.mode = AppMode::TargetSelect {
                picks: crate::app::Picks {
                    sources: crate::types::SmallVec1::new(source),
                    targets: Vec::new(),
                },
                started_on: app.cursor_row(),
                operation: TargetOperation::BookmarkMove {
                    bookmark_name: name,
                },
                flags,
                origin: None,
            };
            Action::None
        }
        PendingSelection::BookmarkRename { flags } => {
            let name = names.into_iter().next().unwrap_or_default();
            app.mode = AppMode::text_input(
                "rename to: ",
                name.clone(),
                crate::types::CommandPrompt::BookmarkRename {
                    old_name: BookmarkName::new(name),
                    flags,
                },
            );
            Action::None
        }
        PendingSelection::PresetSelect => {
            let name = names.into_iter().next().unwrap_or_default();
            let idx = app
                .config
                .revsets
                .presets
                .iter()
                .position(|p| p.name == name);
            if let Some(i) = idx {
                app.revset.active_preset = Some(i);
                Action::UpdateRevset(app.config.revsets.presets[i].revset.clone())
            } else {
                Action::None
            }
        }
        PendingSelection::OpLogWorkspaceFilter => {
            app.op_log.workspace_filter = names.into_iter().map(WorkspaceName::new).collect();
            app.rebuild_rows();
            Action::None
        }
        PendingSelection::FileListAnnotate { commit_id } => {
            let path = names.into_iter().next().unwrap_or_default();
            app.enter_annotate_view(commit_id, crate::types::RepoPath::new(path));
            Action::None
        }
        PendingSelection::CommandCompletion { input } => {
            let selected = names.into_iter().next().unwrap_or_default();
            // The item format is "value  description": extract just the value.
            let value = selected.split_whitespace().next().unwrap_or(&selected);
            let new_text = crate::jj_command::replace_current_token(&input, value, true);
            app.mode = AppMode::text_input(":", new_text, crate::types::PromptStep::RawCommand);
            Action::None
        }
        PendingSelection::RunCommand { change_ids, flags } => {
            let Some(selected) = names.into_iter().next() else {
                return Action::None;
            };
            super::modal::submit_run_command(app, change_ids, flags, selected)
        }
    }
}

/// The value a `kojutsu.ui.choose` thread is resumed with. A multi-select
/// keeps every ticked item; a single-select keeps the first and reports an
/// empty list as a dismissal.
fn lua_selection_value(multi: bool, names: SmallVec<String>) -> crate::lua::ResumeValue {
    if multi {
        crate::lua::ResumeValue::List(names.into_iter().collect())
    } else {
        names
            .into_iter()
            .next()
            .map(crate::lua::ResumeValue::Text)
            .unwrap_or(crate::lua::ResumeValue::None)
    }
}

#[cfg(test)]
mod lua_selection_tests {
    use super::{SmallVec, lua_selection_value};
    use crate::lua::ResumeValue;

    fn names(items: &[&str]) -> SmallVec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn multi_select_keeps_every_ticked_item() {
        let ResumeValue::List(items) =
            lua_selection_value(true, names(&["feature-a", "feature-b", "feature-c"]))
        else {
            panic!("expected a list");
        };
        assert_eq!(items, ["feature-a", "feature-b", "feature-c"]);
    }

    #[test]
    fn multi_select_with_nothing_ticked_stays_a_list() {
        let ResumeValue::List(items) = lua_selection_value(true, names(&[])) else {
            panic!("expected a list");
        };
        assert!(items.is_empty());
    }

    #[test]
    fn single_select_takes_the_first_name() {
        let ResumeValue::Text(item) = lua_selection_value(false, names(&["feature-a"])) else {
            panic!("expected a string");
        };
        assert_eq!(item, "feature-a");
    }

    #[test]
    fn single_select_with_no_name_is_a_dismissal() {
        assert!(matches!(
            lua_selection_value(false, names(&[])),
            ResumeValue::None
        ));
    }
}

#[cfg(test)]
mod list_navigation_tests {
    use super::*;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings, try_parse_key};
    use ratatui::crossterm::event::KeyModifiers;

    fn app_with_list(filtering: bool) -> App {
        let mut app = App::for_test();
        app.mode = AppMode::select_from_list(
            "pick",
            vec!["a".into(), "b".into(), "c".into()],
            false,
            PendingSelection::PresetSelect,
            false,
        );
        if let AppMode::SelectFromList(s) = &mut app.mode {
            s.filtering = filtering;
        }
        app
    }

    fn cursor(app: &App) -> usize {
        match &app.mode {
            AppMode::SelectFromList(s) => s.cursor,
            _ => panic!("list dismissed"),
        }
    }

    fn filter(app: &App) -> String {
        match &app.mode {
            AppMode::SelectFromList(s) => s.filter.clone(),
            _ => panic!("list dismissed"),
        }
    }

    fn press(app: &mut App, keymaps: &Keymaps, key: &str) {
        let node = try_parse_key(key).expect("parsable key");
        let event = KeyEvent::new(
            match node.key {
                keymap_parser::Key::Char(c) => ratatui::crossterm::event::KeyCode::Char(c),
                keymap_parser::Key::Down => ratatui::crossterm::event::KeyCode::Down,
                _ => panic!("unhandled test key: {key}"),
            },
            if node.modifiers & keymap_parser::Modifier::Ctrl as u8 != 0 {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        );
        let lua = crate::lua::LuaEngine::for_test();
        handle_select_from_list(
            app,
            &lua,
            keymaps.for_view(crate::types::ActiveView::Dag),
            event,
        );
    }

    #[test]
    fn the_list_moves_on_the_bound_movement_keys() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_list(false);

        press(&mut app, &keymaps, "j");
        assert_eq!(cursor(&app), 1);
        press(&mut app, &keymaps, "down");
        assert_eq!(cursor(&app), 2);
        press(&mut app, &keymaps, "k");
        assert_eq!(cursor(&app), 1);
        press(&mut app, &keymaps, "$");
        assert_eq!(cursor(&app), 2);
        press(&mut app, &keymaps, "0");
        assert_eq!(cursor(&app), 0);
    }

    /// The widget's own next/prev idiom, deliberately not the keymap's
    /// search-match actions.
    #[test]
    fn ctrl_n_and_ctrl_p_step_the_list() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_list(false);
        press(&mut app, &keymaps, "ctrl-n");
        assert_eq!(cursor(&app), 1);
        press(&mut app, &keymaps, "ctrl-p");
        assert_eq!(cursor(&app), 0);
    }

    /// A movement key is filter text while filtering: text outranks the
    /// keymap, but a chord still navigates.
    #[test]
    fn filtering_types_letters_and_still_navigates_on_chords() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_list(true);

        press(&mut app, &keymaps, "j");
        assert_eq!(filter(&app), "j");
        assert_eq!(cursor(&app), 0);

        press(&mut app, &keymaps, "ctrl-n");
        assert_eq!(filter(&app), "j", "a chord must not reach the filter");
    }
}
