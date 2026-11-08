use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::crossterm::event::KeyEvent;
use unicode_segmentation::UnicodeSegmentation;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap;
use crate::types::{
    BookmarkName, PendingSelection, RemoteCommand, RemoteName, SmallVec, TagName, TargetOperation,
    WorkspaceName,
};

use super::{Action, PAGE_SIZE};

/// Rank `items` against `filter`, best first, with the byte offsets of the
/// matched characters in each. The filter takes fzf syntax: words match
/// independently, and `^`, `$`, `!` and `'` change how a word matches.
fn recompute_list_filter(items: &[String], filter: &str) -> (Vec<usize>, Vec<Vec<usize>>) {
    if filter.is_empty() {
        return ((0..items.len()).collect(), vec![vec![]; items.len()]);
    }
    let pattern = Pattern::parse(filter, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut matched = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<usize>)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            matched.clear();
            let haystack = Utf32Str::new(item, &mut buf);
            let score = pattern.indices(haystack, &mut matcher, &mut matched)?;
            matched.sort_unstable();
            matched.dedup();
            Some((i, score, match_byte_offsets(item, haystack, &matched)))
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    let indices = scored.iter().map(|(i, _, _)| *i).collect();
    let positions = scored.into_iter().map(|(_, _, pos)| pos).collect();
    (indices, positions)
}

/// Translate the matcher's indices into byte offsets of `item`. An ASCII
/// haystack is the item's own bytes, so its indices already are offsets
/// (nucleo also takes this path for non-ASCII text whose graphemes all
/// start with an ASCII char). A Unicode haystack holds one char per
/// grapheme, so each matched grapheme yields the offsets of all its chars.
fn match_byte_offsets(item: &str, haystack: Utf32Str<'_>, matched: &[u32]) -> Vec<usize> {
    match haystack {
        Utf32Str::Ascii(_) => matched.iter().map(|&i| i as usize).collect(),
        Utf32Str::Unicode(_) => item
            .grapheme_indices(true)
            .enumerate()
            .filter(|&(g, _)| matched.binary_search(&(g as u32)).is_ok())
            .flat_map(|(_, (start, grapheme))| {
                grapheme
                    .char_indices()
                    .map(move |(offset, _)| start + offset)
            })
            .collect(),
    }
}

/// Re-run the fuzzy filter over the list's items and land on the best
/// match. The custom-entry row, if any, is pinned to the top regardless of
/// how it scores against the filter, and is landed on only when nothing
/// else matches, where Enter then opens the free input prefilled with the
/// filter text.
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
    s.cursor = if s.custom_entry.is_some() && s.filtered_indices.len() > 1 {
        1
    } else {
        0
    };
    s.scroll_offset = 0;
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
            }
            return Action::None;
        }
        if key.code == ratatui::crossterm::event::KeyCode::Backspace {
            if let AppMode::SelectFromList(s) = &mut app.mode
                && s.filter.pop().is_some()
            {
                refresh_list_filter(s);
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
        PendingSelection::RemoteToEdit { edit, flags } => match names.into_iter().next() {
            Some(name) => super::target::edit_named_remote(app, RemoteName::new(name), edit, flags),
            None => Action::None,
        },
        PendingSelection::GitRemote { command, flags } => {
            let remotes = names.into_iter().map(RemoteName::new).collect();
            Action::run(JJCommand {
                kind: command.to_kind(remotes),
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

    /// A multi-select answers with a list, empty when nothing was ticked;
    /// a single select answers with its one name, or nothing if dismissed.
    #[test]
    fn multi_select_gives_a_list_and_single_select_gives_one_name_or_none() {
        let ResumeValue::List(items) =
            lua_selection_value(true, names(&["feature-a", "feature-b", "feature-c"]))
        else {
            panic!("expected a list");
        };
        assert_eq!(items, ["feature-a", "feature-b", "feature-c"]);
        let ResumeValue::List(items) = lua_selection_value(true, names(&[])) else {
            panic!("expected a list");
        };
        assert!(items.is_empty());

        let ResumeValue::Text(item) = lua_selection_value(false, names(&["feature-a"])) else {
            panic!("expected a string");
        };
        assert_eq!(item, "feature-a");
        assert!(matches!(
            lua_selection_value(false, names(&[])),
            ResumeValue::None
        ));
    }
}

#[cfg(test)]
mod list_filter_tests {
    use super::recompute_list_filter;

    fn filter(items: &[&str], filter: &str) -> (Vec<usize>, Vec<Vec<usize>>) {
        let items: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        recompute_list_filter(&items, filter)
    }

    /// The overlay highlights by byte offset, so a match after a multi-byte
    /// char must land past all of its bytes.
    #[test]
    fn match_positions_are_byte_offsets() {
        let (indices, positions) = filter(&["naïve"], "ïv");
        assert_eq!(indices, [0]);
        assert_eq!(positions, [vec![2, 4]]);
    }

    /// Every grapheme here starts with an ASCII char, so nucleo matches the
    /// raw bytes rather than one char per grapheme.
    #[test]
    fn match_positions_survive_combining_marks() {
        let (_, positions) = filter(&["e\u{301}xit"], "x");
        assert_eq!(positions, [vec![3]]);
    }

    #[test]
    fn an_uppercase_letter_makes_the_filter_case_sensitive() {
        assert_eq!(filter(&["main", "Main"], "m").0.len(), 2);
        assert_eq!(filter(&["main", "Main"], "M").0, [1]);
    }

    #[test]
    fn words_match_in_any_order() {
        let (indices, positions) = filter(&["push bookmark", "push tag"], "book push");
        assert_eq!(indices, [0]);
        assert_eq!(positions, [vec![0, 1, 2, 3, 5, 6, 7, 8]]);
    }
}

#[cfg(test)]
mod list_navigation_tests {
    use super::*;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings, try_parse_key};
    use ratatui::crossterm::event::KeyModifiers;

    fn app_with_list(items: &[&str], filtering: bool) -> App {
        let mut app = App::for_test();
        app.mode = AppMode::select_from_list(
            "pick",
            items.iter().map(|item| item.to_string()).collect(),
            false,
            PendingSelection::PresetSelect,
            false,
        );
        if let AppMode::SelectFromList(s) = &mut app.mode {
            s.filtering = filtering;
        }
        app
    }

    /// A filtering list whose first row is the free-input entry.
    fn app_with_custom_list(items: &[&str]) -> App {
        let mut app = App::for_test();
        app.mode = AppMode::select_from_list_with_custom(
            "run",
            "enter a command",
            crate::app::CustomEntry {
                prompt: "run: ".into(),
                on_submit: crate::types::PromptStep::RawCommand.into(),
            },
            items.iter().map(|item| item.to_string()).collect(),
            PendingSelection::PresetSelect,
        );
        if let AppMode::SelectFromList(s) = &mut app.mode {
            s.filtering = true;
        }
        app
    }

    /// The item on the cursor's row.
    fn cursor_item(app: &App) -> &str {
        match &app.mode {
            AppMode::SelectFromList(s) => &s.items[s.filtered_indices[s.cursor]],
            _ => panic!("list dismissed"),
        }
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

    fn list(app: &mut App) -> &mut crate::app::SelectFromListState {
        match &mut app.mode {
            AppMode::SelectFromList(s) => s,
            _ => panic!("list dismissed"),
        }
    }

    fn press(app: &mut App, keymaps: &Keymaps, key: &str) -> Action {
        let node = try_parse_key(key).expect("parsable key");
        let event = KeyEvent::new(
            match node.key {
                keymap_parser::Key::Char(c) => ratatui::crossterm::event::KeyCode::Char(c),
                keymap_parser::Key::Down => ratatui::crossterm::event::KeyCode::Down,
                keymap_parser::Key::Backspace => ratatui::crossterm::event::KeyCode::Backspace,
                keymap_parser::Key::Enter => ratatui::crossterm::event::KeyCode::Enter,
                keymap_parser::Key::Space => ratatui::crossterm::event::KeyCode::Char(' '),
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
        )
    }

    #[test]
    fn the_list_moves_on_the_bound_movement_keys() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_list(&["a", "b", "c"], false);

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
        let mut app = app_with_list(&["a", "b", "c"], false);
        press(&mut app, &keymaps, "ctrl-n");
        assert_eq!(cursor(&app), 1);
        press(&mut app, &keymaps, "ctrl-p");
        assert_eq!(cursor(&app), 0);
    }

    /// Deleting filter text re-ranks the list, so the cursor lands afresh as
    /// it does on typing, not on whatever now sits at its old row.
    #[test]
    fn deleting_filter_text_lands_on_the_best_match() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_custom_list(&["build", "lint"]);
        press(&mut app, &keymaps, "l");
        press(&mut app, &keymaps, "x");
        assert_eq!(cursor_item(&app), "enter a command");
        press(&mut app, &keymaps, "backspace");
        assert_eq!(cursor_item(&app), "lint");
    }

    /// A movement key is filter text while filtering: text outranks the
    /// keymap, but a chord still navigates.
    #[test]
    fn filtering_types_letters_and_still_navigates_on_chords() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_list(&["j1", "j2", "k"], true);

        press(&mut app, &keymaps, "j");
        assert_eq!(filter(&app), "j");
        assert_eq!(cursor(&app), 0);

        press(&mut app, &keymaps, "ctrl-n");
        assert_eq!(filter(&app), "j", "a chord must not reach the filter");
        assert_eq!(cursor(&app), 1);
    }

    /// The free-input row is pinned above the matches even when it doesn't
    /// match itself, but typing lands on the best match, not on it.
    #[test]
    fn a_filter_keeps_the_custom_row_on_top_and_lands_on_the_best_match() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_custom_list(&["build", "lint"]);
        press(&mut app, &keymaps, "l");
        press(&mut app, &keymaps, "i");
        assert_eq!(list(&mut app).filtered_indices, [0, 2]);
        assert_eq!(cursor_item(&app), "lint");
    }

    #[test]
    fn enter_on_the_custom_row_opens_the_input_with_the_filter_text() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_custom_list(&["build", "lint"]);
        press(&mut app, &keymaps, "l");
        press(&mut app, &keymaps, "i");
        press(&mut app, &keymaps, "ctrl-p");
        press(&mut app, &keymaps, "enter");
        let AppMode::TextInput { prompt, input, .. } = &app.mode else {
            panic!("expected the input");
        };
        assert_eq!((prompt.as_str(), input.value()), ("run: ", "li"));
    }

    /// Whatever order they were ticked in.
    #[test]
    fn a_multi_select_returns_the_marked_items_in_list_order() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = App::for_test();
        app.mode = AppMode::select_from_list(
            "delete",
            ["feature-c", "feature-a", "feature-b", "feature-d"]
                .map(String::from)
                .to_vec(),
            true,
            PendingSelection::BookmarkDelete {
                flags: crate::keymap::CommandFlags::empty(),
            },
            false,
        );
        press(&mut app, &keymaps, "$");
        press(&mut app, &keymaps, "space");
        press(&mut app, &keymaps, "0");
        press(&mut app, &keymaps, "space");
        press(&mut app, &keymaps, "j");
        press(&mut app, &keymaps, "j");
        press(&mut app, &keymaps, "space");
        let Action::RunJj { cmd, .. } = press(&mut app, &keymaps, "enter") else {
            panic!("expected a jj command");
        };
        let JJCommandKind::BookmarkDelete { names } = cmd.kind else {
            panic!("expected a bookmark delete");
        };
        assert!(
            names
                .iter()
                .map(|n| n.as_str())
                .eq(["feature-c", "feature-b", "feature-d"])
        );
    }

    #[test]
    fn space_does_not_mark_the_custom_row() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = app_with_custom_list(&["build", "lint"]);
        let state = list(&mut app);
        state.filtering = false;
        state.multi = true;
        press(&mut app, &keymaps, "space");
        assert!(list(&mut app).marked.is_empty());
        press(&mut app, &keymaps, "j");
        press(&mut app, &keymaps, "space");
        assert_eq!(list(&mut app).marked.len(), 1);
    }
}
