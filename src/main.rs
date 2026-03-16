use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use ratatui::crossterm::event::{self, Event};

use jujujutsu::app::App;
use jujujutsu::input::{self, Action};
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
        default_value = "@ | ancestors(@, 10)"
    )]
    revisions: String,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let repo_path = cli.repository.canonicalize().unwrap_or(cli.repository);
    let jj = JjRepo::open(&repo_path)?;
    let entries = jj.evaluate_revset(&cli.revisions)?;
    let repo_root = jj.workspace_root().display().to_string();

    let mut app = App::new(entries, cli.revisions, repo_root);
    let mut terminal = jujujutsu::terminal::init()?;

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))? {
            let ev = event::read()?;
            let action = match ev {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    input::handle_key(&mut app, &jj, key)
                }
                Event::Mouse(mouse) => input::handle_mouse(&mut app, &jj, mouse, ui::HEADER_HEIGHT),
                _ => Action::None,
            };
            if matches!(action, Action::Quit) {
                break;
            }
        }
    }

    jujujutsu::terminal::restore()?;
    Ok(())
}
