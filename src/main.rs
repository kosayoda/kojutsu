use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use ratatui::crossterm::event::{self, Event};

use kojutsu::app::{App, AppMode};
use kojutsu::input::{self, Action};
use kojutsu::jj_command::JJCommand;
use kojutsu::keymap::Keymap;
use kojutsu::repo::JjRepo;
use kojutsu::ui;

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
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let repo_path = cli.repository.canonicalize().unwrap_or(cli.repository);
    JjRepo::snapshot(&repo_path);
    let mut jj = JjRepo::open(&repo_path)?;
    let revset = cli.revisions.unwrap_or_else(|| jj.default_revset());
    let entries = jj.evaluate_revset(&revset)?;

    if cli.debug_graph {
        debug_print_graph(&entries);
        return Ok(());
    }

    let repo_root = jj.workspace_root().display().to_string();
    let keymap: &'static Keymap = Box::leak(Box::new(Keymap::default()));

    let mut app = App::new(entries, revset, repo_root);
    let mut terminal = kojutsu::terminal::init()?;

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app, keymap))?;

        if event::poll(Duration::from_millis(200))? {
            let ev = event::read()?;
            let action = match ev {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    input::handle_key(&mut app, &jj, keymap, key)
                }
                Event::Mouse(mouse) => {
                    let hdr = app.last_header_height;
                    input::handle_mouse(&mut app, &jj, mouse, hdr)
                }
                _ => Action::None,
            };
            match action {
                Action::Quit => break,
                Action::RunJj(cmd) => {
                    run_jj_command(&mut app, &mut jj, &repo_path, cmd);
                }
                Action::SuspendAndRunJj(cmd) => {
                    suspend_and_run(&mut app, &mut jj, &repo_path, &mut terminal, cmd);
                }
                Action::Refresh => {
                    refresh_app(&mut app, &mut jj, &repo_path);
                }
                Action::UpdateRevset(revset_str) => {
                    update_revset(&mut app, &jj, revset_str);
                }
                Action::EditRevsetInEditor => {
                    edit_revset_in_editor(&mut app, &jj, &mut terminal);
                }
                Action::None => {}
            }
        }
    }

    kojutsu::terminal::restore()?;
    Ok(())
}

fn refresh_app(app: &mut App, jj: &mut JjRepo, repo_path: &std::path::Path) {
    JjRepo::snapshot(repo_path);
    if let Ok(new_jj) = JjRepo::open(repo_path) {
        *jj = new_jj;
        let revset = app.revset.clone();
        app.refresh(jj, &revset);
    }
}

fn suspend_and_run(
    app: &mut App,
    jj: &mut JjRepo,
    repo_path: &std::path::Path,
    terminal: &mut kojutsu::terminal::Term,
    cmd: JJCommand,
) {
    // Leave the alternate screen so the editor can use the terminal.
    let _ = kojutsu::terminal::restore();
    let result = cmd.run_interactive(repo_path);

    // Re-enter the TUI.
    *terminal = kojutsu::terminal::init().expect("failed to re-init terminal");

    if result.success {
        if let Ok(new_jj) = JjRepo::open(repo_path) {
            *jj = new_jj;
            let revset = app.revset.clone();
            app.refresh(jj, &revset);
        }
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

fn update_revset(app: &mut App, jj: &JjRepo, revset_str: String) {
    match app.try_refresh(jj, &revset_str) {
        Ok(()) => {
            app.revset = revset_str;
            app.revset_draft = None;
        }
        Err(err) => {
            app.revset_draft = Some(revset_str);
            app.mode = AppMode::CommandOutput {
                command: "revset error".to_string(),
                output: err.into_bytes(),
                success: false,
            };
        }
    }
}

fn edit_revset_in_editor(app: &mut App, jj: &JjRepo, terminal: &mut kojutsu::terminal::Term) {
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
                        update_revset(app, jj, new_revset);
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
        if c.is_working_copy {
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
        println!(
            "{} ({}){}{} {}",
            c.change_id.display,
            &c.graph_id[..8],
            flags_str,
            bm_str,
            desc
        );
        for edge in &entry.edges {
            println!(
                "  {:?} -> {}",
                edge.kind,
                &edge.target[..8.min(edge.target.len())]
            );
        }
    }
}

fn run_jj_command(app: &mut App, jj: &mut JjRepo, repo_path: &std::path::Path, cmd: JJCommand) {
    let result = cmd.run(repo_path);

    app.mode = AppMode::CommandOutput {
        command: result.display,
        output: result.output,
        success: result.success,
    };

    if result.success {
        if let Ok(new_jj) = JjRepo::open(repo_path) {
            *jj = new_jj;
            let revset = app.revset.clone();
            app.refresh(jj, &revset);
        }
    }
}
