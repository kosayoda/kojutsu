use std::path::PathBuf;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::Result;
use crossterm::event::{Event, KeyEventKind};

use kojutsu::app::{App, AppMode, DeferredWork, JumpTarget, Loadable};
use kojutsu::input::{self, Action};
use kojutsu::jj_command::JJCommand;
use kojutsu::keymap::Keymaps;
use kojutsu::repo::JjRepo;
use kojutsu::repo_service::{RepoRequestHandle, RepoResult, RepoService};
use kojutsu::terminal::spawn_terminal_events;
use kojutsu::ui;

enum AppEvent {
    Init,
    Terminal(Event),
    Repo(Box<RepoResult>),
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

    /// Internal: apply diff selection as a diff tool (invoked by jj).
    #[arg(long, hide = true)]
    apply_diff: Option<PathBuf>,

    /// Internal: left directory for diff tool mode (positional).
    #[arg(hide = true)]
    diff_left: Option<PathBuf>,

    /// Internal: right directory for diff tool mode (positional).
    #[arg(hide = true)]
    diff_right: Option<PathBuf>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    let cli = Cli::parse();

    if cli.print_default_config {
        print!("{}", kojutsu::theme::DEFAULT_CONFIG);
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
    let keymaps: &'static Keymaps = Box::leak(Box::new(Keymaps::default()));
    let (event_tx, event_rx) = mpsc::channel();
    let (repo_requests, repo_responses) = RepoService::spawn(repo_path.clone());
    let _repo_forwarder =
        repo_responses.spawn_forwarder(event_tx.clone(), |r| AppEvent::Repo(Box::new(r)));
    let persisted = kojutsu::app::load_persisted_state();
    // Resolve active preset: persisted → first preset → None (jj default).
    let active_preset = persisted
        .active_preset
        .filter(|&i| i < config.presets.len())
        .or(if config.presets.is_empty() {
            None
        } else {
            Some(0)
        });
    let requested_revset = cli.revisions.clone().or_else(|| {
        active_preset
            .and_then(|i| config.presets.get(i))
            .map(|p| p.revset.clone())
    });
    let mut app = App::new(
        Vec::new(),
        requested_revset.clone().unwrap_or_default(),
        repo_path.display().to_string(),
        &config.presets,
        &config.glyphs,
    );
    app.default_search_scopes = config.default_search_scopes.to_flags();
    app.search_scopes = app.default_search_scopes;
    app.apply_persisted_state(&persisted);
    app.revset.active_preset = active_preset;
    app.request_revset_load(requested_revset);
    flush_repo_requests(&mut app, &repo_requests);
    let mut terminal = kojutsu::terminal::init()?;
    let mut terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
    let _ = event_tx.send(AppEvent::Init);

    let mut dirty = true;
    let mut events: Vec<AppEvent> = Vec::new();
    loop {
        if dirty {
            terminal.draw(|frame| ui::draw(frame, &mut app, keymaps, config))?;
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
                AppEvent::Terminal(ev) => match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        dirty = true;
                        input::handle_key(&mut app, keymaps, key)
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
        if deferred.rebuild {
            app.rebuild_rows();
        }
        if deferred.scroll {
            app.scroll_to_show_children();
        }

        // Handle breaking action.
        match breaking_action.unwrap_or(Action::None) {
            Action::Quit => break,
            Action::RunJj(cmd) => {
                run_jj_command(&mut app, &repo_path, cmd);
            }
            Action::SuspendAndRunJj(cmd) => {
                terminal_events.stop();
                suspend_and_run(&mut app, &repo_path, &mut terminal, cmd);
                terminal_events = spawn_terminal_events(event_tx.clone(), AppEvent::Terminal);
            }
            Action::Refresh => {
                refresh_app(&mut app);
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
            Action::None => {}
        }
        flush_repo_requests(&mut app, &repo_requests);
    }

    kojutsu::app::save_persisted_state(&app.to_persisted_state());
    terminal_events.stop();
    kojutsu::terminal::restore()?;
    Ok(())
}

fn flush_repo_requests(app: &mut App, service: &RepoRequestHandle) {
    for request in app.take_repo_requests() {
        service.send(request);
    }
}

fn refresh_app(app: &mut App) {
    let revset = match &app.revset.load_state {
        Loadable::Loading => app.revset.pending.as_ref().map(|s| s.to_string()),
        _ => Some(app.revset.current.to_string()),
    };
    app.request_revset_load(revset);
}

fn suspend_and_run(
    app: &mut App,
    repo_path: &std::path::Path,
    terminal: &mut kojutsu::terminal::Term,
    cmd: JJCommand,
) {
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
    *terminal = kojutsu::terminal::init().expect("failed to re-init terminal");

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
        refresh_app(app);
    }

    // Show output if there is any, or if the command failed (so failures are
    // always visible even when interactive commands print to inherited stdio).
    if !result.success || !result.output.is_empty() {
        let retry = cmd.retry_options(&result.output);
        app.mode = AppMode::CommandOutput {
            command: result.display,
            command_parts: Some(result.display_parts),
            output: result.output,
            success: result.success,
            retry,
        };
    }
}

fn update_revset(app: &mut App, revset_str: String) {
    app.request_revset_load(Some(revset_str));
}

fn edit_revset_in_editor(app: &mut App, terminal: &mut kojutsu::terminal::Term) {
    use std::io::Write;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let revset_text = app.revset_input_text();

    // Write current revset to a temp file.
    let mut tmpfile = match tempfile::NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            app.mode = AppMode::CommandOutput {
                command: "revset editor".to_string(),
                command_parts: None,
                output: format!("failed to create temp file: {e}").into_bytes(),
                success: false,
                retry: vec![],
            };
            return;
        }
    };
    let _ = writeln!(tmpfile, "{revset_text}");
    let path = tmpfile.path().to_path_buf();

    // Suspend TUI and open editor.
    let _ = kojutsu::terminal::restore();
    let status = std::process::Command::new(&editor).arg(&path).status();
    *terminal = kojutsu::terminal::init().expect("failed to re-init terminal");

    match status {
        Ok(s) if s.success() => {
            // Read back the edited revset.
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let new_revset = content.trim().to_string();
                    if !new_revset.is_empty() {
                        update_revset(app, new_revset);
                    }
                }
                Err(e) => {
                    app.mode = AppMode::CommandOutput {
                        command: "revset editor".to_string(),
                        command_parts: None,
                        output: format!("failed to read temp file: {e}").into_bytes(),
                        success: false,
                        retry: vec![],
                    };
                }
            }
        }
        Ok(_) => {
            // Editor exited with non-zero -- user cancelled.
        }
        Err(e) => {
            app.mode = AppMode::CommandOutput {
                command: "revset editor".to_string(),
                command_parts: None,
                output: format!("failed to run {editor}: {e}").into_bytes(),
                success: false,
                retry: vec![],
            };
        }
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
        if c.is_divergent {
            flags.push("divergent");
        }
        if c.is_hidden {
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
            .change_id_suffix
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

fn run_jj_command(app: &mut App, repo_path: &std::path::Path, cmd: JJCommand) {
    let jump = cmd.jump_target();
    let result = cmd.run(repo_path);

    app.push_command_log(
        kojutsu::app::CommandLogKind::Command,
        &result.display,
        Some(result.display_parts.clone()),
        result.output.clone(),
        result.success,
    );
    let retry = cmd.retry_options(&result.output);
    app.mode = AppMode::CommandOutput {
        command: result.display,
        command_parts: Some(result.display_parts),
        output: result.output,
        success: result.success,
        retry,
    };

    if result.success {
        // Decide whether to switch to DAG before moving jump into app state.
        let switch_to_dag = match &jump {
            Some(JumpTarget::WorkingCopy | JumpTarget::ChangeId(_)) => true,
            Some(JumpTarget::Bookmark(_)) => false,
            None => app.active_view == kojutsu::app::ActiveView::Evolog,
        };
        app.jump_after_refresh = jump;
        app.clear_selection();
        if switch_to_dag && app.active_view != kojutsu::app::ActiveView::Dag {
            app.switch_view(kojutsu::app::ActiveView::Dag);
        }
        refresh_app(app);
    }
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
