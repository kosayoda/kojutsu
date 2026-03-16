use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, DisplayRow};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, ShortId};

/// The Y offset where the list starts (for mouse click translation).
pub const HEADER_HEIGHT: u16 = 2;

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(HEADER_HEIGHT), Constraint::Fill(1)])
            .areas(frame.area());

    draw_header(frame, header_area, app);
    draw_list(frame, list_area, app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let header = vec![
        Line::from(vec![
            Span::styled("repository: ", Style::default().fg(Color::DarkGray)),
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
                let gl = &app.graph[*entry_idx];
                let graph_node = gl.lines.first().map(|s| s.as_str()).unwrap_or("");
                let graph_cont = gl.lines.get(1).map(|s| s.as_str()).unwrap_or("│");
                let is_unfolded = app.unfolded[*entry_idx];
                render_commit_item(graph_node, graph_cont, &entry.commit, is_unfolded)
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
                let is_unfolded = app
                    .file_unfolded
                    .get(&(*entry_idx, *file_idx))
                    .copied()
                    .unwrap_or(false);
                render_file_line(file, is_unfolded)
            }
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => {
                let diff_lines = &app.diff_cache[&(*entry_idx, *file_idx)];
                let diff_line = &diff_lines[*line_idx];
                render_diff_line(diff_line)
            }
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(Style::default().bg(Color::Rgb(50, 50, 60)).add_modifier(Modifier::BOLD));

    // ListState is ephemeral -- we build it from app.cursor each frame.
    let mut list_state = ListState::default();
    list_state.select(Some(app.cursor));
    frame.render_stateful_widget(list, area, &mut list_state);

    // Save scroll offset for mouse click translation.
    app.last_scroll_offset = list_state.offset();
}

/// Render a commit as a 2-line ListItem matching `jj log` default format:
///
/// ```text
/// ○  change_id author timestamp bookmarks commit_id
/// │  description
/// ```
fn render_commit_item<'a>(
    graph_node: &str,
    graph_cont: &str,
    c: &'a CommitInfo,
    is_unfolded: bool,
) -> ListItem<'a> {
    let graph_color = if c.is_working_copy {
        Color::Green
    } else if c.has_conflict {
        Color::Red
    } else {
        Color::Cyan
    };
    let graph_style = Style::default().fg(graph_color);

    // --- Line 1: graph  change_id author timestamp bookmarks commit_id ---
    let mut line1: Vec<Span<'a>> = Vec::new();

    // Graph glyph
    line1.push(Span::styled(format!("{graph_node}  "), graph_style));

    // Fold indicator
    let fold_char = if is_unfolded { "▾ " } else { "▸ " };
    line1.push(Span::styled(
        fold_char,
        Style::default().fg(Color::DarkGray),
    ));

    // Change ID (prefix bright, rest dimmed)
    push_short_id(&mut line1, &c.change_id, Color::Magenta);
    line1.push(Span::raw(" "));

    // Author
    line1.push(Span::styled(
        c.author.email.as_str(),
        Style::default().fg(Color::Yellow),
    ));
    line1.push(Span::raw(" "));

    // Timestamp
    let formatted = c.author.timestamp.strftime("%Y-%m-%d %H:%M:%S").to_string();
    line1.push(Span::styled(
        formatted,
        Style::default().fg(Color::DarkGray),
    ));

    // Bookmarks
    for bm in &c.bookmarks {
        line1.push(Span::raw(" "));
        line1.push(Span::styled(
            bm.as_str(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Commit ID (at end, like jj log -- prefix bright, rest dimmed)
    line1.push(Span::raw(" "));
    push_short_id(&mut line1, &c.commit_id, Color::Blue);

    // --- Line 2: graph_cont  [empty] description ---
    let mut line2: Vec<Span<'a>> = Vec::new();

    // Graph continuation
    line2.push(Span::styled(
        format!("{graph_cont}  "),
        Style::default().fg(Color::DarkGray),
    ));

    if let Some(desc) = &c.description {
        if c.is_empty {
            line2.push(Span::styled(
                "(empty) ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        let desc_style = if c.is_empty {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        line2.push(Span::styled(desc.as_str(), desc_style));
    } else {
        let placeholder = if c.is_empty {
            "(empty)"
        } else {
            "(no description set)"
        };
        line2.push(Span::styled(
            placeholder,
            Style::default().fg(Color::DarkGray),
        ));
    }

    ListItem::new(vec![Line::from(line1), Line::from(line2)])
}

fn render_file_line(file: &FileChange, is_unfolded: bool) -> ListItem<'_> {
    let (marker, color) = match file.status {
        FileStatus::Added => ("A", Color::Green),
        FileStatus::Modified => ("M", Color::Cyan),
        FileStatus::Deleted => ("D", Color::Red),
    };

    let fold_char = if is_unfolded { "▾" } else { "▸" };

    ListItem::new(Line::from(vec![
        Span::raw("    "),
        Span::styled(fold_char, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(
            marker,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(file.path.as_str(), Style::default().fg(Color::White)),
    ]))
}

fn render_diff_line(diff_line: &DiffLine) -> ListItem<'_> {
    let (prefix, style) = match diff_line.kind {
        DiffLineKind::Header => ("      ", Style::default().fg(Color::Magenta)),
        DiffLineKind::Context => ("       ", Style::default().fg(Color::DarkGray)),
        DiffLineKind::Added => ("      +", Style::default().fg(Color::Green)),
        DiffLineKind::Removed => ("      -", Style::default().fg(Color::Red)),
    };

    ListItem::new(Line::from(vec![
        Span::styled(prefix, style),
        Span::styled(diff_line.content.as_str(), style),
    ]))
}

/// Push a `ShortId` as two spans: bright prefix + dimmed suffix.
fn push_short_id<'a>(spans: &mut Vec<Span<'a>>, id: &'a ShortId, color: Color) {
    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];

    spans.push(Span::styled(
        prefix,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
    }
}
