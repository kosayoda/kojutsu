use std::path::PathBuf;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::Result;
use crossterm::event::{Event, KeyEventKind};

use kojutsu::app::{App, AppMode, DeferredWork};
use kojutsu::input::{self, Action};
use kojutsu::jj_command::{JJCommand, JJCommandResult};
use kojutsu::keymap::{self, Keymaps};
use kojutsu::repo::JjRepo;
use kojutsu::repo_service::{RepoRequestHandle, RepoResult, RepoService, RevsetLoadKind};
use kojutsu::terminal::spawn_terminal_events;
use kojutsu::types::JumpTarget;
use kojutsu::ui;

enum AppEvent {
    Init,
    Terminal(Event),
    Repo(Box<RepoResult>),
    /// A chunk of stdout/stderr from a running background jj command.
    JjOutput {
        chunk: Vec<u8>,
    },
    JjDone {
        result: Box<JJCommandResult>,
        cmd: Box<JJCommand>,
        jump: Option<JumpTarget>,
        label: Option<&'static str>,
        /// The command was yielded by a suspended Lua thread; completion
        /// resumes the thread with the result instead of refreshing.
        for_lua: bool,
    },
}

#[derive(Parser)]
#[command(name = "kojutsu", about = "TUI for Jujutsu version control")]
struct Cli {
    /// Path to the repository (default: current directory)
    #[arg(short = 'R', long = "repository", default_value = ".")]
    repository: PathBuf,

    /// Revset expression to display (default: from jj config `revsets.log`)
    #[arg(short = 'r', long = "revisions")]
    revisions: Option<String>,

    /// Print raw DAG edges and exit (for debugging graph rendering)
    #[arg(long)]
    debug_graph: bool,

    /// Print the default config file to stdout and exit.
    #[arg(long)]
    print_default_config: bool,

    /// Print Lua type definitions (EmmyLua annotations) to stdout and exit.
    #[arg(long)]
    generate_lua_types: bool,

    /// Internal: apply diff selection as a diff tool (invoked by jj).
    #[arg(long, hide = true)]
    apply_diff: Option<PathBuf>,

    /// Internal: copy pre-resolved conflict content to a merge tool's
    /// output file (invoked by jj as `--apply-resolution CONTENT OUTPUT`).
    #[arg(long, hide = true, num_args = 2, value_names = ["CONTENT", "OUTPUT"])]
    apply_resolution: Option<Vec<PathBuf>>,

    /// Internal: left directory for diff tool mode (positional).
    #[arg(hide = true)]
    diff_left: Option<PathBuf>,

    /// Internal: right directory for diff tool mode (positional).
    #[arg(hide = true)]
    diff_right: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(clap::Subcommand)]
enum CliCommand {
    /// Show line-by-line annotation (blame) for a file
    #[command(alias = "annotate")]
    Blame {
        /// File to annotate
        file: String,

        /// Revision to annotate at (default: @)
        #[arg(short = 'r', long = "revision", default_value = "@")]
        revision: String,
    },
}

fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    let cli = Cli::parse();

    if cli.print_default_config {
        print!("{}", kojutsu::theme::DEFAULT_CONFIG);
        return Ok(());
    }

    if cli.generate_lua_types {
        print!("{}", kojutsu::lua::generate_type_definitions());
        return Ok(());
    }

    // Merge tool mode: copy pre-resolved content to the output file and exit.
    if let Some(paths) = &cli.apply_resolution {
        let [content, output] = paths.as_slice() else {
            color_eyre::eyre::bail!("--apply-resolution requires CONTENT and OUTPUT paths");
        };
        std::fs::copy(content, output)?;
        let _ = std::fs::remove_file(content);
        return Ok(());
    }

    // Diff tool mode: apply selection and exit.
    if let Some(selection_path) = &cli.apply_diff {
        let left = cli
            .diff_left
            .as_ref()
            .expect("left dir required for --apply-diff");
        let right = cli
            .diff_right
            .as_ref()
            .expect("right dir required for --apply-diff");
        return kojutsu::diff_tool::apply(selection_path, left, right);
    }

    let repo_path = if cli.repository == std::path::Path::new(".") {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        find_workspace_dir(&cwd).to_path_buf()
    } else {
        cli.repository.canonicalize().unwrap_or(cli.repository)
    };

    // Handle blame subcommand: resolve commit + path before entering TUI.
    let blame_target = if let Some(CliCommand::Blame { file, revision }) = &cli.command {
        let _ = JjRepo::snapshot(&repo_path);
        let jj = JjRepo::open(&repo_path)?;
        let commit_hex = jj.resolve_single_commit(revision)?;
        let internal_path = jj.parse_file_path(file)?;
        Some((commit_hex, internal_path))
    } else {
        None
    };

    if cli.debug_graph {
        let _ = JjRepo::snapshot(&repo_path);
        let jj = JjRepo::open(&repo_path)?;
        let revset = cli.revisions.unwrap_or_else(|| jj.default_revset());
        let result = jj.evaluate_revset(&revset)?;
        for w in &result.warnings {
            eprintln!("warning: {w}");
        }
        debug_print_graph(&result.entries);
        return Ok(());
    }

    let config: &'static kojutsu::theme::Config =
        Box::leak(Box::new(kojutsu::theme::load_config()));
    let mut registry = keymap::ActionRegistry::new();
    let default_specs = keymap::default_bindings();
    let mut lua_engine = kojutsu::lua::LuaEngine::new(&repo_path, &mut registry, &default_specs);
    let mut specs = default_specs;
    specs.extend(lua_engine.take_extra_bindings());
    let keymaps = Keymaps::build(specs, registry);
    let lua_init_error = lua_engine.init_error.clone();
    let (event_tx, event_rx) = mpsc::channel();
    let (repo_requests, repo_responses) =
        RepoService::spawn(repo_path.clone(), config.diff.max_file_size_bytes());
    let _repo_forwarder =
        repo_responses.spawn_forwarder(event_tx.clone(), |r| AppEvent::Repo(Box::new(r)));
    let persisted = kojutsu::app::load_persisted_state();
    // Resolve active preset: persisted → first preset → None (jj default).
    let active_preset = persisted
        .active_preset
        .filter(|&i| i < config.revsets.presets.len())
        .or(if config.revsets.presets.is_empty() {
            None
        } else {
            Some(0)
        });
    let requested_revset = cli.revisions.clone().or_else(|| {
        active_preset
            .and_then(|i| config.revsets.presets.get(i))
            .map(|p| p.revset.clone())
    });
    let mut app = App::new(
        requested_revset.clone().unwrap_or_default(),
        repo_path.display().to_string(),
        &config.revsets.presets,
        &config.glyphs,
    );
    app.default_search_scopes = config.default_search_scopes.to_flags();
    app.run_presets = &config.run.presets;
    app.apply_persisted_state(&persisted);
    app.revset.active_preset = active_preset;
    app.request_revset_load(requested_revset);

    // If launched with `blame`, enter annotate view immediately.
    if let Some((commit_hex, internal_path)) = blame_target {
        let commit_id = kojutsu::types::CommitId::new(&commit_hex);
        let path = kojutsu::types::RepoPath::new(&internal_path);
        app.enter_annotate_view(commit_id, path);
    }

    if let Some(err) = lua_init_error {
        app.push_command_log(
            kojutsu::app::CommandLogKind::Warning,
            "init.lua error",
            None,
            err.as_bytes().to_vec(),
            false,
        );
        app.set_error(format!("init.lua: {err}"));
    }

    flush_repo_requests(&mut app, &repo_requests);
    let mut terminal = kojutsu::terminal::init()?;
    let mut terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
    let _ = event_tx.send(AppEvent::Init);

    let mut dirty = true;
    let mut events: Vec<AppEvent> = Vec::new();
    loop {
        if dirty {
            terminal.draw(|frame| ui::draw(frame, &mut app, &keymaps, config))?;
            dirty = false;
        }

        // Block for the first event.
        let first = match event_rx.recv() {
            Ok(event) => event,
            Err(_) => break,
        };

        // Drain all pending events so we can process them as a batch.
        events.clear();
        events.push(first);
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // Process batch.
        let mut deferred = DeferredWork::default();
        let mut breaking_action: Option<Action> = None;

        for event in events.drain(..) {
            let action = match event {
                AppEvent::Init => Action::None,
                AppEvent::Repo(result) => {
                    deferred.merge(app.handle_repo_result_deferred(*result));
                    dirty = true;
                    Action::None
                }
                AppEvent::JjOutput { chunk } => {
                    app.append_running_output(&chunk);
                    dirty = true;
                    Action::None
                }
                AppEvent::JjDone {
                    result,
                    cmd,
                    jump,
                    label,
                    for_lua,
                } => {
                    dirty = true;
                    if for_lua {
                        resume_lua_jj(&mut app, result, &lua_engine)
                    } else {
                        finish_jj_command(&mut app, *result, *cmd, jump, label, &lua_engine)
                    }
                }
                AppEvent::Terminal(ev) => match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        dirty = true;
                        input::handle_key(&mut app, &keymaps, &lua_engine, key)
                    }
                    Event::Mouse(mouse) => {
                        dirty = true;
                        let hdr = app.last_header_height;
                        input::handle_mouse(&mut app, mouse, hdr)
                    }
                    Event::Resize(..) => {
                        dirty = true;
                        Action::None
                    }
                    _ => Action::None,
                },
            };
            match action {
                Action::None => {}
                _ => {
                    breaking_action = Some(action);
                    break;
                }
            }
        }

        // Apply deferred work once for the whole batch.
        app.apply_rebuild(deferred.rebuild);
        if deferred.scroll {
            app.scroll_to_show_children();
        }

        // Handle breaking action.
        match breaking_action.unwrap_or(Action::None) {
            Action::Quit => break,
            Action::RunJj(cmd) => {
                let action_label = app.last_action_label.take();
                run_jj_command(&mut app, &repo_path, cmd, action_label, &event_tx, false);
            }
            Action::RunJjForLua(cmd) => {
                run_jj_command(&mut app, &repo_path, cmd, None, &event_tx, true);
            }
            Action::SuspendAndRunJj(cmd) => {
                let action_label = app.last_action_label.take();
                terminal_events.stop();
                suspend_and_run(&mut app, &repo_path, &mut terminal, cmd);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
                if let Some(label) = action_label {
                    run_post_hooks_after_suspend(
                        &mut app,
                        &lua_engine,
                        label,
                        &repo_path,
                        &event_tx,
                    );
                }
            }
            Action::Refresh => {
                refresh_app(&mut app, RevsetLoadKind::Snapshot);
            }
            Action::UpdateRevset(revset_str) => {
                update_revset(&mut app, revset_str);
            }
            Action::EditRevsetInEditor => {
                app.revset.active_preset = None;
                terminal_events.stop();
                edit_revset_in_editor(&mut app, &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
            }
            Action::EditWorkingCopyFile { path, line } => {
                terminal_events.stop();
                open_file_in_editor(&repo_path, &path, line, &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
            }
            Action::EditFileAtRevision {
                commit_id,
                path,
                line,
                ..
            } => {
                terminal_events.stop();
                open_revision_in_editor(&repo_path, &commit_id, &path, line, &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
            }
            Action::EditConflictFile {
                change_id,
                path,
                content,
                flags,
            } => {
                terminal_events.stop();
                let edited = edit_content_in_editor(&content, path.as_str(), &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
                match edited {
                    EditOutcome::Edited(edited) => {
                        if let Some(cmd) = kojutsu::input::staged_resolution(
                            &mut app, change_id, &path, &edited, flags,
                        ) {
                            let action_label = app.last_action_label.take();
                            run_jj_command(
                                &mut app,
                                &repo_path,
                                cmd,
                                action_label,
                                &event_tx,
                                false,
                            );
                        }
                    }
                    EditOutcome::Failed(e) => app.set_error(format!("editor: {e}")),
                    EditOutcome::Unchanged | EditOutcome::Cancelled => {
                        app.set_status("edit cancelled - no changes")
                    }
                }
            }
            Action::EditConflictHunk {
                commit_id,
                path,
                hunk_idx,
                seed,
                flags,
            } => {
                terminal_events.stop();
                let edited = edit_content_in_editor(&seed, path.as_str(), &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
                match edited {
                    EditOutcome::Edited(edited) => {
                        kojutsu::input::complete_hunk_edit(
                            &mut app, &commit_id, &path, hunk_idx, &edited, flags,
                        );
                    }
                    EditOutcome::Failed(e) => app.set_error(format!("editor: {e}")),
                    EditOutcome::Unchanged | EditOutcome::Cancelled => {
                        app.set_status("edit cancelled - no changes")
                    }
                }
            }
            Action::CheckoutAndEdit {
                commit_id,
                path,
                line,
            } => {
                let cmd = JJCommand {
                    kind: kojutsu::jj_command::JJCommandKind::New {
                        change_ids: smallvec::smallvec![kojutsu::types::ChangeId::new(
                            commit_id.as_str()
                        )],
                        insert: None,
                    },
                    flags: kojutsu::keymap::CommandFlags::empty(),
                };
                terminal_events.stop();
                let success = suspend_and_run(&mut app, &repo_path, &mut terminal, cmd);
                if success {
                    open_file_in_editor(&repo_path, &path, line, &mut terminal);
                }
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
            }
            Action::DeferredDispatch { action, flags } => {
                let keymap = keymaps.for_view(app.active_view);
                let result = input::dispatch_action_after_hooks(
                    &mut app,
                    &keymaps.registry,
                    &lua_engine,
                    keymap,
                    action,
                    flags,
                );
                // Re-process the resulting action.
                match result {
                    Action::RunJj(cmd) => {
                        let action_label = app.last_action_label.take();
                        run_jj_command(&mut app, &repo_path, cmd, action_label, &event_tx, false);
                    }
                    Action::RunJjForLua(cmd) => {
                        run_jj_command(&mut app, &repo_path, cmd, None, &event_tx, true);
                    }
                    Action::SuspendAndRunJj(cmd) => {
                        let action_label = app.last_action_label.take();
                        terminal_events.stop();
                        suspend_and_run(&mut app, &repo_path, &mut terminal, cmd);
                        terminal_events =
                            spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
                        if let Some(label) = action_label {
                            run_post_hooks_after_suspend(
                                &mut app,
                                &lua_engine,
                                label,
                                &repo_path,
                                &event_tx,
                            );
                        }
                    }
                    Action::Refresh => refresh_app(&mut app, RevsetLoadKind::Snapshot),
                    Action::UpdateRevset(revset_str) => update_revset(&mut app, revset_str),
                    _ => {}
                }
            }
            Action::None => {}
        }
        lua_engine.flush_logs(&mut app);
        flush_repo_requests(&mut app, &repo_requests);
    }

    kojutsu::app::save_persisted_state(&app.to_persisted_state());
    terminal_events.stop();
    kojutsu::terminal::restore()?;
    Ok(())
}

fn extract_command_result(app: &App) -> (bool, Vec<u8>) {
    match &app.mode {
        AppMode::CommandOutput(state) => (state.success, state.output.clone()),
        _ => (false, Vec::new()),
    }
}

fn flush_repo_requests(app: &mut App, service: &RepoRequestHandle) {
    for request in app.take_repo_requests() {
        service.send(request);
    }
}

/// Reload the current revset. `Snapshot` re-scans the working copy first —
/// needed only when the user may have edited files since jj last looked
/// (manual refresh, returning from a suspended command). Refreshes after
/// captured jj commands use `NoSnapshot`: the command itself already
/// snapshotted the working copy when it started.
fn refresh_app(app: &mut App, load_kind: RevsetLoadKind) {
    app.refresh(load_kind);
}

fn suspend_and_run(
    app: &mut App,
    repo_path: &std::path::Path,
    terminal: &mut kojutsu::terminal::Term,
    cmd: JJCommand,
) -> bool {
    // Store the jump target before running (cmd is consumed).
    let jump = cmd.jump_target();

    // Leave the alternate screen so the child can use the terminal.
    // Interactive commands need full terminal access; captured commands
    // need stdin for SSH password prompts.
    let _ = kojutsu::terminal::restore();
    let result = if cmd.is_interactive() {
        cmd.run_interactive(repo_path)
    } else {
        cmd.run_suspend_captured(repo_path)
    };
    // Re-enter the TUI.
    *terminal = match kojutsu::terminal::init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fatal: failed to re-init terminal: {e}");
            std::process::exit(1);
        }
    };

    app.push_command_log(
        kojutsu::app::CommandLogKind::Command,
        &result.display,
        Some(result.display_parts.clone()),
        result.output.clone(),
        result.success,
    );

    if result.success {
        app.jump_after_refresh = jump;
        app.clear_selection();
        // The user may have edited files while the terminal was suspended
        // (e.g. in $EDITOR) — re-scan the working copy.
        refresh_app(app, RevsetLoadKind::Snapshot);
    }

    // Show output if there is any, or if the command failed (so failures are
    // always visible even when interactive commands print to inherited stdio).
    if !result.success || !result.output.is_empty() {
        let retry = cmd.retry_options(&result.output);
        app.mode = AppMode::command_output(
            result.display,
            Some(result.display_parts),
            result.output,
            result.success,
            retry,
        );
    }
    result.success
}

fn update_revset(app: &mut App, revset_str: String) {
    // An explicit revset change leaves any active conflicted() toggle.
    app.revset.conflicted_prev = None;
    app.request_revset_load_no_snapshot(Some(revset_str));
}

/// Suspend the TUI, run `$EDITOR` with `args`, and re-init the terminal
/// afterward. Re-init failure is fatal — the TUI cannot continue. The
/// event reader thread is the caller's responsibility (stop before, spawn
/// after). Returns the editor's exit status, or the spawn error.
fn run_editor_suspended(
    terminal: &mut kojutsu::terminal::Term,
    args: &[&std::ffi::OsStr],
) -> std::io::Result<std::process::ExitStatus> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let _ = kojutsu::terminal::restore();
    let status = std::process::Command::new(&editor).args(args).status();
    *terminal = match kojutsu::terminal::init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fatal: failed to re-init terminal: {e}");
            std::process::exit(1);
        }
    };
    status
}

/// Outcome of editing content in `$EDITOR`.
enum EditOutcome {
    /// Saved with changed content.
    Edited(String),
    /// Saved, but the content was unchanged.
    Unchanged,
    /// Editor exited non-zero (user cancelled, e.g. `:cq`).
    Cancelled,
    /// The editor couldn't be run, or the temp file couldn't be
    /// written/read.
    Failed(String),
}

fn edit_revset_in_editor(app: &mut App, terminal: &mut kojutsu::terminal::Term) {
    use std::io::Write;

    let revset_text = app.revset_input_text();
    let mut tmpfile = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            app.mode = AppMode::command_output(
                "revset editor".to_string(),
                None,
                format!("failed to create temp file: {e}").into_bytes(),
                false,
                vec![],
            );
            return;
        }
    };
    let _ = writeln!(tmpfile, "{revset_text}");
    let path = tmpfile.path().to_path_buf();

    let report_error = |app: &mut App, msg: String| {
        app.mode = AppMode::command_output(
            "revset editor".to_string(),
            None,
            msg.into_bytes(),
            false,
            vec![],
        );
    };
    match run_editor_suspended(terminal, &[path.as_os_str()]) {
        Ok(s) if s.success() => match std::fs::read_to_string(&path) {
            Ok(content) => {
                let new_revset = content.trim().to_string();
                if !new_revset.is_empty() {
                    update_revset(app, new_revset);
                }
            }
            Err(e) => report_error(app, format!("failed to read temp file: {e}")),
        },
        // Non-zero exit: user cancelled, nothing to do.
        Ok(_) => {}
        Err(e) => report_error(app, format!("failed to run editor: {e}")),
    }
}

/// Suspend the TUI, open `content` in $EDITOR (temp file suffixed like
/// `path` for syntax highlighting), and report what happened.
fn edit_content_in_editor(
    content: &str,
    path: &str,
    terminal: &mut kojutsu::terminal::Term,
) -> EditOutcome {
    use std::io::Write;

    let suffix = std::path::Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let staged = (|| -> std::io::Result<tempfile::NamedTempFile> {
        let mut tmpfile = tempfile::Builder::new().suffix(&suffix).tempfile()?;
        tmpfile.write_all(content.as_bytes())?;
        tmpfile.flush()?;
        Ok(tmpfile)
    })();
    let tmpfile = match staged {
        Ok(f) => f,
        Err(e) => return EditOutcome::Failed(format!("temp file: {e}")),
    };
    let tmp_path = tmpfile.path().to_path_buf();

    match run_editor_suspended(terminal, &[tmp_path.as_os_str()]) {
        Ok(s) if s.success() => match std::fs::read_to_string(&tmp_path) {
            Ok(edited) if edited != content => EditOutcome::Edited(edited),
            Ok(_) => EditOutcome::Unchanged,
            Err(e) => EditOutcome::Failed(format!("read: {e}")),
        },
        Ok(_) => EditOutcome::Cancelled,
        Err(e) => EditOutcome::Failed(format!("run editor: {e}")),
    }
}

fn debug_print_graph(entries: &[kojutsu::dag::DagEntry]) {
    for entry in entries {
        let c = &entry.commit;
        let mut flags = Vec::new();
        if c.is_working_copy() {
            flags.push("wc");
        }
        if c.is_immutable {
            flags.push("immutable");
        }
        if c.is_empty {
            flags.push("empty");
        }
        if c.has_conflict {
            flags.push("conflict");
        }
        if c.is_divergent() {
            flags.push("divergent");
        }
        if c.is_hidden() {
            flags.push("hidden");
        }
        let flags_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };
        let bookmarks: Vec<&str> = c.bookmarks.iter().map(|b| b.name.as_str()).collect();
        let bm_str = if bookmarks.is_empty() {
            String::new()
        } else {
            format!(" bookmarks={}", bookmarks.join(","))
        };
        let desc = c.description.as_deref().unwrap_or("(no description)");
        let suffix = c
            .change_id_suffix()
            .map(|n| format!("/{n}"))
            .unwrap_or_default();
        println!(
            "{}{} ({}){}{} {}",
            c.change_id.display,
            suffix,
            &c.graph_id.as_str()[..8],
            flags_str,
            bm_str,
            desc
        );
        for edge in &entry.edges {
            println!(
                "  {:?} -> {}",
                edge.kind,
                &edge.target.as_str()[..8.min(edge.target.as_str().len())]
            );
        }
    }
}

/// Spawn a background thread to run a non-interactive jj command.
///
/// Sets `app.mode` to `CommandRunning` immediately so the UI shows progress,
/// then sends `AppEvent::JjDone` when the child exits (or is cancelled via
/// Esc, which kills the child process group).
fn run_jj_command(
    app: &mut App,
    repo_path: &std::path::Path,
    cmd: JJCommand,
    label: Option<&'static str>,
    event_tx: &mpsc::Sender<AppEvent>,
    for_lua: bool,
) {
    let jump = cmd.jump_target();
    let command = cmd.display();
    let command_parts = cmd.display_parts();
    let (kill_main, kill_bg) = kojutsu::jj_command::KillHandle::pair();

    app.mode = AppMode::CommandRunning(kojutsu::app::CommandRunningState::new(
        command,
        command_parts,
        kill_main,
    ));

    let repo_path = repo_path.to_path_buf();
    let event_tx = event_tx.clone();
    std::thread::spawn(move || {
        let result = cmd.run_cancellable(&repo_path, &kill_bg, |chunk| {
            let _ = event_tx.send(AppEvent::JjOutput {
                chunk: chunk.to_vec(),
            });
        });
        let _ = event_tx.send(AppEvent::JjDone {
            result: Box::new(result),
            cmd: Box::new(cmd),
            jump,
            label,
            for_lua,
        });
    });
}

/// A jj command yielded by a suspended Lua thread finished — log it, dismiss
/// the running overlay, and resume the thread with the result table.
fn resume_lua_jj(
    app: &mut App,
    result: Box<JJCommandResult>,
    lua_engine: &kojutsu::lua::LuaEngine,
) -> Action {
    app.push_command_log(
        kojutsu::app::CommandLogKind::Command,
        &result.display,
        Some(result.display_parts.clone()),
        result.output.clone(),
        result.success,
    );
    if matches!(app.mode, AppMode::CommandRunning(_)) {
        app.mode = AppMode::Normal;
    }
    match lua_engine.resume_suspended(app, kojutsu::lua::ResumeValue::JjResult(result)) {
        kojutsu::lua::ResumeResult::Action(action) => action,
        kojutsu::lua::ResumeResult::DispatchAction { action, flags } => {
            Action::DeferredDispatch { action, flags }
        }
    }
}

/// Run post-hooks for a command that ran while the TUI was suspended.
/// If a hook yields a jj command, start it for the suspended thread.
fn run_post_hooks_after_suspend(
    app: &mut App,
    lua_engine: &kojutsu::lua::LuaEngine,
    label: &'static str,
    repo_path: &std::path::Path,
    event_tx: &mpsc::Sender<AppEvent>,
) {
    let (success, output) = extract_command_result(app);
    let outcome = lua_engine.run_post_hooks(
        label,
        app,
        kojutsu::lua::CommandOutcome {
            success,
            cancelled: false,
            code: None,
            output: &output,
        },
    );
    if let kojutsu::lua::HookOutcome::Suspended(Action::RunJjForLua(cmd)) = outcome {
        run_jj_command(app, repo_path, cmd, None, event_tx, true);
    }
}

/// Process the result of a completed background jj command.
fn finish_jj_command(
    app: &mut App,
    result: JJCommandResult,
    cmd: JJCommand,
    jump: Option<JumpTarget>,
    label: Option<&'static str>,
    lua_engine: &kojutsu::lua::LuaEngine,
) -> Action {
    let (cancelled, code) = (result.cancelled, result.code);
    app.push_command_log(
        kojutsu::app::CommandLogKind::Command,
        &result.display,
        Some(result.display_parts.clone()),
        result.output.clone(),
        result.success,
    );
    let retry = cmd.retry_options(&result.output);
    app.mode = AppMode::command_output(
        result.display,
        Some(result.display_parts),
        result.output,
        result.success,
        retry,
    );

    if result.success {
        let switch_to_dag = match &jump {
            Some(JumpTarget::WorkingCopy | JumpTarget::Prefix(_)) => true,
            Some(JumpTarget::Bookmark(_)) => false,
            None => app.active_view == kojutsu::app::ActiveView::Evolog,
        };
        app.jump_after_refresh = jump;
        app.clear_selection();
        if switch_to_dag && app.active_view != kojutsu::app::ActiveView::Dag {
            app.switch_view(kojutsu::app::ActiveView::Dag);
        }
        // The command already snapshotted the working copy when it started
        // (unless run with --ignore-working-copy, where skipping is wanted),
        // so skip the redundant re-scan.
        refresh_app(app, RevsetLoadKind::NoSnapshot);
    }

    if let Some(lbl) = label {
        let (success, output) = extract_command_result(app);
        let outcome = lua_engine.run_post_hooks(
            lbl,
            app,
            kojutsu::lua::CommandOutcome {
                success,
                cancelled,
                code,
                output: &output,
            },
        );
        if let kojutsu::lua::HookOutcome::Suspended(action) = outcome {
            return action;
        }
    }
    Action::None
}

/// Initialize tracing subscriber writing to a log file.
/// Best-effort: if file creation fails, tracing is silently disabled.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let Some(cache_dir) = dirs::cache_dir() else {
        return;
    };
    let log_dir = cache_dir.join("kojutsu");
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    let log_file = log_dir.join("kojutsu.log");
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
    else {
        return;
    };

    let filter =
        EnvFilter::try_from_env("KOJUTSU_LOG").unwrap_or_else(|_| EnvFilter::new("kojutsu=warn"));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .with_target(false)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Find the nearest ancestor directory containing a `.jj/` workspace.
fn open_file_in_editor(
    repo_path: &std::path::Path,
    file_path: &str,
    line: usize,
    terminal: &mut kojutsu::terminal::Term,
) {
    let full_path = repo_path.join(file_path);
    let plus_line = format!("+{line}");
    let _ = run_editor_suspended(
        terminal,
        &[std::ffi::OsStr::new(&plus_line), full_path.as_os_str()],
    );
}

fn open_revision_in_editor(
    repo_path: &std::path::Path,
    commit_id: &kojutsu::types::CommitId,
    file_path: &kojutsu::types::RepoPath,
    line: usize,
    terminal: &mut kojutsu::terminal::Term,
) {
    // Extract file extension for the temp file name.
    let ext = file_path.as_str().rsplit('.').next().unwrap_or("txt");
    let name = file_path.as_str().rsplit('/').next().unwrap_or("file");

    let content = (|| -> color_eyre::Result<Vec<u8>> {
        let _ = JjRepo::snapshot(repo_path);
        let jj = JjRepo::open(repo_path)?;
        jj.get_file_at_commit(commit_id, file_path)
    })();

    match content {
        Ok(bytes) => {
            let mut tmpfile = match tempfile::Builder::new()
                .prefix(name)
                .suffix(&format!(".{ext}"))
                .tempfile()
            {
                Ok(f) => f,
                Err(_) => return,
            };
            use std::io::Write;
            let _ = tmpfile.write_all(&bytes);
            let _ = tmpfile.flush();
            let plus_line = format!("+{line}");
            let _ = run_editor_suspended(
                terminal,
                &[std::ffi::OsStr::new(&plus_line), tmpfile.path().as_os_str()],
            );
        }
        Err(e) => {
            tracing::warn!("failed to get file at revision: {e}");
        }
    }
}

/// Matches jj CLI behavior (`cli_util.rs::find_workspace_dir`).
fn find_workspace_dir(cwd: &std::path::Path) -> &std::path::Path {
    cwd.ancestors()
        .find(|path| path.join(".jj").is_dir())
        .unwrap_or(cwd)
}
