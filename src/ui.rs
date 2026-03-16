use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, DisplayRow};
use crate::dag::{CommitInfo, FileChange, FileStatus};

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &App) {
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

fn draw_list(frame: &mut Frame, area: Rect, app: &App) {
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

    // ListState is ephemeral -- we build it from app.cursor each frame.
    let mut list_state = ListState::default();
    list_state.select(Some(app.cursor));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_commit_line<'a>(
    graph_prefix: &str,
    c: &'a CommitInfo,
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
