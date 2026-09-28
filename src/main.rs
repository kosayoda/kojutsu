use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::Result;
use crossterm::event::{Event, KeyEventKind};

use kojutsu::app::{App, AppMode, DeferredWork};
use kojutsu::input::{self, Action};
use kojutsu::jj_command::{JJCommand, JJCommandResult, TerminalUse};
use kojutsu::repo::JjRepo;
use kojutsu::repo_service::{
    CancellationToken, RepoRequestHandle, RepoResult, RepoService, RevsetLoadKind,
};
use kojutsu::terminal::{TerminalEvents, spawn_terminal_events};
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
        completion: Completion,
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

    /// Print shortest unique change/commit ID prefixes and exit. Comparable
    /// against `jj log -T 'change_id.shortest().prefix()'`: see `just
    /// check-prefixes`.
    #[arg(long)]
    debug_prefixes: bool,

    /// Annotate a file and print a one-line-per-line summary, then exit.
    /// Use with the `blame` subcommand's arguments.
    #[arg(long)]
    debug_annotate: bool,

    /// Print a starter init.lua to stdout and exit.
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
        print!("{}", kojutsu::lua::DEFAULT_INIT);
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
        if cli.debug_annotate {
            let result = jj.file_annotate(
                &kojutsu::types::CommitId::new(commit_hex.as_str()),
                &kojutsu::types::RepoPath::new(internal_path.as_str()),
            )?;
            for line in &result.lines {
                println!(
                    "{} {} {}",
                    line.change_id.prefix(),
                    line.line_number,
                    line.content
                );
            }
            return Ok(());
        }
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

    if cli.debug_prefixes {
        let _ = JjRepo::snapshot(&repo_path);
        let jj = JjRepo::open(&repo_path)?;
        let revset = cli.revisions.unwrap_or_else(|| jj.default_revset());
        let result = jj.evaluate_revset(&revset)?;
        for w in &result.warnings {
            eprintln!("warning: {w}");
        }
        let mut result = result;
        debug_print_prefixes(&jj, &mut result.entries)?;
        return Ok(());
    }

    let runtime = kojutsu::lua::LuaRuntime::load(&repo_path);
    let config = runtime.config.clone();
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
        config.clone(),
    );
    app.apply_persisted_state(&persisted);
    app.revset.active_preset = active_preset;
    app.request_revset_load(requested_revset, RevsetLoadKind::Snapshot);

    // If launched with `blame`, enter annotate view immediately.
    if let Some((commit_hex, internal_path)) = blame_target {
        let commit_id = kojutsu::types::CommitId::new(&commit_hex);
        let path = kojutsu::types::RepoPath::new(&internal_path);
        app.enter_annotate_view(commit_id, path);
    }

    if let Some(path) = kojutsu::theme::superseded_toml_config() {
        let msg = format!(
            "{} is no longer read - its settings now live in init.lua \
             (see --print-default-config)",
            path.display()
        );
        app.push_command_log(
            kojutsu::app::CommandLogKind::Warning,
            msg.clone(),
            None,
            Vec::new(),
            false,
        );
        app.set_error(msg);
    }

    if let Some(err) = runtime.init_error() {
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
    let screen = Screen::open(event_tx.clone())?;
    let _ = event_tx.send(AppEvent::Init);

    let mut session = Session {
        app,
        runtime,
        repo_path,
        repo_requests,
        event_tx,
        screen,
    };
    session.run(&event_rx)?;
    session.close()
}

/// What to do once a background jj command finishes.
enum Completion {
    /// Refresh, then run the post-hooks of the action that issued it, if a
    /// labelled action did.
    Refresh { hook_label: Option<&'static str> },
    /// Resume the suspended Lua thread that yielded it with its result.
    ResumeLua,
}

/// The terminal and the thread reading its events. They are handed to a
/// child process together, so the thread doesn't steal the child's input.
struct Screen {
    terminal: kojutsu::terminal::Term,
    events: Option<TerminalEvents>,
    event_tx: mpsc::Sender<AppEvent>,
}

impl Screen {
    fn open(event_tx: mpsc::Sender<AppEvent>) -> Result<Self> {
        let terminal = kojutsu::terminal::init()?;
        let events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
        Ok(Self {
            terminal,
            events: Some(events),
            event_tx,
        })
    }

    /// Give the terminal to a child process for the duration of `f`, then
    /// take it back. Failing to take it back is fatal: the TUI can't go on.
    fn suspended<T>(&mut self, f: impl FnOnce() -> T) -> T {
        if let Some(events) = self.events.take() {
            events.stop();
        }
        let _ = kojutsu::terminal::restore();
        let out = f();
        self.terminal = kojutsu::terminal::init().unwrap_or_else(|e| {
            eprintln!("fatal: failed to re-init terminal: {e}");
            std::process::exit(1);
        });
        self.events = Some(spawn_terminal_events(
            self.event_tx.clone(),
            AppEvent::Terminal,
        ));
        out
    }

    fn close(mut self) -> Result<()> {
        if let Some(events) = self.events.take() {
            events.stop();
        }
        kojutsu::terminal::restore()?;
        Ok(())
    }
}

/// Everything the event loop acts on.
struct Session {
    app: App,
    runtime: kojutsu::lua::LuaRuntime,
    repo_path: PathBuf,
    repo_requests: RepoRequestHandle,
    event_tx: mpsc::Sender<AppEvent>,
    screen: Screen,
}

impl Session {
    /// Draw, wait for events, handle them in batches, until told to quit.
    fn run(&mut self, event_rx: &mpsc::Receiver<AppEvent>) -> Result<()> {
        let mut dirty = true;
        let mut events: Vec<AppEvent> = Vec::new();
        loop {
            if dirty {
                let (app, keymaps) = (&mut self.app, &self.runtime.keymaps);
                self.screen
                    .terminal
                    .draw(|frame| ui::draw(frame, app, keymaps))?;
                dirty = false;
            }

            // Block for the first event, then drain the rest so they are
            // handled as one batch.
            let Ok(first) = event_rx.recv() else {
                return Ok(());
            };
            events.clear();
            events.push(first);
            events.extend(event_rx.try_iter());

            let mut deferred = DeferredWork::default();
            let mut breaking_action = Action::None;
            for event in events.drain(..) {
                if !matches!(event, AppEvent::Init | AppEvent::Terminal(_)) {
                    dirty = true;
                }
                let action = self.handle_event(event, &mut deferred, &mut dirty);
                if !matches!(action, Action::None) {
                    breaking_action = action;
                    break;
                }
            }

            // Apply deferred work once for the whole batch.
            self.app.apply_rebuild(deferred.rebuild);
            if deferred.scroll {
                self.app.scroll_to_show_children();
            }
            if let Some(view) = deferred.file_view {
                self.view_file(view);
            }

            if self.execute(breaking_action).is_break() {
                return Ok(());
            }
            self.runtime.engine.flush_logs(&mut self.app);
            sync_plugin_config(&mut self.app, &mut self.runtime, &self.repo_requests);
            flush_repo_requests(&mut self.app, &self.repo_requests);
        }
    }

    fn handle_event(
        &mut self,
        event: AppEvent,
        deferred: &mut DeferredWork,
        dirty: &mut bool,
    ) -> Action {
        match event {
            AppEvent::Init => Action::None,
            AppEvent::Repo(result) => {
                deferred.merge(self.app.handle_repo_result_deferred(*result));
                Action::None
            }
            AppEvent::JjOutput { chunk } => {
                self.app.append_running_output(&chunk);
                Action::None
            }
            AppEvent::JjDone {
                result,
                cmd,
                jump,
                completion,
            } => match completion {
                Completion::ResumeLua => resume_lua_jj(&mut self.app, result, &self.runtime.engine),
                Completion::Refresh { hook_label } => finish_jj_command(
                    &mut self.app,
                    *result,
                    *cmd,
                    jump,
                    hook_label,
                    &self.runtime.engine,
                ),
            },
            AppEvent::Terminal(ev) => match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    *dirty = true;
                    input::handle_key(
                        &mut self.app,
                        &self.runtime.keymaps,
                        &self.runtime.engine,
                        key,
                    )
                }
                Event::Mouse(mouse) => {
                    *dirty = true;
                    let hdr = self.app.last_header_height;
                    input::handle_mouse(&mut self.app, mouse, hdr)
                }
                Event::Resize(..) => {
                    *dirty = true;
                    Action::None
                }
                _ => Action::None,
            },
        }
    }

    /// Carry out an action, and whatever action it leads to. Breaks to quit.
    fn execute(&mut self, action: Action) -> ControlFlow<()> {
        match action {
            Action::None => {}
            Action::Quit => return ControlFlow::Break(()),
            Action::RunJj(cmd) => {
                let hook_label = self.app.last_action_label.take();
                if cmd.terminal_use() == TerminalUse::Background {
                    self.run_jj(cmd, Completion::Refresh { hook_label });
                } else {
                    let result = self
                        .screen
                        .suspended(|| run_in_foreground(&cmd, &self.repo_path));
                    finish_foreground_command(&mut self.app, &cmd, result);
                    if let Some(label) = hook_label {
                        return self.run_post_hooks_after_suspend(label);
                    }
                }
            }
            Action::RunJjForLua(cmd) => {
                if cmd.terminal_use() == TerminalUse::Background {
                    self.run_jj(cmd, Completion::ResumeLua);
                } else {
                    let result = self
                        .screen
                        .suspended(|| run_in_foreground(&cmd, &self.repo_path));
                    let next = resume_lua_jj(&mut self.app, Box::new(result), &self.runtime.engine);
                    return self.execute(next);
                }
            }
            Action::Refresh => self.app.refresh(RevsetLoadKind::Snapshot),
            Action::ReloadConfig => {
                reload_config(
                    &mut self.app,
                    &mut self.runtime,
                    &self.repo_path,
                    &self.repo_requests,
                );
            }
            Action::UpdateRevset(revset_str) => update_revset(&mut self.app, revset_str),
            Action::EditRevsetInEditor => {
                self.app.revset.active_preset = None;
                self.edit_revset();
            }
            Action::EditWorkingCopyFile { path, line } => {
                let path = self.repo_path.join(path);
                self.screen.suspended(|| open_in_editor(&path, line));
            }
            Action::EditConflictFile {
                change_id,
                path,
                content,
                flags,
            } => {
                let edited = self
                    .screen
                    .suspended(|| edit_in_editor(&content, path.as_str()));
                match edited {
                    EditOutcome::Edited(edited) => {
                        if let Some(cmd) = kojutsu::input::staged_resolution(
                            &mut self.app,
                            change_id,
                            &path,
                            &edited,
                            flags,
                        ) {
                            return self.execute(Action::RunJj(cmd));
                        }
                    }
                    outcome => report_unapplied_edit(&mut self.app, outcome),
                }
            }
            Action::EditConflictHunk {
                commit_id,
                path,
                hunk_idx,
                seed,
                flags,
            } => {
                let edited = self
                    .screen
                    .suspended(|| edit_in_editor(&seed, path.as_str()));
                match edited {
                    EditOutcome::Edited(edited) => {
                        kojutsu::input::complete_hunk_edit(
                            &mut self.app,
                            &commit_id,
                            &path,
                            hunk_idx,
                            &edited,
                            flags,
                        );
                    }
                    outcome => report_unapplied_edit(&mut self.app, outcome),
                }
            }
            Action::CheckoutAndEdit {
                commit_id,
                path,
                line,
            } => {
                let cmd = JJCommand {
                    kind: kojutsu::jj_command::JJCommandKind::New {
                        change_ids: smallvec::smallvec![kojutsu::types::RevisionArg::new(
                            commit_id.as_str()
                        )],
                        insert: None,
                    },
                    flags: kojutsu::keymap::CommandFlags::empty(),
                };
                let path = self.repo_path.join(path);
                // One suspension for both, so the TUI doesn't flash between.
                let result = self.screen.suspended(|| {
                    let result = run_in_foreground(&cmd, &self.repo_path);
                    if result.success {
                        open_in_editor(&path, line);
                    }
                    result
                });
                finish_foreground_command(&mut self.app, &cmd, result);
            }
            Action::DeferredDispatch { action, flags } => {
                let keymap = self.runtime.keymaps.for_view(self.app.active_view);
                let next = input::dispatch_action_after_hooks(
                    &mut self.app,
                    &self.runtime.keymaps.registry,
                    &self.runtime.engine,
                    keymap,
                    action,
                    flags,
                );
                return self.execute(next);
            }
        }
        ControlFlow::Continue(())
    }

    /// Spawn a background thread to run a non-interactive jj command.
    ///
    /// Sets `app.mode` to `CommandRunning` immediately so the UI shows
    /// progress, then sends `AppEvent::JjDone` when the child exits (or is
    /// cancelled via Esc, which kills the child process group).
    fn run_jj(&mut self, cmd: JJCommand, completion: Completion) {
        let jump = cmd.jump_target();
        let (kill_main, kill_bg) = kojutsu::jj_command::KillHandle::pair();
        self.app.mode = AppMode::CommandRunning(kojutsu::app::CommandRunningState::new(
            cmd.display(),
            cmd.display_parts(),
            kill_main,
        ));

        let repo_path = self.repo_path.clone();
        let event_tx = self.event_tx.clone();
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
                completion,
            });
        });
    }

    /// Run post-hooks for a command that ran while the TUI was suspended,
    /// and carry out whatever action a hook leads to.
    fn run_post_hooks_after_suspend(&mut self, label: &'static str) -> ControlFlow<()> {
        let (success, output) = extract_command_result(&self.app);
        let outcome = self.runtime.engine.run_post_hooks(
            label,
            &mut self.app,
            kojutsu::lua::CommandOutcome {
                success,
                cancelled: false,
                code: None,
                output: &output,
            },
        );
        match outcome {
            kojutsu::lua::HookOutcome::Suspended(action) => self.execute(action),
            kojutsu::lua::HookOutcome::Proceed | kojutsu::lua::HookOutcome::Cancel => {
                ControlFlow::Continue(())
            }
        }
    }

    /// Open the revset in `$EDITOR` and load what comes back.
    fn edit_revset(&mut self) {
        let text = format!("{}\n", self.app.revset_input_text());
        match self.screen.suspended(|| edit_in_editor(&text, "revset")) {
            EditOutcome::Edited(edited) => {
                let revset = edited.trim();
                if !revset.is_empty() {
                    update_revset(&mut self.app, revset.to_string());
                }
            }
            EditOutcome::Unchanged | EditOutcome::Cancelled => {}
            EditOutcome::Failed(e) => {
                self.app.mode = AppMode::command_output(
                    "revset editor".to_string(),
                    None,
                    e.into_bytes(),
                    false,
                    vec![],
                );
            }
        }
    }

    /// Open a file's content at a revision read-only in `$EDITOR`, from a
    /// temp file named like it so the editor picks the right syntax.
    fn view_file(&mut self, view: kojutsu::app::FileView) {
        let staged = temp_file_like(view.path.as_str(), &view.content);
        match staged {
            Ok(file) => {
                self.screen
                    .suspended(|| open_in_editor(file.path(), view.line));
            }
            Err(e) => self.app.set_error(format!("temp file: {e}")),
        }
    }

    fn close(self) -> Result<()> {
        kojutsu::app::save_persisted_state(&self.app.to_persisted_state());
        self.screen.close()
    }
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

/// Run a jj command on the real terminal. Interactive commands need full
/// terminal access; captured ones still need stdin for SSH password prompts.
fn run_in_foreground(cmd: &JJCommand, repo_path: &std::path::Path) -> JJCommandResult {
    match cmd.terminal_use() {
        TerminalUse::Interactive => cmd.run_interactive(repo_path),
        TerminalUse::Foreground | TerminalUse::Background => cmd.run_suspend_captured(repo_path),
    }
}

/// Record a command that ran on the real terminal, refresh after success,
/// and show its output if it printed any or failed.
fn finish_foreground_command(app: &mut App, cmd: &JJCommand, result: JJCommandResult) {
    app.push_command_log(
        kojutsu::app::CommandLogKind::Command,
        &result.display,
        Some(result.display_parts.clone()),
        result.output.bytes().to_vec(),
        result.success,
    );

    if result.success {
        app.jump_after_refresh = cmd.jump_target();
        app.clear_selection();
        // The user may have edited files while the terminal was suspended
        // (e.g. in $EDITOR), so re-scan the working copy.
        app.refresh(RevsetLoadKind::Snapshot);
    }

    // Show output if there is any, or if the command failed (so failures are
    // always visible even when interactive commands print to inherited stdio).
    if !result.success || !result.output.is_empty() {
        let retry = cmd.retry_options(result.output.bytes());
        app.mode = AppMode::command_output(
            result.display,
            Some(result.display_parts),
            result.output.into_bytes(),
            result.success,
            retry,
        );
    }
}

/// Point everything that holds config-derived state at `config`. The repo
/// service keeps its own copy of the diff limit, so it is told separately.
fn install_config(
    app: &mut App,
    repo_requests: &RepoRequestHandle,
    config: std::rc::Rc<kojutsu::theme::Config>,
) {
    repo_requests.send(kojutsu::repo_service::RepoRequest::SetDiffSizeLimit {
        bytes: config.diff.max_file_size_bytes(),
    });
    app.apply_reloaded_config(config);
}

/// Adopt whatever a plugin just did to `kojutsu.config`.
fn sync_plugin_config(
    app: &mut App,
    runtime: &mut kojutsu::lua::LuaRuntime,
    repo_requests: &RepoRequestHandle,
) {
    match runtime.engine.take_config_change(&runtime.config) {
        None => {}
        Some(Ok(config)) => {
            runtime.config = std::rc::Rc::new(config);
            install_config(app, repo_requests, runtime.config.clone());
        }
        Some(Err(e)) => app.set_error(e),
    }
}

/// Rebuild the runtime from `init.lua` and swap it in.
///
/// All-or-nothing: the new runtime is built first and only installed if its
/// script ran clean, so a broken edit leaves the running one untouched. The
/// rebuild is wholesale rather than a re-run over the live engine, because
/// registering a command or binding appends: a second pass would double
/// every one of them, and a fresh Lua also drops `package.loaded`, so
/// `require`d modules are re-read instead of served from cache.
fn reload_config(
    app: &mut App,
    runtime: &mut kojutsu::lua::LuaRuntime,
    repo_path: &std::path::Path,
    repo_requests: &RepoRequestHandle,
) {
    // A parked thread belongs to the engine that created it, and the reload
    // drops that engine along with the prompt it is waiting on.
    if runtime.engine.has_suspended_thread() {
        app.set_error("reload: a plugin prompt is still open");
        return;
    }

    let reloaded = kojutsu::lua::LuaRuntime::load(repo_path);
    if let Some(err) = reloaded.init_error() {
        app.push_command_log(
            kojutsu::app::CommandLogKind::Warning,
            "reload failed - keeping the running config",
            None,
            err.as_bytes().to_vec(),
            false,
        );
        app.set_error(format!("reload: {err}"));
        return;
    }

    *runtime = reloaded;
    install_config(app, repo_requests, runtime.config.clone());
    app.push_command_log(
        kojutsu::app::CommandLogKind::Background,
        "reloaded init.lua",
        None,
        Vec::new(),
        true,
    );
    app.set_status("reloaded init.lua");
}

fn update_revset(app: &mut App, revset_str: String) {
    // An explicit revset change leaves any active conflicted() toggle.
    app.revset.conflicted_prev = None;
    app.request_revset_load(Some(revset_str), RevsetLoadKind::NoSnapshot);
}

/// Run `$EDITOR` with `args`. The caller hands it the terminal.
fn run_editor(args: &[&std::ffi::OsStr]) -> std::io::Result<std::process::ExitStatus> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    std::process::Command::new(&editor).args(args).status()
}

/// Open `path` in `$EDITOR` at `line`.
fn open_in_editor(path: &std::path::Path, line: usize) {
    let plus_line = format!("+{line}");
    let _ = run_editor(&[std::ffi::OsStr::new(&plus_line), path.as_os_str()]);
}

/// A temp file holding `content`, named after `path`'s file name and
/// extension so the editor highlights it the same way.
fn temp_file_like(path: &str, content: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;

    let path = std::path::Path::new(path);
    let stem = path
        .file_stem()
        .map(|s| format!("{}.", s.to_string_lossy()))
        .unwrap_or_default();
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut file = tempfile::Builder::new()
        .prefix(&stem)
        .suffix(&suffix)
        .tempfile()?;
    file.write_all(content)?;
    file.flush()?;
    Ok(file)
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

/// Open `content` in `$EDITOR` (in a temp file named like `path`) and
/// report what happened. The caller hands it the terminal.
fn edit_in_editor(content: &str, path: &str) -> EditOutcome {
    let file = match temp_file_like(path, content.as_bytes()) {
        Ok(f) => f,
        Err(e) => return EditOutcome::Failed(format!("temp file: {e}")),
    };
    match run_editor(&[file.path().as_os_str()]) {
        Ok(s) if s.success() => match std::fs::read_to_string(file.path()) {
            Ok(edited) if edited != content => EditOutcome::Edited(edited),
            Ok(_) => EditOutcome::Unchanged,
            Err(e) => EditOutcome::Failed(format!("read: {e}")),
        },
        Ok(_) => EditOutcome::Cancelled,
        Err(e) => EditOutcome::Failed(format!("run editor: {e}")),
    }
}

/// Tell the user why an edit made no change.
fn report_unapplied_edit(app: &mut App, outcome: EditOutcome) {
    match outcome {
        EditOutcome::Failed(e) => app.set_error(format!("editor: {e}")),
        EditOutcome::Unchanged | EditOutcome::Cancelled => {
            app.set_status("edit cancelled - no changes")
        }
        EditOutcome::Edited(_) => {}
    }
}

/// Print `<change prefix>|<commit prefix>` per commit, one line each, using the
/// same code path the DAG view does. Output is directly comparable against
/// `jj log -T 'change_id.shortest().prefix()'`; any divergence means IDs shown
/// in the TUI won't round-trip as `jj` revision arguments.
fn debug_print_prefixes(jj: &JjRepo, entries: &mut [kojutsu::dag::DagEntry]) -> Result<()> {
    let commit_ids: Vec<_> = entries.iter().map(|e| e.commit.graph_id.clone()).collect();
    let updates: std::collections::HashMap<_, _> = jj
        .compute_prefix_lengths(&commit_ids, &CancellationToken::new())?
        .into_iter()
        .collect();

    for entry in entries {
        let commit = &mut entry.commit;
        let Some(update) = updates.get(&commit.graph_id) else {
            continue;
        };
        commit.change_id.set_prefix_len(update.change_prefix_len);
        commit.commit_id.set_prefix_len(update.commit_prefix_len);
        println!(
            "{}|{}",
            commit.change_id.prefix(),
            commit.commit_id.prefix()
        );
    }
    Ok(())
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
            c.change_id.display(),
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

/// A jj command yielded by a suspended Lua thread finished: log it, dismiss
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
        result.output.bytes().to_vec(),
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
        result.output.bytes().to_vec(),
        result.success,
    );
    let retry = cmd.retry_options(result.output.bytes());
    app.mode = AppMode::command_output(
        result.display,
        Some(result.display_parts),
        result.output.into_bytes(),
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
        app.refresh(RevsetLoadKind::NoSnapshot);
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
/// Matches jj CLI behavior (`cli_util.rs::find_workspace_dir`).
fn find_workspace_dir(cwd: &std::path::Path) -> &std::path::Path {
    cwd.ancestors()
        .find(|path| path.join(".jj").is_dir())
        .unwrap_or(cwd)
}
