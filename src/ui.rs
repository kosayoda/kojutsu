use itertools::Itertools;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, AppMode, GLOBAL_TOGGLES};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, ShortId};
use crate::keymap::{self, CommandFlags, HelpEntry, HelpGroup, Keymap, KeymapNode};
use crate::types::{
    DisplayRow, FileSelectionState, FollowUpOption, SearchFocus, SearchScopes, SEARCH_SCOPE_SPECS,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchRowState {
    None,
    Match,
    Current,
}

#[derive(Clone, Copy)]
struct SearchRender<'a> {
    query: &'a str,
    scopes: SearchScopes,
    case_sensitive: bool,
    row_state: SearchRowState,
}

fn search_row_state(app: &App, row_idx: usize) -> SearchRowState {
    let Some(search) = &app.search else {
        return SearchRowState::None;
    };
    if !app.is_match(row_idx) {
        return SearchRowState::None;
    }
    if let Some(current) = search.current_match {
        if search.matches.get(current).copied() == Some(row_idx) {
            return SearchRowState::Current;
        }
    }
    SearchRowState::Match
}

fn search_gutter<'a>(state: SearchRowState) -> Span<'a> {
    match state {
        SearchRowState::None => Span::raw("  "),
        SearchRowState::Match => Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        SearchRowState::Current => Span::styled(
            "┃ ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

fn contains_query(haystack: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(query)
    } else {
        haystack.to_lowercase().contains(&query.to_lowercase())
    }
}

fn push_highlighted<'a>(
    out: &mut Vec<Span<'a>>,
    text: &str,
    query: &str,
    base: Style,
    case_sensitive: bool,
) {
    if query.is_empty() {
        out.push(Span::styled(text.to_string(), base));
        return;
    }

    let (hay, needle) = if case_sensitive {
        (text.to_string(), query.to_string())
    } else {
        (text.to_lowercase(), query.to_lowercase())
    };
    if let Some(start) = hay.find(&needle) {
        let end = start + needle.len();
        if start > 0 {
            out.push(Span::styled(text[..start].to_string(), base));
        }
        out.push(Span::styled(
            text[start..end].to_string(),
            base.add_modifier(Modifier::REVERSED),
        ));
        if end < text.len() {
            out.push(Span::styled(text[end..].to_string(), base));
        }
    } else {
        out.push(Span::styled(text.to_string(), base));
    }
}

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

    let [header_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    app.last_header_height = header_height;
    draw_header(frame, header_area, app);
    draw_list(frame, main_area, app);
    draw_status_bar(frame, status_area, app);

    // Overlays render on top of the main + status area (bottom-aligned).
    // This means overlays cover the status bar too.
    let overlay_base = Rect {
        x: main_area.x,
        y: main_area.y,
        width: main_area.width,
        height: main_area.height + status_area.height,
    };

    match &app.mode {
        AppMode::Normal => {}
        AppMode::Submenu {
            key,
            label,
            children,
            flags,
        } => {
            // 1 line for top border (with title + toggles) + 1 line for actions.
            let overlay = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_submenu(
                frame,
                overlay,
                key,
                label,
                children,
                *flags,
                app.selection_summary().submenu_suffix(),
            );
        }
        AppMode::CommandOutput {
            command,
            output,
            success,
        } => {
            let output_lines = output.iter().filter(|&&b| b == b'\n').count().max(1);
            let height = (output_lines as u16 + 3)
                .min(overlay_base.height / 2)
                .max(3);
            let overlay = overlay_area(overlay_base, height);
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
            let height = (max_col as u16 + 2)
                .min(overlay_base.height * 7 / 10)
                .max(4);
            let overlay = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_help(frame, overlay, app, &left, &right);
        }
        AppMode::TextInput { prompt, input, .. } => {
            let overlay = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_text_input(frame, overlay, prompt, input);
        }
        AppMode::SearchInput => {
            let overlay = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_search_input(frame, overlay, app);
        }
        AppMode::TargetSelect { prompt, source, .. } => {
            let overlay = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, overlay);
            draw_target_select(frame, overlay, prompt, source.as_str());
        }
        AppMode::FollowUp { prompt, options } => {
            let overlay = overlay_area(overlay_base, 2);
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
            let height = (items.len() as u16 + 2).min(overlay_base.height / 2).max(3);
            let overlay = overlay_area(overlay_base, height);
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

fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    // Build toggle indicators for the title bar.
    let mut toggle_spans: Vec<Span> = Vec::new();
    for toggle in GLOBAL_TOGGLES {
        let active = app.toggles.contains(toggle.flag);
        let style = if active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        toggle_spans.push(Span::styled(
            format!(" [{}] {} ", toggle.hint, toggle.label),
            style,
        ));
    }

    if let Some(text) = app.selection_summary().display_text() {
        toggle_spans.push(Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title("")
        .title(" Status ")
        .title_style(Style::default().fg(Color::Cyan).bold())
        .title_alignment(Alignment::Left)
        .title(Line::from(toggle_spans))
        .title(
            Line::from(Span::styled(" ? Help ", Style::default().fg(Color::White))).right_aligned(),
        )
        .title(Line::from("").right_aligned());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let content = if let Some(search) = &app.search {
        let pos = search.current_match.map(|i| i + 1).unwrap_or(0);
        format!(
            "search: {} ({}/{})",
            search.query(),
            pos,
            search.matches.len()
        )
    } else {
        app.last_command.clone().unwrap_or_default()
    };
    let line = Line::from(Span::styled(content, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(line), inner);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    // If in target selection mode, get the source change_id for highlighting.
    let target_select_source: Option<&str> = match &app.mode {
        AppMode::TargetSelect { source, .. } => Some(source.as_str()),
        _ => None,
    };

    let search_ctx = app.search.as_ref().map(|s| SearchRender {
        query: s.query(),
        scopes: s.scopes,
        case_sensitive: s.query().chars().any(|c| c.is_ascii_uppercase()),
        row_state: SearchRowState::None,
    });

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let row_search = search_ctx.as_ref().map(|ctx| SearchRender {
                row_state: search_row_state(app, row_idx),
                ..*ctx
            });
            match row {
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
                    render_commit_item(
                        graph_node,
                        graph_cont,
                        &entry.commit,
                        is_source,
                        row_search.as_ref(),
                    )
                }
                DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                } => {
                    let graph_str = app.graph[*entry_idx]
                        .extra
                        .get(line_idx.raw())
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let mut spans = vec![search_gutter(
                        row_search
                            .as_ref()
                            .map(|s| s.row_state)
                            .unwrap_or(SearchRowState::None),
                    )];
                    spans.push(Span::styled(
                        graph_str.to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                    ListItem::new(Line::from(spans))
                }
                DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                } => {
                    let files = &app.file_cache[entry_idx];
                    let file = &files[file_idx.raw()];
                    let is_unfolded = app
                        .file_unfolded
                        .get(&(*entry_idx, *file_idx))
                        .copied()
                        .unwrap_or(false);
                    let sel_state = app.file_selection_state(*entry_idx, *file_idx);
                    render_file_line(file, is_unfolded, sel_state, row_search.as_ref())
                }
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                } => {
                    let diff_lines = &app.diff_cache[&(*entry_idx, *file_idx)];
                    let diff_line = &diff_lines[line_idx.raw()];
                    let is_selected = app.is_line_selected(*entry_idx, *file_idx, *line_idx);
                    let in_visual = app.is_in_visual_range(*entry_idx, *file_idx, *line_idx);
                    render_diff_line(
                        diff_line,
                        app.show_line_numbers,
                        is_selected,
                        in_visual,
                        row_search.as_ref(),
                    )
                }
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
    app: &App,
    left_groups: &[&(HelpGroup, Vec<HelpEntry>)],
    right_groups: &[&(HelpGroup, Vec<HelpEntry>)],
) {
    use ratatui::widgets::Padding;

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        // Empty title as left padding for key
        .title("")
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

    render_help_column(frame, left_area, app, left_groups);
    render_help_column(frame, right_area, app, right_groups);
}

fn render_help_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    groups: &[&(HelpGroup, Vec<HelpEntry>)],
) {
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
            let required = app.selection_kind().as_bitset();
            let blocked = app.selection_active() && !entry.selection_support.contains(required);
            let key_style = if blocked {
                key_style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
            } else {
                key_style
            };
            let desc_style = if blocked {
                desc_style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
            } else {
                desc_style
            };
            rows.push(Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::from("  "),
                    Span::styled(format!("{:4}", entry.keys), key_style),
                ])),
                Cell::from(Span::styled(desc, desc_style)),
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
    key: &str,
    label: &str,
    children: &[(keymap_parser::Node, KeymapNode)],
    flags: CommandFlags,
    selection_suffix: Option<String>,
) {
    // Build toggle indicators for the title bar.
    let mut toggle_spans: Vec<Span> = Vec::new();
    for (key_node, child) in children.iter() {
        if let KeymapNode::Toggle { flag, description } = child {
            let active = flags.contains(*flag);
            let key_str = keymap::display_key(key_node);
            let style = if active {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            toggle_spans.push(Span::styled(format!(" [{key_str}] {description} "), style));
        }
    }

    // Capitalize the label for display (e.g., "squash" -> "Squash").
    let display_label = {
        let mut chars = label.chars();
        match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => String::new(),
        }
    };

    // Build title: " s Squash " or " s Squash (3 lines) " etc.
    let title = if let Some(suffix) = selection_suffix {
        format!(" {key} {display_label} ({suffix}) ")
    } else {
        format!(" {key} {display_label} ")
    };

    // Block with title on the border.
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title("")
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(toggle_spans))
        .title_alignment(Alignment::Left);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Content: action hints.
    let mut action_spans: Vec<Span> = Vec::new();
    for (key_node, child) in children.iter() {
        let desc = match child {
            KeymapNode::Action { description, .. } => *description,
            KeymapNode::Prefix { label, .. } => *label,
            KeymapNode::Toggle { .. } => continue,
        };
        if !action_spans.is_empty() {
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
    frame.render_widget(Paragraph::new(Line::from(action_spans)), inner);
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

fn draw_search_input(frame: &mut Frame, area: Rect, app: &App) {
    let Some(search) = &app.search else {
        return;
    };
    let mut scope_spans: Vec<Span> = Vec::new();
    for spec in SEARCH_SCOPE_SPECS.iter() {
        let enabled = search.scopes.contains(spec.flag);
        let mut style = if enabled {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        if matches!(search.focus, SearchFocus::Scopes) {
            style = style.bg(Color::Rgb(50, 50, 60));
        }
        scope_spans.push(Span::styled(
            format!(" [{}] {} ", spec.hint, spec.label),
            style,
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title("")
        .title(" Search ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(scope_spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(Span::styled(
        search.query(),
        Style::default().fg(Color::White),
    ));
    frame.render_widget(Paragraph::new(line), inner);

    if matches!(search.focus, SearchFocus::Query) {
        let cursor_x = inner.x + search.input.visual_cursor() as u16;
        frame.set_cursor_position((cursor_x, inner.y));
    }
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

fn draw_follow_up(frame: &mut Frame, area: Rect, prompt: &str, options: &[FollowUpOption]) {
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
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
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
    let search_state = search.map(|s| s.row_state).unwrap_or(SearchRowState::None);

    // --- Line 1: graph  change_id author timestamp bookmarks commit_id ---
    let mut line1: Vec<Span<'static>> = Vec::new();
    line1.push(search_gutter(search_state));

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
            line1.push(Span::styled(span.to_string(), graph_style));
        } else {
            line1.push(Span::styled(
                span.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // Change ID (prefix bright, rest dimmed; red if divergent)
    let change_color = if c.is_divergent {
        Color::Red
    } else if c.is_hidden {
        Color::White
    } else {
        Color::Magenta
    };
    let change_id_text = if let Some(suffix) = c.change_id_suffix {
        format!("{}/{}", c.change_id.display, suffix)
    } else {
        c.change_id.display.clone()
    };
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(&change_id_text, search.query, search.case_sensitive)
        {
            push_highlighted_short_id(
                &mut line1,
                &c.change_id,
                c.change_id_suffix.map(|s| format!("/{s}")),
                change_color,
                search.query,
                search.case_sensitive,
            );
        } else {
            push_short_id(&mut line1, &c.change_id, change_color);
            if let Some(suffix) = c.change_id_suffix {
                line1.push(Span::styled(
                    format!("/{suffix}"),
                    Style::default().fg(change_color),
                ));
            }
        }
    } else {
        push_short_id(&mut line1, &c.change_id, change_color);
        if let Some(suffix) = c.change_id_suffix {
            line1.push(Span::styled(
                format!("/{suffix}"),
                Style::default().fg(change_color),
            ));
        }
    }
    if c.is_divergent {
        line1.push(Span::styled(
            "??",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    line1.push(Span::raw(" "));

    // Author
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::AUTHOR)
            && contains_query(c.author.email.as_str(), search.query, search.case_sensitive)
        {
            push_highlighted(
                &mut line1,
                c.author.email.as_str(),
                search.query,
                Style::default().fg(Color::Yellow),
                search.case_sensitive,
            );
        } else {
            line1.push(Span::styled(
                c.author.email.clone(),
                Style::default().fg(Color::Yellow),
            ));
        }
    } else {
        line1.push(Span::styled(
            c.author.email.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }
    line1.push(Span::raw(" "));

    // Timestamp
    let formatted = c.author.timestamp.strftime("%Y-%m-%d %H:%M:%S").to_string();
    line1.push(Span::styled(
        formatted,
        Style::default().fg(Color::DarkGray),
    ));

    // Local bookmarks (with * suffix if dirty)
    for bm in &c.bookmarks {
        line1.push(Span::raw(" "));
        let display = if bm.is_dirty {
            format!("{}*", bm.name)
        } else {
            bm.name.clone()
        };
        let style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::BOOKMARK)
                && contains_query(&display, search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line1,
                    &display,
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                line1.push(Span::styled(display, style));
            }
        } else {
            line1.push(Span::styled(display, style));
        }
    }

    // Remote bookmarks (name@remote, shown when no local bookmark covers them)
    for rb in &c.remote_bookmarks {
        line1.push(Span::raw(" "));
        let text = format!("{}@{}", rb.name, rb.remote);
        let style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::BOOKMARK)
                && contains_query(&text, search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line1,
                    &text,
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                line1.push(Span::styled(text, style));
            }
        } else {
            line1.push(Span::styled(text, style));
        }
    }

    // Commit ID (at end, like jj log -- prefix bright, rest dimmed)
    line1.push(Span::raw(" "));
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::COMMIT_ID)
            && contains_query(
                c.commit_id.display.as_str(),
                search.query,
                search.case_sensitive,
            )
        {
            push_highlighted_short_id(
                &mut line1,
                &c.commit_id,
                None,
                Color::Blue,
                search.query,
                search.case_sensitive,
            );
        } else {
            push_short_id(&mut line1, &c.commit_id, Color::Blue);
        }
    } else {
        push_short_id(&mut line1, &c.commit_id, Color::Blue);
    }

    // Hidden indicator
    if c.is_hidden {
        line1.push(Span::styled(" (hidden)", Style::default().fg(Color::White)));
    }

    // --- Line 2: graph_cont  description ---
    let mut line2: Vec<Span<'static>> = Vec::new();
    line2.push(search_gutter(search_state));

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
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::DESCRIPTION)
                && contains_query(desc.as_str(), search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line2,
                    desc.as_str(),
                    search.query,
                    desc_style,
                    search.case_sensitive,
                );
            } else {
                line2.push(Span::styled(desc.clone(), desc_style));
            }
        } else {
            line2.push(Span::styled(desc.clone(), desc_style));
        }
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

fn render_file_line(
    file: &FileChange,
    is_unfolded: bool,
    sel_state: FileSelectionState,
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
    let (marker, color) = match file.status {
        FileStatus::Added => ("A", Color::Green),
        FileStatus::Modified => ("M", Color::Cyan),
        FileStatus::Deleted => ("D", Color::Red),
    };

    let fold_char = if is_unfolded { "▾" } else { "▸" };
    let select_char = match sel_state {
        FileSelectionState::Full => "●",
        FileSelectionState::Partial => "◐",
        FileSelectionState::None => " ",
    };

    let mut spans = vec![search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
    )];
    spans.extend(vec![
        Span::styled(
            format!("  {select_char} "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(fold_char, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(
            marker,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let base = Style::default().fg(Color::White);
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::PATH)
            && contains_query(file.path.as_str(), search.query, search.case_sensitive)
        {
            push_highlighted(
                &mut spans,
                file.path.as_str(),
                search.query,
                base,
                search.case_sensitive,
            );
        } else {
            spans.push(Span::styled(file.path.clone(), base));
        }
    } else {
        spans.push(Span::styled(file.path.clone(), base));
    }
    ListItem::new(Line::from(spans))
}

fn render_diff_line(
    diff_line: &DiffLine,
    show_line_numbers: bool,
    is_selected: bool,
    in_visual: bool,
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
    let (marker, style) = match diff_line.kind {
        DiffLineKind::Header => (" ", Style::default().fg(Color::Magenta)),
        DiffLineKind::Context => (" ", Style::default().fg(Color::DarkGray)),
        DiffLineKind::Added => ("+", Style::default().fg(Color::Green)),
        DiffLineKind::Removed => ("-", Style::default().fg(Color::Red)),
    };

    let line_num_style = Style::default().fg(Color::DarkGray);

    let mut spans = vec![search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
    )];
    // Left margin: visual range bar │ + selection indicator ●.
    let is_selectable =
        diff_line.kind == DiffLineKind::Added || diff_line.kind == DiffLineKind::Removed;
    if is_selectable {
        let bar = if in_visual { "│" } else { " " };
        let dot = if is_selected { "●" } else { " " };
        spans.push(Span::styled(bar, Style::default().fg(Color::Cyan)));
        spans.push(Span::styled(dot, Style::default().fg(Color::Yellow)));
    } else {
        spans.push(Span::raw("  "));
    }
    if show_line_numbers && diff_line.kind != DiffLineKind::Header {
        // "  {old:>4} {new:>4} {marker}{content}"
        let old = diff_line
            .old_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        let new = diff_line
            .new_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        spans.push(Span::styled(format!("  {old} {new} "), line_num_style));
        spans.push(Span::styled(marker, style));
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::LINE)
                && contains_query(
                    diff_line.content.as_str(),
                    search.query,
                    search.case_sensitive,
                )
            {
                push_highlighted(
                    &mut spans,
                    diff_line.content.as_str(),
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                spans.push(Span::styled(diff_line.content.clone(), style));
            }
        } else {
            spans.push(Span::styled(diff_line.content.clone(), style));
        }
    } else {
        // Original layout: fixed indent + marker + content
        let prefix = match diff_line.kind {
            DiffLineKind::Header => "      ",
            DiffLineKind::Context => "       ",
            DiffLineKind::Added => "      +",
            DiffLineKind::Removed => "      -",
        };
        spans.push(Span::styled(prefix, style));
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::LINE)
                && contains_query(
                    diff_line.content.as_str(),
                    search.query,
                    search.case_sensitive,
                )
            {
                push_highlighted(
                    &mut spans,
                    diff_line.content.as_str(),
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                spans.push(Span::styled(diff_line.content.clone(), style));
            }
        } else {
            spans.push(Span::styled(diff_line.content.clone(), style));
        }
    }

    ListItem::new(Line::from(spans))
}

/// Push a `ShortId` as two spans: bright prefix + dimmed suffix.
fn push_short_id(spans: &mut Vec<Span<'static>>, id: &ShortId, color: Color) {
    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];

    spans.push(Span::styled(
        prefix.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(
            suffix.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
}

fn push_highlighted_short_id(
    spans: &mut Vec<Span<'static>>,
    id: &ShortId,
    extra_suffix: Option<String>,
    color: Color,
    query: &str,
    case_sensitive: bool,
) {
    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];
    let extra = extra_suffix.unwrap_or_default();
    let text = format!("{prefix}{suffix}{extra}");

    let (hay, needle) = if case_sensitive {
        (text.clone(), query.to_string())
    } else {
        (text.to_lowercase(), query.to_lowercase())
    };
    let match_range = hay.find(&needle).map(|start| start..start + needle.len());

    let mut push_part = |part: &str, style: Style, global_start: usize| {
        if part.is_empty() {
            return;
        }
        if let Some(range) = &match_range {
            let part_start = global_start;
            let part_end = global_start + part.len();
            let overlap_start = range.start.max(part_start);
            let overlap_end = range.end.min(part_end);
            if overlap_start < overlap_end {
                let local_start = overlap_start - part_start;
                let local_end = overlap_end - part_start;
                if local_start > 0 {
                    spans.push(Span::styled(part[..local_start].to_string(), style));
                }
                spans.push(Span::styled(
                    part[local_start..local_end].to_string(),
                    style.add_modifier(Modifier::REVERSED),
                ));
                if local_end < part.len() {
                    spans.push(Span::styled(part[local_end..].to_string(), style));
                }
                return;
            }
        }
        spans.push(Span::styled(part.to_string(), style));
    };

    let prefix_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let suffix_style = Style::default().fg(Color::DarkGray);
    let extra_style = Style::default().fg(color);

    let mut pos = 0;
    push_part(prefix, prefix_style, pos);
    pos += prefix.len();
    push_part(suffix, suffix_style, pos);
    pos += suffix.len();
    push_part(&extra, extra_style, pos);
}
