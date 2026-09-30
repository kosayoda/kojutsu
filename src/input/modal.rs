use ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use tui_input::backend::crossterm::EventHandler;

use crate::app::{App, AppMode};
use crate::jj_command::{JJCommand, JJCommandKind};
use crate::keymap;
use crate::types::{PromptStep, Str, TextPrompt};

use super::Action;

/// Carry a prompt step forward with its submitted text.
fn submit_step(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    step: PromptStep,
    text: String,
) -> Action {
    match step {
        PromptStep::LuaResume => {
            match lua.resume_suspended(app, crate::lua::ResumeValue::Text(text)) {
                crate::lua::ResumeResult::Action(a) => a,
                crate::lua::ResumeResult::DispatchAction { action, flags } => {
                    Action::DeferredDispatch { action, flags }
                }
            }
        }
        PromptStep::Revset => {
            app.revset.active_preset = None;
            Action::UpdateRevset(text)
        }
        PromptStep::WorkspaceAddPath { flags } => {
            app.mode = AppMode::text_input(
                "workspace name (enter for default): ",
                "",
                PromptStep::WorkspaceAddName { path: text, flags },
            );
            Action::None
        }
        PromptStep::WorkspaceAddName { path, flags } => {
            let name = (!text.is_empty()).then(|| crate::types::WorkspaceName::new(text));
            if app.active_view != crate::types::ActiveView::Dag {
                app.switch_view(crate::types::ActiveView::Dag);
            }
            app.mode = AppMode::CommitSelect {
                restore_cursor: app.cursor,
                pending: crate::types::PendingCommitSelect::WorkspaceAdd { path, name },
                flags,
                origin: None,
            };
            Action::None
        }
        PromptStep::RunCommand { change_ids, flags } => {
            submit_run_command(app, change_ids, flags, text)
        }
        PromptStep::RunJobs {
            change_ids,
            argv,
            flags,
        } => submit_run_jobs(app, change_ids, argv, flags, text),
        PromptStep::RawCommand => match shlex::split(&text) {
            Some(args) if !args.is_empty() => Action::run(JJCommand {
                kind: JJCommandKind::Raw {
                    args: args.into_iter().map(Str::from).collect(),
                },
                flags: keymap::CommandFlags::empty(),
            }),
            _ => Action::None,
        },
    }
}

pub(super) fn handle_text_input(
    app: &mut App,
    lua: &crate::lua::LuaEngine,
    key: KeyEvent,
) -> Action {
    match key.code {
        KeyCode::Enter => {
            let AppMode::TextInput {
                input, on_submit, ..
            } = std::mem::replace(&mut app.mode, AppMode::Normal)
            else {
                unreachable!("text input keys are handled in text input mode");
            };
            let text = input.to_string();
            match on_submit {
                TextPrompt::Command(prompt) => Action::run(prompt.into_command(text)),
                TextPrompt::Step(step) => submit_step(app, lua, step, text),
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
                && matches!(on_submit, TextPrompt::Step(PromptStep::RawCommand))
            {
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
                        // Already at common prefix: show completion list.
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

/// The free-input entry for the run picker; also the single source of the
/// `run:` prompt and its submit handler.
pub(in crate::input) fn run_custom_entry(
    change_ids: crate::types::SmallVec<crate::types::RevisionArg>,
    flags: keymap::CommandFlags,
) -> crate::app::CustomEntry {
    crate::app::CustomEntry {
        prompt: "run: ".into(),
        on_submit: PromptStep::RunCommand { change_ids, flags }.into(),
    }
}

/// The `run:` command prompt (initial entry and parse-error re-prompt).
pub(in crate::input) fn run_command_input(
    change_ids: crate::types::SmallVec<crate::types::RevisionArg>,
    flags: keymap::CommandFlags,
    prefill: impl Into<String>,
) -> AppMode {
    let custom = run_custom_entry(change_ids, flags);
    AppMode::text_input(custom.prompt, prefill, custom.on_submit)
}

/// The `--jobs` prompt shown when running over several revisions.
fn run_jobs_input(
    change_ids: crate::types::SmallVec<crate::types::RevisionArg>,
    argv: Vec<Str>,
    flags: keymap::CommandFlags,
    prefill: impl Into<String>,
) -> AppMode {
    AppMode::text_input(
        "jobs (empty = jj default): ",
        prefill,
        PromptStep::RunJobs {
            change_ids,
            argv,
            flags,
        },
    )
}

/// A `jj run` command line has been chosen (typed or picked from the list):
/// record it in history, then dispatch directly for a single revision or
/// chain into the `--jobs` prompt for several.
pub(in crate::input) fn submit_run_command(
    app: &mut App,
    change_ids: crate::types::SmallVec<crate::types::RevisionArg>,
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
        return Action::run(JJCommand {
            kind: JJCommandKind::Run {
                change_ids,
                argv,
                jobs: None,
            },
            flags,
        });
    }
    let prefill = app.run_jobs.map(|n| n.to_string()).unwrap_or_default();
    app.mode = run_jobs_input(change_ids, argv, flags, prefill);
    Action::None
}

/// The `--jobs` value has been entered: dispatch the run, or re-prompt on
/// a non-numeric value.
fn submit_run_jobs(
    app: &mut App,
    change_ids: crate::types::SmallVec<crate::types::RevisionArg>,
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
    Action::run(JJCommand {
        kind: JJCommandKind::Run {
            change_ids,
            argv,
            jobs,
        },
        flags,
    })
}

pub(super) fn handle_search_input(
    app: &mut App,
    keymap: &crate::keymap::Keymap,
    key: KeyEvent,
) -> Action {
    use crate::types::SearchFocus;

    match key.code {
        KeyCode::Esc => {
            app.cancel_search();
            Action::None
        }
        KeyCode::Enter => {
            app.finish_search();
            Action::None
        }
        KeyCode::Tab => {
            app.toggle_search_focus();
            Action::None
        }
        _ => {
            // Stepping through matches is the same action the DAG binds, so
            // take it from the keymap, but only for keys that aren't text,
            // which in the query field is anything printable.
            if super::typed_char(&key).is_none()
                && let Some(node) = keymap::key_event_to_node(&key)
            {
                match keymap.builtin_action(&node) {
                    Some(crate::keymap::AppAction::NextMatch) => {
                        app.search_next();
                        return Action::None;
                    }
                    Some(crate::keymap::AppAction::PrevMatch) => {
                        app.search_prev();
                        return Action::None;
                    }
                    _ => {}
                }
            }
            let focus = app.search.as_ref().map(|s| s.focus);
            match focus {
                Some(SearchFocus::Query) => {
                    if let Some(search) = &mut app.search {
                        let mut input = search.input.clone();
                        input.handle_event(&Event::Key(key));
                        app.update_search_input(input);
                    }
                }
                Some(SearchFocus::Scopes) => {
                    if let KeyCode::Char(ch) = key.code {
                        let specs = crate::types::scope_specs_for_view(app.active_view);
                        if let Some(spec) = specs.iter().find(|s| s.hint.starts_with(ch)) {
                            app.toggle_search_scope(spec.flag);
                        }
                    }
                }
                None => {}
            }
            Action::None
        }
    }
}

/// Handle the keys target-select owns. `None` means the key isn't one of
/// them, leaving the caller to resolve it through the keymap as navigation.
pub(super) fn handle_target_select(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char(' ') => {
            // Toggle a target, where the operation takes several.
            let multi = matches!(
                &app.mode,
                AppMode::TargetSelect { operation, .. } if operation.multi_target()
            );
            if multi
                && let Some(id) = app.selected_change_id()
                && !refuse_source_as_target(app, &id)
                && let AppMode::TargetSelect { picks, .. } = &mut app.mode
            {
                match picks.targets.iter().position(|t| *t == id) {
                    Some(i) => {
                        picks.targets.remove(i);
                    }
                    None => picks.targets.push(id),
                }
            }
            Some(Action::None)
        }
        KeyCode::Enter => Some(confirm_target_select(app)),
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::TargetSelect { restore_cursor, .. } = mode {
                app.set_cursor(restore_cursor);
            }
            Some(Action::None)
        }
        KeyCode::Char(_) => {
            if let (Some(node), AppMode::TargetSelect { origin, flags, .. }) =
                (crate::keymap::key_event_to_node(&key), &mut app.mode)
                && let Some(toggle) = origin
                    .iter()
                    .flat_map(|o| o.toggles.iter())
                    .find(|t| t.node == node)
            {
                flags.toggle(toggle.flag);
                app.status_message = None;
                return Some(Action::None);
            }
            None
        }
        _ => None,
    }
}

/// Say so, and return true, when `target` is one of the commits the target
/// select acts on: jj would only fail on it.
fn refuse_source_as_target(app: &mut App, target: &crate::types::RevisionArg) -> bool {
    let is_source = app
        .mode
        .picks()
        .is_some_and(|picks| picks.sources.contains(target));
    if is_source {
        app.set_error(format!("{target} is a source; pick another commit"));
    }
    is_source
}

/// Confirm the pending target(s) and start the operation they were picked for.
fn confirm_target_select(app: &mut App) -> Action {
    use crate::app::FollowUpPrompt;
    use crate::types::{RevisionArg, SmallVec, SmallVec1};

    // Picked with Space, or else the one under the cursor. Checked before the
    // mode is left, so a refused target keeps the selection going.
    let picked: SmallVec<RevisionArg> = match app.mode.picks() {
        Some(picks) if !picks.targets.is_empty() => picks.targets.iter().cloned().collect(),
        _ => match app.selected_change_id() {
            Some(target) if !refuse_source_as_target(app, &target) => smallvec::smallvec![target],
            _ => return Action::None,
        },
    };
    let Ok(targets) = SmallVec1::try_from_smallvec(picked) else {
        return Action::None;
    };

    let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
    let AppMode::TargetSelect {
        mut picks,
        operation,
        flags,
        ..
    } = mode
    else {
        return Action::None;
    };
    // Interdiff is handled directly (needs commit IDs from app state).
    if matches!(operation, crate::types::TargetOperation::Interdiff) {
        let source = picks.sources.first();
        let target = targets.first();
        // Resolve change IDs to commit IDs via the DAG index.
        let from_commit = app.commit_id_for_change(source);
        let to_commit = app.commit_id_for_change(target);
        if let (Some(from_cid), Some(to_cid)) = (from_commit, to_commit) {
            let from_label = crate::types::Str::from(source.as_str());
            let to_label = crate::types::Str::from(target.as_str());
            app.enter_interdiff_view(from_cid, to_cid, from_label, to_label);
        }
        return Action::None;
    }

    let label = operation.label();
    let selection = super::action::build_change_selection(app);
    picks.targets = targets.iter().cloned().collect();
    let mut options = operation.follow_up(picks.sources.clone(), targets, flags, selection);
    if options.len() == 1 {
        let opt = options.remove(0);
        return super::action::execute_follow_up(app, opt.action);
    }
    app.mode = AppMode::FollowUp {
        prompt: FollowUpPrompt::Picked {
            operation: label,
            picks,
        },
        options,
        origin: None,
    };
    Action::None
}

/// Handle the keys commit-select owns; see [`handle_target_select`].
pub(super) fn handle_commit_select(app: &mut App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            let AppMode::CommitSelect { pending, flags, .. } = mode else {
                return Some(Action::None);
            };
            // Enter belongs to this mode even with nothing selected: `None`
            // here would send it back to the keymap after the mode is gone.
            let Some(target) = app.selected_change_id() else {
                return Some(Action::None);
            };
            Some(Action::run(pending.into_jj_command(target, flags)))
        }
        KeyCode::Esc => {
            let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
            if let AppMode::CommitSelect { restore_cursor, .. } = mode {
                app.set_cursor(restore_cursor);
            }
            Some(Action::None)
        }
        _ => None,
    }
}

pub(super) fn handle_jump(app: &mut App, key: KeyEvent) -> Action {
    let mut state = app
        .mode
        .take_jump()
        .expect("handle_jump called outside Jump mode");

    // `take_jump` left Normal behind; exiting restores whatever the overlay
    // was opened over (a target-select in progress, say).
    let KeyCode::Char(c) = key.code else {
        app.exit_overlay();
        return Action::None;
    };

    state.input.push(c);
    state
        .labels
        .retain(|(label, _)| label.starts_with(&state.input));

    if state.labels.is_empty() {
        app.exit_overlay();
        return Action::None;
    }

    // Exact match → jump and exit.
    if let Some((_, row_idx)) = state.labels.iter().find(|(label, _)| *label == state.input) {
        app.set_cursor(*row_idx);
        app.exit_overlay();
    } else {
        // Input is a prefix of remaining labels: stay in jump mode. Assigned
        // directly, not via `enter_overlay`: the mode being restored is the
        // Normal that `take_jump` just put there, which would lose the real one.
        app.mode = AppMode::Jump(state);
    }

    Action::None
}

pub(super) fn handle_follow_up(app: &mut App, key: KeyEvent) -> Action {
    if key.code == KeyCode::Esc {
        app.mode = AppMode::Normal;
        return Action::None;
    }

    // Enter accepts the first (primary) option.
    if key.code == KeyCode::Enter {
        let mode = std::mem::replace(&mut app.mode, AppMode::Normal);
        let AppMode::FollowUp { mut options, .. } = mode else {
            return Action::None;
        };
        if options.is_empty() {
            return Action::None;
        }
        return super::action::execute_follow_up(app, options.remove(0).action);
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

#[cfg(test)]
mod jump_tests {
    use super::*;
    use crate::app::JumpState;
    use crate::idx::RowIdx;
    use crate::keymap::CommandFlags;
    use ratatui::crossterm::event::KeyModifiers;

    /// Jump is opened over a select mode often enough that returning to the
    /// right one is the whole point; it used to carry its own restore field
    /// rather than going through the overlay stack.
    fn app_in_commit_select() -> App {
        let mut app = App::for_test();
        app.mode = AppMode::CommitSelect {
            pending: crate::types::PendingCommitSelect::WorkspaceAdd {
                path: String::new(),
                name: None,
            },
            flags: CommandFlags::empty(),
            restore_cursor: RowIdx::new(0),
            origin: None,
        };
        app
    }

    fn open_jump(app: &mut App, labels: &[&str]) {
        app.enter_overlay(AppMode::Jump(JumpState {
            labels: labels
                .iter()
                .map(|l| (l.to_string(), RowIdx::new(0)))
                .collect(),
            input: String::new(),
        }));
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_jump(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn picking_a_label_returns_to_the_mode_underneath() {
        let mut app = app_in_commit_select();
        open_jump(&mut app, &["a"]);
        press(&mut app, KeyCode::Char('a'));
        assert!(matches!(app.mode, AppMode::CommitSelect { .. }));
    }

    #[test]
    fn dismissing_returns_to_the_mode_underneath() {
        let mut app = app_in_commit_select();
        open_jump(&mut app, &["a"]);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, AppMode::CommitSelect { .. }));

        // An input matching no label dismisses the same way.
        open_jump(&mut app, &["a"]);
        press(&mut app, KeyCode::Char('z'));
        assert!(matches!(app.mode, AppMode::CommitSelect { .. }));
    }

    /// A partial multi-character label stays in jump without consuming the
    /// mode it has to return to.
    #[test]
    fn a_prefix_keeps_jump_open_and_keeps_the_restore_target() {
        let mut app = app_in_commit_select();
        open_jump(&mut app, &["]c"]);
        press(&mut app, KeyCode::Char(']'));
        assert!(matches!(app.mode, AppMode::Jump(_)));
        press(&mut app, KeyCode::Char('c'));
        assert!(matches!(app.mode, AppMode::CommitSelect { .. }));
    }

    #[test]
    fn jump_from_normal_returns_to_normal() {
        let mut app = App::for_test();
        open_jump(&mut app, &["a"]);
        press(&mut app, KeyCode::Char('a'));
        assert!(matches!(app.mode, AppMode::Normal));
    }
}
