use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::crossterm::event::KeyEvent;

use crate::app::TargetMode;
use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap;
use crate::types::{
    BookmarkName, PendingSelection, RemoteName, SmallVec, TagName, TargetOperation, WorkspaceName,
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

pub(super) fn handle_select_from_list(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    key: KeyEvent,
) -> Action {
    use keymap_parser::Key;

    let node = keymap::key_event_to_node(&key);
    let ctrl = node
        .as_ref()
        .is_some_and(|n| (n.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0);
    let node_key = node.map(|n| n.key);

    // While filtering, intercept all keys except Tab/Esc/Enter.
    let is_filtering = matches!(&app.mode, AppMode::SelectFromList(s) if s.filtering);
    if is_filtering {
        match key.code {
            ratatui::crossterm::event::KeyCode::Char(c) if !ctrl => {
                if let AppMode::SelectFromList(s) = &mut app.mode {
                    s.filter.push(c);
                    refresh_list_filter(s);
                    // Land on the best match, not the pinned custom row —
                    // unless nothing matches, where Enter then opens the
                    // free input prefilled with the filter text.
                    s.cursor = if s.custom_entry.is_some() && s.filtered_indices.len() > 1 {
                        1
                    } else {
                        0
                    };
                    s.scroll_offset = 0;
                }
                return Action::None;
            }
            ratatui::crossterm::event::KeyCode::Backspace => {
                if let AppMode::SelectFromList(s) = &mut app.mode {
                    s.filter.pop();
                    refresh_list_filter(s);
                    s.cursor = s.cursor.min(s.filtered_indices.len().saturating_sub(1));
                    s.scroll_offset = 0;
                }
                return Action::None;
            }
            // Tab, Esc, Enter, arrows, page keys, and ctrl-n/p fall through to the
            // main match below so list navigation works while filtering.
            _ if matches!(
                node_key,
                Some(Key::Tab)
                    | Some(Key::Esc)
                    | Some(Key::Enter)
                    | Some(Key::Up)
                    | Some(Key::Down)
                    | Some(Key::PageUp)
                    | Some(Key::PageDown)
            ) => {}
            _ if matches!(node_key, Some(Key::Char('n') | Key::Char('p'))) && ctrl => {}
            // All other keys are swallowed while filtering.
            _ => return Action::None,
        }
    }

    match node_key {
        Some(Key::Char('n')) if ctrl => {
            list_move(app, 1);
            Action::None
        }
        Some(Key::Char('p')) if ctrl => {
            list_move(app, -1);
            Action::None
        }
        Some(Key::Char('j')) | Some(Key::Down) => {
            list_move(app, 1);
            Action::None
        }
        Some(Key::Char('k')) | Some(Key::Up) => {
            list_move(app, -1);
            Action::None
        }
        Some(Key::Char('d')) if ctrl => {
            list_move(app, PAGE_SIZE as isize);
            Action::None
        }
        Some(Key::Char('u')) if ctrl => {
            list_move(app, -(PAGE_SIZE as isize));
            Action::None
        }
        Some(Key::PageDown) => {
            list_move(app, PAGE_SIZE as isize);
            Action::None
        }
        Some(Key::PageUp) => {
            list_move(app, -(PAGE_SIZE as isize));
            Action::None
        }
        Some(Key::Char('0')) => {
            list_jump(app, false);
            Action::None
        }
        Some(Key::Char('$')) => {
            list_jump(app, true);
            Action::None
        }
        Some(Key::Tab) => {
            if let AppMode::SelectFromList(s) = &mut app.mode {
                s.filtering = !s.filtering;
            }
            Action::None
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
            Action::None
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
                resolve_selection(app, lua, s.on_select, names.into())
            } else {
                Action::None
            }
        }
        Some(Key::Esc) => {
            // If filtering, just exit filter focus — keep the filter text.
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
            Action::None
        }
        _ => Action::None,
    }
}

/// After item(s) have been selected from a list, decide what to do next.
pub(super) fn resolve_selection(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    on_select: PendingSelection,
    names: SmallVec<String>,
) -> Action {
    match on_select {
        PendingSelection::BookmarkDelete { flags, .. } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkDelete { names },
                flags,
            })
        }
        PendingSelection::BookmarkForget { flags, .. } => {
            let names = names.into_iter().map(BookmarkName::new).collect();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::BookmarkForget { names },
                flags,
            })
        }
        PendingSelection::WorkspaceForget { flags } => {
            let names = names.into_iter().map(WorkspaceName::new).collect();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::WorkspaceForget { names },
                flags,
            })
        }
        PendingSelection::BookmarkTrack { flags } => Action::RunJj(JJCommand {
            kind: JJCommandKind::BookmarkTrack {
                bookmarks: super::bookmark::parse_remote_bookmarks(names),
            },
            flags,
        }),
        PendingSelection::BookmarkUntrack { flags } => Action::RunJj(JJCommand {
            kind: JJCommandKind::BookmarkUntrack {
                bookmarks: super::bookmark::parse_remote_bookmarks(names),
            },
            flags,
        }),
        PendingSelection::GitPushBookmark { flags } => {
            let bookmarks: SmallVec<BookmarkName> =
                names.into_iter().map(BookmarkName::new).collect();
            if app.views.remotes.len() > 1 {
                let items = app.views.remotes.iter().map(|r| r.to_string()).collect();
                app.mode = AppMode::select_from_list(
                    "push bookmark to remote",
                    items,
                    false,
                    PendingSelection::GitRemoteForPushBookmark { bookmarks, flags },
                    false,
                );
                Action::None
            } else {
                Action::SuspendAndRunJj(JJCommand {
                    kind: JJCommandKind::GitPushBookmark {
                        bookmarks,
                        remote: None,
                    },
                    flags,
                })
            }
        }
        PendingSelection::GitRemoteForFetch { all_remotes, flags } => {
            let remote = names.into_iter().next().map(RemoteName::new);
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitFetch {
                    all_remotes,
                    remote,
                },
                flags,
            })
        }
        PendingSelection::GitRemoteForPush { all, flags } => {
            let remote = names.into_iter().next().map(RemoteName::new);
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitPush { all, remote },
                flags,
            })
        }
        PendingSelection::GitRemoteForPushBookmark { bookmarks, flags } => {
            let remote = names.into_iter().next().map(RemoteName::new);
            Action::SuspendAndRunJj(JJCommand {
                kind: JJCommandKind::GitPushBookmark { bookmarks, remote },
                flags,
            })
        }
        PendingSelection::TagDelete { flags } => {
            let names = names.into_iter().map(TagName::new).collect();
            Action::RunJj(JJCommand {
                kind: JJCommandKind::TagDelete { names },
                flags,
            })
        }
        // Single-item operations: take the first name.
        PendingSelection::BookmarkMove {
            change_id, flags, ..
        } => {
            let name = BookmarkName::new(names.into_iter().next().unwrap_or_default());
            let toggles = std::mem::take(&mut app.pending_toggles);
            app.mode = AppMode::TargetSelect {
                prompt: "move bookmark",
                source: change_id,
                restore_cursor: app.cursor,
                operation: TargetOperation::BookmarkMove {
                    bookmark_name: name,
                },
                flags,
                target_mode: TargetMode::Single,
                toggles,
            };
            Action::None
        }
        PendingSelection::BookmarkRename { flags, .. } => {
            let name = names.into_iter().next().unwrap_or_default();
            app.mode = AppMode::text_input(
                "rename to: ",
                name.clone(),
                crate::types::PendingCommand::BookmarkRename {
                    old_name: BookmarkName::new(name),
                    flags,
                },
            );
            Action::None
        }
        PendingSelection::PresetSelect => {
            let name = names.into_iter().next().unwrap_or_default();
            let idx = app.revset.presets.iter().position(|p| p.name == name);
            if let Some(i) = idx {
                app.revset.active_preset = Some(i);
                Action::UpdateRevset(app.revset.presets[i].revset.clone())
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
            // The item format is "value  description" — extract just the value.
            let value = selected.split_whitespace().next().unwrap_or(&selected);
            let new_text = crate::jj_command::replace_current_token(&input, value, true);
            app.mode = AppMode::text_input(":", new_text, crate::types::PendingCommand::RawCommand);
            Action::None
        }
        PendingSelection::RunCommand { change_ids, flags } => {
            let Some(selected) = names.into_iter().next() else {
                return Action::None;
            };
            super::modal::submit_run_command(app, change_ids, flags, selected)
        }
        PendingSelection::LuaResume => {
            let selected = names.into_iter().next().map(|s| s.to_string());
            match lua.resume_suspended(app, selected) {
                crate::lua::ResumeResult::Action(a) => a,
                crate::lua::ResumeResult::DispatchAction { action, flags } => {
                    Action::DeferredDispatch { action, flags }
                }
            }
        }
    }
}
