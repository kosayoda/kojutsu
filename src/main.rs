use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use ratatui::crossterm::event::{self, Event};

use jujujutsu::app::{App, AppMode};
use jujujutsu::input::{self, Action};
use jujujutsu::jj_command::JJCommand;
use jujujutsu::keymap::Keymap;
use jujujutsu::repo::JjRepo;
use jujujutsu::ui;

#[derive(Parser)]
#[command(name = "jujujutsu", about = "TUI for Jujutsu version control")]
struct Cli {
    /// Path to the repository (default: current directory)
    #[arg(short = 'R', long = "repository", default_value = ".")]
    repository: PathBuf,

    /// Revset expression to display
    #[arg(
        short = 'r',
        long = "revisions",
        default_value = "present(@) | ancestors((tags() | untracked_remote_bookmarks()).., 2)"
    )]
    revisions: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let repo_path = cli.repository.canonicalize().unwrap_or(cli.repository);
    let mut jj = JjRepo::open(&repo_path)?;
    let entries = jj.evaluate_revset(&cli.revisions)?;
    let repo_root = jj.workspace_root().display().to_string();
    let keymap: &'static Keymap = Box::leak(Box::new(Keymap::default()));

    let mut app = App::new(entries, cli.revisions, repo_root);
    let mut terminal = jujujutsu::terminal::init()?;

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))? {
            let ev = event::read()?;
            let action = match ev {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    input::handle_key(&mut app, &jj, keymap, key)
                }
                Event::Mouse(mouse) => input::handle_mouse(&mut app, &jj, mouse, ui::HEADER_HEIGHT),
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
                Action::None => {}
            }
        }
    }

    jujujutsu::terminal::restore()?;
    Ok(())
}

fn suspend_and_run(
    app: &mut App,
    jj: &mut JjRepo,
    repo_path: &std::path::Path,
    terminal: &mut jujujutsu::terminal::Term,
    cmd: JJCommand,
) {
    // Leave the alternate screen so the editor can use the terminal.
    let _ = jujujutsu::terminal::restore();
    let result = cmd.run_interactive(repo_path);

    // Re-enter the TUI.
    *terminal = jujujutsu::terminal::init().expect("failed to re-init terminal");

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
