use itertools::Itertools;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, AppMode, DisplayRow};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, ShortId};
use crate::keymap::{self, CommandFlags, HelpEntry, HelpGroup, Keymap, KeymapNode};

/// The Y offset where the list starts (for mouse click translation).
/// Minimum separator between repo and revset when on a single line.
const HEADER_SEP: &str = "  ";

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &mut App, keymap: &'static Keymap) {
    // Use a single-line header if both repo and revset fit on one line.
    let single_line_len = "repository: ".len()
        + app.repo_root.len()
        + HEADER_SEP.len()
        + "revset: ".len()
        + app.revset.len();
    let header_height = if single_line_len <= frame.area().width as usize {
        1
    } else {
        2
    };

    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(header_height), Constraint::Fill(1)])
            .areas(frame.area());

    app.last_header_height = header_height;
    draw_header(frame, header_area, app);
    draw_list(frame, list_area, app);

    // Overlays render on top of the list area (bottom-aligned).
    match &app.mode {
        AppMode::Normal => {}
        AppMode::Submenu {
            label,
            children,
            flags,
        } => {
            let has_toggles = children
                .iter()
                .any(|(_, n)| matches!(n, KeymapNode::Toggle { .. }));
            // +1 for top border.
            let height = if has_toggles { 3 } else { 2 };
            let overlay = overlay_area(list_area, height);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_submenu(frame, overlay, label, children, *flags);
        }
        AppMode::CommandOutput {
            command,
            output,
            success,
        } => {
            let output_lines = output.iter().filter(|&&b| b == b'\n').count().max(1);
            let height = (output_lines as u16 + 3).min(list_area.height / 2).max(3);
            let overlay = overlay_area(list_area, height);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_command_output(frame, overlay, command, output, *success);
        }
        AppMode::Help => {
            let groups = keymap::help_entries(keymap);
            // We need to balance first to compute the correct height.
            let (left, right) = balance_help_groups(&groups);
            let left_h: usize = left.iter().map(|(_, e)| e.len() + 1).sum();
            let right_h: usize = right
                .iter()
                .enumerate()
                .map(|(i, (_, e))| e.len() + 1 + if i > 0 { 1 } else { 0 }) // blank between groups
                .sum();
            let max_col = left_h.max(right_h);
            // +2 for border + breathing room.
            let height = (max_col as u16 + 2).min(list_area.height * 7 / 10).max(4);
            let overlay = overlay_area(list_area, height);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_help(frame, overlay, &left, &right);
        }
        AppMode::TextInput { prompt, input, .. } => {
            let overlay = overlay_area(list_area, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_text_input(frame, overlay, prompt, input);
        }
        AppMode::TargetSelect { prompt, source, .. } => {
            let overlay = overlay_area(list_area, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_target_select(frame, overlay, prompt, source);
        }
        AppMode::FollowUp { prompt, options } => {
            let overlay = overlay_area(list_area, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_follow_up(frame, overlay, prompt, options);
        }
        AppMode::SelectFromList {
            title,
            items,
            selected,
            ..
        } => {
            // +2 for top border + bottom padding.
            let height = (items.len() as u16 + 2).min(list_area.height / 2).max(3);
            let overlay = overlay_area(list_area, height);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_select_list(frame, overlay, title, items, *selected);
        }
    }
}

/// Compute an overlay area at the bottom of `area` with the given height.
fn overlay_area(area: Rect, height: u16) -> Rect {
    let h = height.min(area.height);
    Rect {
        x: area.x,
        y: area.y + area.height - h,
        width: area.width,
        height: h,
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let header = if area.height == 1 {
        // Single-line: "repository: <path>  revset: <revset>"
        vec![Line::from(vec![
            Span::styled("repository: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.repo_root, Style::default().fg(Color::White)),
            Span::raw(HEADER_SEP),
            Span::styled("revset: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&app.revset, Style::default().fg(Color::Cyan)),
        ])]
    } else {
        vec![
            Line::from(vec![
                Span::styled("repository: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&app.repo_root, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("revset: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&app.revset, Style::default().fg(Color::Cyan)),
            ]),
        ]
    };
    frame.render_widget(Paragraph::new(header), area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    // If in target selection mode, get the source change_id for highlighting.
    let target_select_source: Option<&str> = match &app.mode {
        AppMode::TargetSelect { source, .. } => Some(source.as_str()),
        _ => None,
    };

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| match row {
            DisplayRow::CommitNode { entry_idx } => {
                let entry = &app.entries[*entry_idx];
                let gl = &app.graph[*entry_idx];
                let graph_node = gl.node.as_str();
                let graph_cont = gl.cont.as_str();
                let is_source = target_select_source.is_some_and(|src| {
                    let id = &entry.commit.change_id;
                    id.display.starts_with(src)
                        || src.starts_with(&id.display[..id.prefix_len.min(id.display.len())])
                });
                render_commit_item(graph_node, graph_cont, &entry.commit, is_source)
            }
            DisplayRow::GraphLink {
                entry_idx,
                line_idx,
            } => {
                let graph_str = app.graph[*entry_idx]
                    .extra
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
        .scroll_padding(5)
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(50, 50, 60))
                .add_modifier(Modifier::BOLD),
        );

    // ListState is ephemeral -- we build it from app.cursor each frame.
    let mut list_state = ListState::default();
    list_state.select(Some(app.cursor));
    frame.render_stateful_widget(list, area, &mut list_state);

    // Save scroll offset for mouse click translation.
    app.last_scroll_offset = list_state.offset();
}

type HelpColumn<'a> = Vec<&'a (HelpGroup, Vec<HelpEntry>)>;

/// Balance help groups into two columns, keeping groups intact.
fn balance_help_groups(groups: &[(HelpGroup, Vec<HelpEntry>)]) -> (HelpColumn<'_>, HelpColumn<'_>) {
    let group_rows: Vec<usize> = groups.iter().map(|(_, e)| e.len() + 1).collect();
    let total: usize = group_rows.iter().sum();
    let half = total / 2;

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut left_count = 0usize;

    for (i, group) in groups.iter().enumerate() {
        if i == 0 || left_count + group_rows[i] <= half {
            left.push(group);
            left_count += group_rows[i];
        } else {
            right.push(group);
        }
    }

    (left, right)
}

fn draw_help(
    frame: &mut Frame,
    area: Rect,
    left_groups: &[&(HelpGroup, Vec<HelpEntry>)],
    right_groups: &[&(HelpGroup, Vec<HelpEntry>)],
) {
    use ratatui::widgets::Padding;

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" ? Help ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .padding(Padding::new(1, 1, 0, 0));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);

    render_help_column(frame, left_area, left_groups);
    render_help_column(frame, right_area, right_groups);
}

fn render_help_column(frame: &mut Frame, area: Rect, groups: &[&(HelpGroup, Vec<HelpEntry>)]) {
    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::White);

    let mut rows: Vec<Row> = Vec::new();

    for (i, (group, entries)) in groups.iter().enumerate() {
        if i > 0 {
            rows.push(Row::new(vec![Cell::from(""), Cell::from("")]));
        }
        rows.push(Row::new(vec![
            Cell::from(group.label()).style(header_style),
            Cell::from(""),
        ]));
        let desc_width = area.width.saturating_sub(20) as usize;
        for entry in entries.iter() {
            let desc = if entry.description.len() > desc_width && desc_width > 1 {
                format!("{}…", &entry.description[..desc_width - 1])
            } else {
                entry.description.clone()
            };
            rows.push(Row::new(vec![
                Cell::from(format!("  {}", entry.keys)).style(key_style),
                Cell::from(desc).style(desc_style),
            ]));
        }
    }

    let widths = [Constraint::Length(20), Constraint::Fill(1)];
    let table = Table::new(rows, widths);
    frame.render_widget(table, area);
}

fn draw_submenu(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    children: &[(keymap_parser::Node, KeymapNode)],
    flags: CommandFlags,
) {
    let mut lines = Vec::new();

    // Line 1: toggles (if any).
    let mut toggle_spans: Vec<Span> = Vec::new();
    for (key_node, child) in children.iter() {
        if let KeymapNode::Toggle { flag, description } = child {
            if !toggle_spans.is_empty() {
                toggle_spans.push(Span::raw("  "));
            }
            let active = flags.contains(*flag);
            let key_str = keymap::display_key(key_node);
            let style = if active {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            toggle_spans.push(Span::styled(format!("[{key_str}]"), style));
            toggle_spans.push(Span::styled(format!(" {description}"), style));
        }
    }
    if !toggle_spans.is_empty() {
        let mut line = vec![Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )];
        line.extend(toggle_spans);
        lines.push(Line::from(line));
    }

    // Line 2 (or line 1 if no toggles): actions.
    let mut action_spans: Vec<Span> = Vec::new();
    // If toggles took line 1, indent actions to align; otherwise show the label here.
    if lines.is_empty() {
        action_spans.push(Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        // Indent to align with the label on the toggle line.
        let pad = " ".repeat(label.len() + 2);
        action_spans.push(Span::raw(pad));
    }
    for (key_node, child) in children.iter() {
        let desc = match child {
            KeymapNode::Action { description, .. } => *description,
            KeymapNode::Prefix { label, .. } => *label,
            KeymapNode::Toggle { .. } => continue,
        };
        if action_spans.len() > 1 {
            action_spans.push(Span::raw("  "));
        }
        let key_str = keymap::display_key(key_node);
        action_spans.push(Span::styled(
            format!("({key_str})"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        action_spans.push(Span::styled(
            format!(" {desc}"),
            Style::default().fg(Color::White),
        ));
    }
    lines.push(Line::from(action_spans));

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_text_input(frame: &mut Frame, area: Rect, prompt: &str, input: &tui_input::Input) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_width = prompt.len() as u16;
    let input_width = inner.width.saturating_sub(prompt_width);

    let scroll = input.visual_scroll(input_width.saturating_sub(1) as usize);

    let spans = vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(&input.value()[scroll..], Style::default().fg(Color::White)),
    ];

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);

    // Place the cursor.
    let cursor_x = inner.x + prompt_width + (input.visual_cursor() - scroll) as u16;
    let cursor_y = inner.y;
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn draw_target_select(frame: &mut Frame, area: Rect, prompt: &str, source: &str) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let spans = vec![
        Span::styled(
            format!("{prompt} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("from {source}"),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " → select target (Enter = confirm, Esc = cancel)",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_follow_up(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    options: &[crate::app::FollowUpOption],
) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![Span::styled(
        format!("{prompt} "),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];

    for (i, opt) in options.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("({})", opt.key),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", opt.label),
            Style::default().fg(Color::White),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_select_list(frame: &mut Frame, area: Rect, title: &str, items: &[String], selected: usize) {
    use ratatui::widgets::Padding;

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(" {title} "))
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .padding(Padding::new(1, 1, 0, 0));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let marker = if i == selected { "▸ " } else { "  " };
            let style = if i == selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(format!("{marker}{item}"), style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_command_output(
    frame: &mut Frame,
    area: Rect,
    command: &str,
    output: &[u8],
    _success: bool,
) {
    use ansi_to_tui::IntoText;
    use ratatui::widgets::Padding;

    let mut lines = vec![Line::from(Span::styled(
        command,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];

    // Convert ANSI-colored output to ratatui styled text.
    if let Ok(styled) = output.into_text() {
        lines.extend(styled.lines);
    } else {
        // Fallback: render as plain text.
        let text = String::from_utf8_lossy(output);
        for line in text.lines() {
            lines.push(Line::from(Span::raw(line.to_string())));
        }
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .padding(Padding::new(1, 1, 0, 0));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Render a commit as a 2-line ListItem matching `jj log` default format:
///
/// ```text
/// ○  change_id author timestamp bookmarks commit_id
/// │  description
/// ```
fn render_commit_item<'a>(
    graph_node: &'a str,
    graph_cont: &str,
    c: &'a CommitInfo,
    is_source: bool,
) -> ListItem<'a> {
    let graph_color = if is_source {
        Color::Yellow
    } else {
        match c.glyph() {
            crate::dag::Glyph::WorkingCopy => Color::Green,
            crate::dag::Glyph::Conflict => Color::Red,
            crate::dag::Glyph::Immutable => Color::Cyan,
            crate::dag::Glyph::Normal => Color::Cyan,
        }
    };

    let graph_style = Style::default().fg(graph_color);

    // --- Line 1: graph  change_id author timestamp bookmarks commit_id ---
    let mut line1: Vec<Span<'a>> = Vec::new();

    // Source marker for target selection mode.
    if is_source {
        line1.push(Span::styled("► ", Style::default().fg(Color::Yellow)));
    }

    // Graph prefix (properly padded by the renderer).
    // Split into glyph characters vs connector characters for coloring.
    for (is_glyph, group) in graph_node
        .char_indices()
        .chunk_by(|&(_, c)| crate::dag::Glyph::try_from(c).is_ok())
        .into_iter()
    {
        let mut iter = group.into_iter();
        let (start, c) = iter.next().unwrap();
        let end = {
            let (end, c) = iter.last().unwrap_or((start, c));
            end + c.len_utf8()
        };
        let span = &graph_node[start..end];
        if is_glyph {
            line1.push(Span::styled(span, graph_style));
        } else {
            line1.push(Span::styled(span, Style::default().fg(Color::DarkGray)));
        }
    }

    // Change ID (prefix bright, rest dimmed; red if divergent)
    let change_color = if c.is_divergent {
        Color::Red
    } else {
        Color::Magenta
    };
    push_short_id(&mut line1, &c.change_id, change_color);
    if c.is_divergent {
        line1.push(Span::styled(
            "??",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
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

    // Bookmarks (with * suffix if dirty)
    for bm in &c.bookmarks {
        line1.push(Span::raw(" "));
        let display = if bm.is_dirty {
            format!("{}*", bm.name)
        } else {
            bm.name.clone()
        };
        line1.push(Span::styled(
            display,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Commit ID (at end, like jj log -- prefix bright, rest dimmed)
    line1.push(Span::raw(" "));
    push_short_id(&mut line1, &c.commit_id, Color::Blue);

    // --- Line 2: graph_cont  description ---
    let mut line2: Vec<Span<'a>> = Vec::new();

    // Graph continuation prefix (properly padded by the renderer).
    line2.push(Span::styled(
        graph_cont.to_string(),
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
