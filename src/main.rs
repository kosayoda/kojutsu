use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;

use clap::Parser;
use color_eyre::Result;
use crossterm::event::{self, Event, KeyEventKind};

use kojutsu::app::{App, AppMode, Loadable};
use kojutsu::input::{self, Action};
use kojutsu::jj_command::JJCommand;
use kojutsu::keymap::Keymap;
use kojutsu::repo::JjRepo;
use kojutsu::repo_service::{RepoRequestHandle, RepoResult, RepoService};
use kojutsu::ui;

enum AppEvent {
    Init,
    Terminal(Event),
    Repo(RepoResult),
}

struct TerminalEvents {
    waker: Arc<mio::Waker>,
    join: thread::JoinHandle<()>,
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
    let cli = Cli::parse();

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
        let entries = jj.evaluate_revset(&revset)?;
        debug_print_graph(&entries);
        return Ok(());
    }

    let keymap: &'static Keymap = Box::leak(Box::new(Keymap::default()));
    let (event_tx, event_rx) = mpsc::channel();
    let (repo_requests, repo_responses) = RepoService::spawn(repo_path.clone());
    let _repo_forwarder = repo_responses.spawn_forwarder(event_tx.clone(), AppEvent::Repo);
    let persisted = kojutsu::app::load_persisted_state(&repo_path);
    let requested_revset = cli.revisions.clone().or_else(|| persisted.revset.clone());
    let mut app = App::new(
        Vec::new(),
        requested_revset.clone().unwrap_or_default(),
        repo_path.display().to_string(),
    );
    app.apply_persisted_state(&persisted);
    app.request_revset_load(requested_revset);
    flush_repo_requests(&mut app, &repo_requests);
    let mut terminal = kojutsu::terminal::init()?;
    let mut terminal_events = spawn_terminal_events(event_tx.clone());
    let _ = event_tx.send(AppEvent::Init);

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app, keymap))?;

        let app_event = match event_rx.recv() {
            Ok(event) => event,
            Err(_) => break,
        };

        let action = match app_event {
            AppEvent::Init => Action::None,
            AppEvent::Repo(result) => {
                app.handle_repo_result(result);
                Action::None
            }
            AppEvent::Terminal(ev) => match ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    input::handle_key(&mut app, keymap, key)
                }
                Event::Mouse(mouse) => {
                    let hdr = app.last_header_height;
                    input::handle_mouse(&mut app, mouse, hdr)
                }
                _ => Action::None,
            },
        };
        match action {
            Action::Quit => break,
            Action::RunJj(cmd) => {
                run_jj_command(&mut app, &repo_path, cmd);
            }
            Action::SuspendAndRunJj(cmd) => {
                terminal_events.stop();
                suspend_and_run(&mut app, &repo_path, &mut terminal, cmd);
                terminal_events = spawn_terminal_events(event_tx.clone());
            }
            Action::Refresh => {
                refresh_app(&mut app);
            }
            Action::UpdateRevset(revset_str) => {
                update_revset(&mut app, revset_str);
            }
            Action::EditRevsetInEditor => {
                terminal_events.stop();
                edit_revset_in_editor(&mut app, &mut terminal);
                terminal_events = spawn_terminal_events(event_tx.clone());
            }
            Action::None => {}
        }
        flush_repo_requests(&mut app, &repo_requests);
    }

    kojutsu::app::save_persisted_state(&repo_path, &app.to_persisted_state());
    terminal_events.stop();
    kojutsu::terminal::restore()?;
    Ok(())
}

fn flush_repo_requests(app: &mut App, service: &RepoRequestHandle) {
    for request in app.take_repo_requests() {
        service.send(request);
    }
}

const STDIN_TOKEN: mio::Token = mio::Token(0);
const WAKE_TOKEN: mio::Token = mio::Token(1);

fn spawn_terminal_events(event_tx: mpsc::Sender<AppEvent>) -> TerminalEvents {
    let poll = mio::Poll::new().expect("failed to create mio Poll");
    let waker =
        Arc::new(mio::Waker::new(poll.registry(), WAKE_TOKEN).expect("failed to create Waker"));

    let waker_clone = Arc::clone(&waker);
    let join = thread::spawn(move || {
        let mut poll = poll;
        let stdin_fd = std::io::stdin().as_raw_fd();
        let mut source = mio::unix::SourceFd(&stdin_fd);
        poll.registry()
            .register(&mut source, STDIN_TOKEN, mio::Interest::READABLE)
            .expect("failed to register stdin");

        let mut events = mio::Events::with_capacity(2);
        loop {
            if poll.poll(&mut events, None).is_err() {
                break;
            }
            for ev in &events {
                match ev.token() {
                    WAKE_TOKEN => return,
                    STDIN_TOKEN => {
                        // Read the first event, then drain any events
                        // crossterm buffered internally since mio won't
                        // re-trigger for bytes already consumed from stdin.
                        loop {
                            match event::read() {
                                Ok(ev) => {
                                    if event_tx.send(AppEvent::Terminal(ev)).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => return,
                            }
                            if !event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    TerminalEvents {
        waker: waker_clone,
        join,
    }
}

impl TerminalEvents {
    fn stop(self) {
        let _ = self.waker.wake();
        let _ = self.join.join();
    }
}

fn refresh_app(app: &mut App) {
    let revset = match &app.revset_state {
        Loadable::Loading => app.pending_revset.clone(),
        _ => Some(app.revset.clone()),
    };
    app.request_revset_load(revset);
}

fn suspend_and_run(
    app: &mut App,
    repo_path: &std::path::Path,
    terminal: &mut kojutsu::terminal::Term,
    cmd: JJCommand,
) {
    // Store the display string and jump target before running (cmd is consumed).
    let display = cmd.display();
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

    app.last_command = Some(display);

    if result.success {
        app.jump_after_refresh = jump;
        app.clear_selection();
        refresh_app(app);
    }

    // If there was output (e.g. error), show it. Otherwise stay in Normal mode.
    if !result.output.is_empty() {
        app.mode = AppMode::CommandOutput {
            command: result.display,
            output: result.output,
            success: result.success,
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
                output: format!("failed to create temp file: {e}").into_bytes(),
                success: false,
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
                        output: format!("failed to read temp file: {e}").into_bytes(),
                        success: false,
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
                output: format!("failed to run {editor}: {e}").into_bytes(),
                success: false,
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

    app.last_command = Some(result.display.clone());
    app.mode = AppMode::CommandOutput {
        command: result.display,
        output: result.output,
        success: result.success,
    };

    if result.success {
        app.jump_after_refresh = jump;
        app.clear_selection();
        refresh_app(app);
    }
}

/// Find the nearest ancestor directory containing a `.jj/` workspace.
/// Matches jj CLI behavior (`cli_util.rs::find_workspace_dir`).
fn find_workspace_dir(cwd: &std::path::Path) -> &std::path::Path {
    cwd.ancestors()
        .find(|path| path.join(".jj").is_dir())
        .unwrap_or(cwd)
}
