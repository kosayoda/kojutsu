use ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap;
use crate::types::{PendingCommand, Str};

use super::{Action, PAGE_SIZE};

pub(super) fn handle_text_input(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    key: KeyEvent,
) -> Action {
    match key.code {
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TextInput {
                input, on_submit, ..
            } = mode
            {
                let text = input.to_string();
                match on_submit {
                    PendingCommand::LuaResume => match lua.resume_suspended(app, Some(text)) {
                        crate::lua::ResumeResult::Action(a) => a,
                        crate::lua::ResumeResult::DispatchAction { action, flags } => {
                            Action::DeferredDispatch { action, flags }
                        }
                    },
                    PendingCommand::Revset => {
                        app.revset.active_preset = None;
                        Action::UpdateRevset(text)
                    }
                    PendingCommand::WorkspaceAddPath { flags } => {
                        app.mode = AppMode::text_input(
                            "workspace name (enter for default): ",
                            "",
                            PendingCommand::WorkspaceAddName { path: text, flags },
                        );
                        Action::None
                    }
                    PendingCommand::WorkspaceAddName { path, flags } => {
                        let name = if text.is_empty() {
                            None
                        } else {
                            Some(crate::types::WorkspaceName::new(text))
                        };
                        if app.active_view != crate::app::ActiveView::Dag {
                            app.switch_view(crate::app::ActiveView::Dag);
                        }
                        let restore_cursor = app.cursor;
                        app.mode = AppMode::CommitSelect {
                            restore_cursor,
                            pending: crate::types::PendingCommitSelect::WorkspaceAdd { path, name },
                            flags,
                        };
                        Action::None
                    }
                    PendingCommand::RunCommand { change_ids, flags } => {
                        submit_run_command(app, change_ids, flags, text)
                    }
                    PendingCommand::RunJobs {
                        change_ids,
                        argv,
                        flags,
                    } => submit_run_jobs(app, change_ids, argv, flags, text),
                    PendingCommand::RawCommand => {
                        match PendingCommand::RawCommand.into_jj_command(text) {
                            Some(jj_cmd) => Action::SuspendAndRunJj(jj_cmd),
                            None => Action::None,
                        }
                    }
                    cmd => match cmd.into_jj_command(text) {
                        Some(jj_cmd) => Action::RunJj(jj_cmd),
                        None => Action::None,
                    },
                }
            } else {
                Action::None
            }
        }
        KeyCode::Esc => {
            if lua.has_suspended_thread() {
                app.mode = AppMode::Normal;
                lua.cancel_suspended_thread();
                Action::None
            } else {
                app.mode = AppMode::Normal;
                Action::None
            }
        }
        KeyCode::Tab => {
            if let AppMode::TextInput {
                input, on_submit, ..
            } = &mut app.mode
            {
                if matches!(on_submit, PendingCommand::RawCommand) {
                    let text = input.to_string();
                    let repo_path = std::path::PathBuf::from(&app.repo_root);
                    let completions = crate::jj_command::complete(&repo_path, &text);

                    if completions.len() == 1 {
                        // Single match: replace and add trailing space.
                        let new_text = crate::jj_command::replace_current_token(
                            &text,
                            &completions[0].value,
                            true,
                        );
                        *input = tui_input::Input::new(new_text);
                    } else if completions.len() > 1 {
                        // Multiple matches: extend to common prefix if possible.
                        let prefix = crate::jj_command::common_prefix(&completions);
                        let (_, current) = crate::jj_command::split_for_completion(&text);
                        if prefix.len() > current.len() {
                            let new_text =
                                crate::jj_command::replace_current_token(&text, prefix, false);
                            *input = tui_input::Input::new(new_text);
                        } else {
                            // Already at common prefix — show completion list.
                            let items: Vec<String> = completions
                                .iter()
                                .map(|c| {
                                    if c.description.is_empty() {
                                        c.value.clone()
                                    } else {
                                        format!("{}  {}", c.value, c.description)
                                    }
                                })
                                .collect();
                            app.mode = AppMode::select_from_list(
                                "completions",
                                items,
                                false,
                                crate::types::PendingSelection::CommandCompletion { input: text },
                                true,
                            );
                        }
                    }
                }
            }
            Action::None
        }
        _ => {
            if let AppMode::TextInput { input, .. } = &mut app.mode {
                input.handle_event(&Event::Key(key));
            }
            Action::None
        }
    }
}

/// A `jj run` command line has been chosen (typed or picked from the list):
/// record it in history, then dispatch directly for a single revision or
/// chain into the `--jobs` prompt for several.
/// The free-input entry for the run picker; also the single source of the
/// `run:` prompt and its submit handler.
pub(in crate::input) fn run_custom_entry(
    change_ids: crate::types::SmallVec<crate::types::ChangeId>,
    flags: keymap::CommandFlags,
) -> crate::app::CustomEntry {
    crate::app::CustomEntry {
        prompt: "run: ".into(),
        on_submit: PendingCommand::RunCommand { change_ids, flags },
    }
}

/// The `run:` command prompt (initial entry and parse-error re-prompt).
pub(in crate::input) fn run_command_input(
    change_ids: crate::types::SmallVec<crate::types::ChangeId>,
    flags: keymap::CommandFlags,
    prefill: impl Into<String>,
) -> AppMode {
    let custom = run_custom_entry(change_ids, flags);
    AppMode::text_input(custom.prompt, prefill, custom.on_submit)
}

/// The `--jobs` prompt shown when running over several revisions.
fn run_jobs_input(
    change_ids: crate::types::SmallVec<crate::types::ChangeId>,
    argv: Vec<Str>,
    flags: keymap::CommandFlags,
    prefill: impl Into<String>,
) -> AppMode {
    AppMode::text_input(
        "jobs (empty = jj default): ",
        prefill,
        PendingCommand::RunJobs {
            change_ids,
            argv,
            flags,
        },
    )
}

pub(in crate::input) fn submit_run_command(
    app: &mut App,
    change_ids: crate::types::SmallVec<crate::types::ChangeId>,
    flags: keymap::CommandFlags,
    text: String,
) -> Action {
    let argv: Vec<Str> = match shlex::split(&text) {
        Some(args) if !args.is_empty() => args.into_iter().map(Str::from).collect(),
        _ => {
            app.set_error("run: enter a command");
            app.mode = run_command_input(change_ids, flags, text);
            return Action::None;
        }
    };
    app.record_run_command(&text);
    // --jobs only matters when running over several revisions.
    if change_ids.len() == 1 {
        return Action::RunJj(JJCommand {
            kind: JJCommandKind::Run {
                change_ids,
                argv,
                jobs: None,
            },
            flags,
        });
    }
    let prefill = crate::repo::JjRepo::read_run_jobs(std::path::Path::new(&app.repo_root))
        .map(|n| n.to_string())
        .unwrap_or_default();
    app.mode = run_jobs_input(change_ids, argv, flags, prefill);
    Action::None
}

/// The `--jobs` value has been entered: dispatch the run, or re-prompt on
/// a non-numeric value.
fn submit_run_jobs(
    app: &mut App,
    change_ids: crate::types::SmallVec<crate::types::ChangeId>,
    argv: Vec<Str>,
    flags: keymap::CommandFlags,
    text: String,
) -> Action {
    let trimmed = text.trim();
    let jobs = if trimmed.is_empty() {
        None
    } else {
        match trimmed.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                app.set_error("jobs must be a positive number");
                app.mode = run_jobs_input(change_ids, argv, flags, text);
                return Action::None;
            }
        }
    };
    Action::RunJj(JJCommand {
        kind: JJCommandKind::Run {
            change_ids,
            argv,
            jobs,
        },
        flags,
    })
}

pub(super) fn handle_search_input(app: &mut App, key: KeyEvent) -> Action {
    use crate::types::SearchFocus;
    use ratatui::crossterm::event::KeyModifiers;

    match key.code {
        KeyCode::Esc => {
            app.cancel_search();
            Action::None
        }
        KeyCode::Enter => {
            app.confirm_search();
            Action::None
        }
        KeyCode::Tab => {
            app.toggle_search_focus();
            Action::None
        }
        _ => {
            let focus = app.search.as_ref().map(|s| s.focus);
            match focus {
                Some(SearchFocus::Query) => match key.code {
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_next();
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_prev();
                    }
                    _ => {
                        if let Some(search) = &mut app.search {
                            let mut input = search.input.clone();
                            input.handle_event(&Event::Key(key));
                            app.update_search_input(input);
                        }
                    }
                },
                Some(SearchFocus::Scopes) => match key.code {
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_next();
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_prev();
                    }
                    KeyCode::Char('0') => app.reset_search_scopes(),
                    KeyCode::Char('*') => app.enable_all_search_scopes(),
                    KeyCode::Char(ch) => {
                        let specs = crate::types::scope_specs_for_view(app.active_view);
                        if let Some(spec) = specs.iter().find(|s| s.hint.starts_with(ch)) {
                            app.toggle_search_scope(spec.flag);
                        }
                    }
                    _ => {}
                },
                None => {}
            }
            Action::None
        }
    }
}

/// Shared navigation for TargetSelect and CommitSelect modes.
/// Returns `Some(Action)` if the key was handled, `None` if not recognized.
///
/// Uses `key_event_to_node` for consistent key matching with the keymap system.
pub(super) fn handle_select_navigation(app: &mut App, key: &KeyEvent) -> Option<Action> {
    use keymap_parser::Key;

    let node = keymap::key_event_to_node(key)?;
    let shift = (node.modifiers & keymap_parser::Modifier::Shift as u8) != 0;
    let ctrl = (node.modifiers & keymap_parser::Modifier::Ctrl as u8) != 0;

    match (node.key, shift, ctrl) {
        (Key::Char('j'), true, _) => {
            app.move_down_section();
            Some(Action::None)
        }
        (Key::Char('k'), true, _) => {
            app.move_up_section();
            Some(Action::None)
        }
        (Key::Char('d'), _, true) => {
            app.page_down(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::Char('u'), _, true) => {
            app.page_up(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::Char('n'), _, true) => {
            app.search_next();
            Some(Action::None)
        }
        (Key::Char('p'), _, true) => {
            app.search_prev();
            Some(Action::None)
        }
        (Key::Char('j'), _, _) | (Key::Down, _, _) => {
            app.move_down();
            Some(Action::None)
        }
        (Key::Char('k'), _, _) | (Key::Up, _, _) => {
            app.move_up();
            Some(Action::None)
        }
        (Key::PageDown, _, _) => {
            app.page_down(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::PageUp, _, _) => {
            app.page_up(PAGE_SIZE);
            Some(Action::None)
        }
        (Key::Char('@'), _, _) => {
            app.jump_to_working_copy();
            Some(Action::None)
        }
        (Key::Char('0'), _, _) => {
            app.move_to_top();
            Some(Action::None)
        }
        (Key::Char('$'), _, _) => {
            app.move_to_bottom();
            Some(Action::None)
        }
        (Key::Tab, _, _) => {
            app.toggle_fold();
            Some(Action::None)
        }
        (Key::Char('\''), _, _) => {
            app.enter_jump();
            Some(Action::None)
        }
        (Key::Char('/'), _, _) => {
            app.begin_search();
            Some(Action::None)
        }
        (Key::Char('?'), _, _) => {
            app.enter_overlay(AppMode::Help { scroll: 0 });
            Some(Action::None)
        }
        _ => None,
    }
}

pub(super) fn handle_target_select(app: &mut App, key: KeyEvent) -> Action {
    use crate::app::TargetMode;

    match key.code {
        KeyCode::Char(' ') => {
            // Toggle target in multi-select mode.
            let id = app.selected_change_id();
            if let (
                Some(id),
                AppMode::TargetSelect {
                    target_mode: TargetMode::Multi { targets },
                    ..
                },
            ) = (id, &mut app.mode)
            {
                if !targets.remove(&id) {
                    targets.insert(id);
                }
            }
            Action::None
        }
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect {
                source,
                operation,
                flags,
                target_mode,
                ..
            } = mode
            {
                let targets: crate::types::SmallVec1<crate::types::ChangeId> = match target_mode {
                    TargetMode::Multi { targets } if !targets.is_empty() => {
                        match crate::types::SmallVec1::try_from_smallvec(
                            targets.into_iter().collect(),
                        ) {
                            Ok(v) => v,
                            Err(_) => return Action::None,
                        }
                    }
                    _ => {
                        let Some(target) = app.selected_change_id() else {
                            return Action::None;
                        };
                        crate::types::SmallVec1::new(target)
                    }
                };
                // Interdiff is handled directly (needs commit IDs from app state).
                if matches!(operation, crate::types::TargetOperation::Interdiff) {
                    let target = targets.split_off_first().0;
                    // Resolve change IDs to commit IDs via the DAG index.
                    let from_commit = app.commit_id_for_change(&source);
                    let to_commit = app.commit_id_for_change(&target);
                    if let (Some(from_cid), Some(to_cid)) = (from_commit, to_commit) {
                        let from_label = crate::types::Str::from(source.as_str());
                        let to_label = crate::types::Str::from(target.as_str());
                        app.enter_interdiff_view(from_cid, to_cid, from_label, to_label);
                    }
                    return Action::None;
                }

                let label = operation.label();
                let selection = super::action::build_change_selection(app);
                let mut options = operation.follow_up(source, targets.clone(), flags, selection);
                if options.len() == 1 {
                    let opt = options.remove(0);
                    return super::action::execute_follow_up(app, opt.action);
                }
                let target_str: String = targets
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let prompt = format!("{label} {target_str}:");
                app.mode = AppMode::FollowUp { prompt, options };
            }
            Action::None
        }
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect { restore_cursor, .. } = mode {
                app.set_cursor(restore_cursor);
            }
            Action::None
        }
        KeyCode::Char(_) => {
            if let (Some(node), AppMode::TargetSelect { toggles, flags, .. }) =
                (crate::keymap::key_event_to_node(&key), &mut app.mode)
            {
                if let Some(toggle) = toggles.iter().find(|t| t.node == node) {
                    flags.toggle(toggle.flag);
                    app.status_message = None;
                    return Action::None;
                }
            }
            handle_select_navigation(app, &key).unwrap_or(Action::None)
        }
        _ => handle_select_navigation(app, &key).unwrap_or(Action::None),
    }
}

pub(super) fn handle_commit_select(app: &mut App, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::CommitSelect { pending, flags, .. } = mode {
                let Some(target) = app.selected_change_id() else {
                    return Action::None;
                };
                let cmd = pending.into_jj_command(target, flags);
                Action::RunJj(cmd)
            } else {
                Action::None
            }
        }
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::CommitSelect { restore_cursor, .. } = mode {
                app.set_cursor(restore_cursor);
            }
            Action::None
        }
        _ => handle_select_navigation(app, &key).unwrap_or(Action::None),
    }
}

pub(super) fn handle_jump(app: &mut App, key: KeyEvent) -> Action {
    let (mut labels, mut input, restore_mode) = app
        .mode
        .take_jump()
        .expect("handle_jump called outside Jump mode");

    let exit = |app: &mut App, restore: Option<Box<AppMode>>| {
        app.mode = restore.map_or(AppMode::Normal, |m| *m);
    };

    let KeyCode::Char(c) = key.code else {
        exit(app, restore_mode);
        return Action::None;
    };

    input.push(c);
    labels.retain(|(label, _)| label.starts_with(&input));

    if labels.is_empty() {
        exit(app, restore_mode);
        return Action::None;
    }

    // Exact match → jump and exit.
    if let Some((_, row_idx)) = labels.iter().find(|(label, _)| *label == input) {
        app.set_cursor(*row_idx);
        exit(app, restore_mode);
    } else {
        // Input is a prefix of remaining labels — stay in jump mode.
        app.mode = AppMode::Jump {
            labels,
            input,
            restore_mode,
        };
    }

    Action::None
}

pub(super) fn handle_follow_up(app: &mut App, key: KeyEvent) -> Action {
    if key.code == KeyCode::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    let c = match key.code {
        KeyCode::Char(c) => c,
        _ => return Action::None,
    };

    let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
    let AppMode::FollowUp { options, .. } = mode else {
        return Action::None;
    };

    // Find the matching option.
    let Some(option) = options.into_iter().find(|o| o.key == c) else {
        return Action::None;
    };

    super::action::execute_follow_up(app, option.action)
}
