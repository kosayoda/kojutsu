use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use jujujutsu::dag::DagEntry;
use jujujutsu::repo::JjRepo;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

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

struct App {
    entries: Vec<DagEntry>,
    list_state: ListState,
    revset: String,
    repo_root: String,
}

impl App {
    fn new(entries: Vec<DagEntry>, revset: String, repo_root: String) -> Self {
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            entries,
            list_state,
            revset,
            repo_root,
        }
    }

    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i > 0 {
                self.list_state.select(Some(i - 1));
            }
        }
    }

    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i + 1 < self.entries.len() {
                self.list_state.select(Some(i + 1));
            }
        }
    }
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
        terminal.draw(|frame| draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                    KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                    _ => {}
                }
            }
        }
    }

    jujujutsu::terminal::restore()?;
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(frame.area());

    draw_header(frame, header_area, app);
    draw_list(frame, list_area, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let header = vec![
        Line::from(vec![
            Span::styled("repo: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.repo_root, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("revset: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.revset, Style::default().fg(Color::Cyan)),
        ]),
    ];
    frame.render_widget(Paragraph::new(header), area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let c = &entry.commit;

            let mut spans = Vec::new();

            // Glyph
            let glyph = if c.is_working_copy { "@" } else { "○" };
            let glyph_color = if c.is_working_copy {
                Color::Green
            } else {
                Color::Cyan
            };
            spans.push(Span::styled(
                format!("{glyph} "),
                Style::default().fg(glyph_color),
            ));

            // Change ID
            spans.push(Span::styled(
                &c.change_id,
                Style::default().fg(Color::Magenta),
            ));
            spans.push(Span::raw(" "));

            // Commit ID
            spans.push(Span::styled(&c.commit_id, Style::default().fg(Color::Blue)));
            spans.push(Span::raw(" "));

            // Author email
            spans.push(Span::styled(
                &c.author.email,
                Style::default().fg(Color::Yellow),
            ));
            spans.push(Span::raw(" "));

            // Timestamp
            let ts = c.author.timestamp;
            let formatted = ts.strftime("%Y-%m-%d %H:%M:%S").to_string();
            spans.push(Span::styled(
                formatted,
                Style::default().fg(Color::DarkGray),
            ));

            // Bookmarks
            for bm in &c.bookmarks {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    bm,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            // Description
            if let Some(desc) = &c.description {
                spans.push(Span::raw(" "));
                let style = if c.is_empty {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::White)
                };
                spans.push(Span::styled(desc, style));
            } else {
                spans.push(Span::styled(
                    " (empty)",
                    Style::default().fg(Color::DarkGray),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut app.list_state);
}
