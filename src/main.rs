use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use color_eyre::Result;
use jujujutsu::dag::{DagEntry, FileChange, FileStatus};
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

/// One visual row in the list.
enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: usize },
    /// A graph link/pad line between commits.
    GraphLink { entry_idx: usize, line_idx: usize },
    /// A file change line (shown when commit is unfolded).
    FileChange { entry_idx: usize, file_idx: usize },
}

struct App {
    entries: Vec<DagEntry>,
    graph: Vec<GraphLines>,
    jj: JjRepo,
    /// Flattened display rows (one per visual line).
    rows: Vec<DisplayRow>,
    list_state: ListState,
    revset: String,
    repo_root: String,
    /// Per-commit fold state: true = unfolded (showing files).
    unfolded: Vec<bool>,
    /// Lazily loaded file changes, keyed by entry index.
    file_cache: HashMap<usize, Vec<FileChange>>,
}

impl App {
    fn new(entries: Vec<DagEntry>, jj: JjRepo, revset: String, repo_root: String) -> Self {
        let graph = graph::render(&entries);
        let unfolded = vec![false; entries.len()];
        let file_cache = HashMap::new();

        let mut app = Self {
            entries,
            graph,
            jj,
            rows: Vec::new(),
            list_state: ListState::default(),
            revset,
            repo_root,
            unfolded,
            file_cache,
        };
        app.rebuild_rows();
        app
    }

    /// Rebuild the flattened row list from current fold state.
    fn rebuild_rows(&mut self) {
        let selected_entry = self
            .list_state
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| match r {
                DisplayRow::CommitNode { entry_idx }
                | DisplayRow::GraphLink { entry_idx, .. }
                | DisplayRow::FileChange { entry_idx, .. } => *entry_idx,
            });

        self.rows.clear();
        for (entry_idx, gl) in self.graph.iter().enumerate() {
            // Node line (always present).
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            // File changes (only if unfolded and loaded).
            if self.unfolded[entry_idx] {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    for file_idx in 0..files.len() {
                        self.rows.push(DisplayRow::FileChange {
                            entry_idx,
                            file_idx,
                        });
                    }
                }
            }

            // Link/pad lines (graph connectors, skip the first which is the node line).
            for line_idx in 1..gl.lines.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                });
            }
        }

        // Restore selection to the same commit if possible.
        let new_selection = selected_entry
            .and_then(|target| {
                self.rows.iter().position(
                    |r| matches!(r, DisplayRow::CommitNode { entry_idx } if *entry_idx == target),
                )
            })
            .or(if self.rows.is_empty() { None } else { Some(0) });
        self.list_state.select(new_selection);
    }

    /// Move selection to the previous commit node line.
    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            for j in (0..i).rev() {
                if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                    self.list_state.select(Some(j));
                    return;
                }
            }
        }
    }

    /// Move selection to the next commit node line.
    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            for j in (i + 1)..self.rows.len() {
                if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                    self.list_state.select(Some(j));
                    return;
                }
            }
        }
    }

    /// Toggle fold on the currently selected commit.
    fn toggle_fold(&mut self) {
        let entry_idx = match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(DisplayRow::CommitNode { entry_idx }) => *entry_idx,
            Some(DisplayRow::FileChange { entry_idx, .. }) => *entry_idx,
            _ => return,
        };

        if self.unfolded[entry_idx] {
            self.unfolded[entry_idx] = false;
        } else {
            // Lazy load file changes if not cached.
            if !self.file_cache.contains_key(&entry_idx) {
                let graph_id = &self.entries[entry_idx].commit.graph_id;
                match self.jj.file_changes(graph_id) {
                    Ok(files) => {
                        self.file_cache.insert(entry_idx, files);
                    }
                    Err(_) => {
                        self.file_cache.insert(entry_idx, Vec::new());
                    }
                }
            }
            self.unfolded[entry_idx] = true;
        }

        self.rebuild_rows();
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let repo_path = cli.repository.canonicalize().unwrap_or(cli.repository);
    let jj = JjRepo::open(&repo_path)?;
    let entries = jj.evaluate_revset(&cli.revisions)?;
    let repo_root = jj.workspace_root().display().to_string();

    let mut app = App::new(entries, jj, cli.revisions, repo_root);
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
                    KeyCode::Tab => app.toggle_fold(),
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
        .map(|row| match row {
            DisplayRow::CommitNode { entry_idx } => {
                let entry = &app.entries[*entry_idx];
                let graph_str = app.graph[*entry_idx]
                    .lines
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let is_unfolded = app.unfolded[*entry_idx];
                render_commit_line(graph_str, &entry.commit, is_unfolded)
            }
            DisplayRow::GraphLink {
                entry_idx,
                line_idx,
            } => {
                let graph_str = app.graph[*entry_idx]
                    .lines
                    .get(*line_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                ListItem::new(Line::from(Span::styled(
                    graph_str.to_string(),
                    Style::default().fg(Color::DarkGray),
                )))
            }
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => {
                let files = &app.file_cache[entry_idx];
                let file = &files[*file_idx];
                render_file_line(file)
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

fn render_commit_line<'a>(
    graph_prefix: &str,
    c: &'a jujujutsu::dag::CommitInfo,
    is_unfolded: bool,
) -> ListItem<'a> {
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

    // Fold indicator
    let fold_char = if is_unfolded { "▾ " } else { "▸ " };
    spans.push(Span::styled(
        fold_char,
        Style::default().fg(Color::DarkGray),
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

fn render_file_line(file: &FileChange) -> ListItem<'_> {
    let (marker, color) = match file.status {
        FileStatus::Added => ("A", Color::Green),
        FileStatus::Modified => ("M", Color::Cyan),
        FileStatus::Deleted => ("D", Color::Red),
    };

    ListItem::new(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            marker,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(file.path.as_str(), Style::default().fg(Color::White)),
    ]))
}
