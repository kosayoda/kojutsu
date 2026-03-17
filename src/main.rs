use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use ratatui::crossterm::event::{self, Event};

use jujujutsu::app::{App, AppMode};
use jujujutsu::input::{self, Action};
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
                Action::RunJj { args, display_cmd } => {
                    run_jj_command(&mut app, &mut jj, &repo_path, args, display_cmd);
                }
                Action::None => {}
            }
        }
    }

    jujujutsu::terminal::restore()?;
    Ok(())
}

fn run_jj_command(
    app: &mut App,
    jj: &mut JjRepo,
    repo_path: &PathBuf,
    args: Vec<String>,
    display_cmd: String,
) {
    let result = std::process::Command::new("jj")
        .args(&args)
        .arg("-R")
        .arg(repo_path)
        .arg("--color=never")
        .output();

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lines: Vec<String> = stdout
                .lines()
                .chain(stderr.lines())
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
            let success = output.status.success();

            app.mode = AppMode::CommandOutput {
                command: display_cmd,
                output: lines,
                success,
            };

            if success {
                // Re-open the repo to see the new state, then refresh.
                if let Ok(new_jj) = JjRepo::open(repo_path) {
                    *jj = new_jj;
                    let revset = app.revset.clone();
                    app.refresh(jj, &revset);
                }
            }
        }
        Err(e) => {
            app.mode = AppMode::CommandOutput {
                command: display_cmd,
                output: vec![format!("failed to run jj: {e}")],
                success: false,
            };
        }
    }
}
