use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use jujujutsu::dag::DagEntry;
use jujujutsu::graph::{self, GraphLines};
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

/// One visual row in the list. A single commit expands into multiple rows:
/// the first has the node glyph + commit info, the rest are graph connectors.
struct DisplayRow {
    /// Index into `entries` that this row belongs to.
    entry_idx: usize,
    /// Which line of the graph output this row corresponds to.
    /// 0 = the node line (has commit info), 1+ = link/pad lines.
    graph_line_idx: usize,
}

struct App {
    entries: Vec<DagEntry>,
    graph: Vec<GraphLines>,
    /// Flattened display rows (one per visual line).
    rows: Vec<DisplayRow>,
    list_state: ListState,
    revset: String,
    repo_root: String,
}

impl App {
    fn new(entries: Vec<DagEntry>, revset: String, repo_root: String) -> Self {
        let graph = graph::render(&entries);

        // Build flattened row list.
        let mut rows = Vec::new();
        for (entry_idx, gl) in graph.iter().enumerate() {
            for graph_line_idx in 0..gl.lines.len() {
                rows.push(DisplayRow {
                    entry_idx,
                    graph_line_idx,
                });
            }
        }

        let mut list_state = ListState::default();
        if !rows.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            entries,
            graph,
            rows,
            list_state,
            revset,
            repo_root,
        }
    }

    /// Move selection to the previous commit node line (skip pad/link lines).
    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            // Find the previous row that is a node line (graph_line_idx == 0).
            for j in (0..i).rev() {
                if self.rows[j].graph_line_idx == 0 {
                    self.list_state.select(Some(j));
                    return;
                }
            }
        }
    }

    /// Move selection to the next commit node line (skip pad/link lines).
    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            for j in (i + 1)..self.rows.len() {
                if self.rows[j].graph_line_idx == 0 {
                    self.list_state.select(Some(j));
                    return;
                }
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
        .rows
        .iter()
        .map(|row| {
            let graph_lines = &app.graph[row.entry_idx];
            let graph_str = graph_lines
                .lines
                .get(row.graph_line_idx)
                .map(|s| s.as_str())
                .unwrap_or("");

            if row.graph_line_idx == 0 {
                // Node line: graph prefix + commit info
                let entry = &app.entries[row.entry_idx];
                render_commit_line(graph_str, &entry.commit)
            } else {
                // Link/pad line: just graph characters
                ListItem::new(Line::from(Span::styled(
                    graph_str.to_string(),
                    Style::default().fg(Color::DarkGray),
                )))
            }
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

fn render_commit_line<'a>(graph_prefix: &str, c: &'a jujujutsu::dag::CommitInfo) -> ListItem<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();

    // Graph column
    let graph_color = if c.is_working_copy {
        Color::Green
    } else if c.has_conflict {
        Color::Red
    } else {
        Color::Cyan
    };
    spans.push(Span::styled(
        format!("{graph_prefix} "),
        Style::default().fg(graph_color),
    ));

    // Change ID
    spans.push(Span::styled(
        c.change_id.as_str(),
        Style::default().fg(Color::Magenta),
    ));
    spans.push(Span::raw(" "));

    // Commit ID
    spans.push(Span::styled(
        c.commit_id.as_str(),
        Style::default().fg(Color::Blue),
    ));
    spans.push(Span::raw(" "));

    // Author email
    spans.push(Span::styled(
        c.author.email.as_str(),
        Style::default().fg(Color::Yellow),
    ));
    spans.push(Span::raw(" "));

    // Timestamp
    let formatted = c.author.timestamp.strftime("%Y-%m-%d %H:%M:%S").to_string();
    spans.push(Span::styled(
        formatted,
        Style::default().fg(Color::DarkGray),
    ));

    // Bookmarks
    for bm in &c.bookmarks {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            bm.as_str(),
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
        spans.push(Span::styled(desc.as_str(), style));
    } else {
        spans.push(Span::styled(
            " (empty)",
            Style::default().fg(Color::DarkGray),
        ));
    }

    ListItem::new(Line::from(spans))
}
