use std::collections::HashSet;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::keymap::{self, CommandFlags, HelpEntry, HelpGroup, KeymapNode};
use crate::theme::Theme;
use crate::types::{scope_specs_for_view, FollowUpOption, SearchFocus, SelectionContext};

/// A plain block with only a top border (used by several simple overlay panels).
fn top_border(theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
}

pub(super) type HelpColumn<'a> = Vec<&'a (HelpGroup, Vec<HelpEntry>)>;

/// Balance help groups into two columns, keeping groups intact.
pub(super) fn balance_help_groups(
    groups: &[(HelpGroup, Vec<HelpEntry>)],
) -> (HelpColumn<'_>, HelpColumn<'_>) {
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

pub(super) fn draw_help(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    left_groups: &[&(HelpGroup, Vec<HelpEntry>)],
    right_groups: &[&(HelpGroup, Vec<HelpEntry>)],
    scroll: u16,
    theme: &Theme,
) {
    use ratatui::widgets::Padding;

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
        // Empty title as left padding for key
        .title("")
        .title(" ? Help ")
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .padding(Padding::new(1, 1, 0, 0));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);

    render_help_column(frame, left_area, app, left_groups, scroll, theme);
    render_help_column(frame, right_area, app, right_groups, scroll, theme);
}

fn render_help_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    groups: &[&(HelpGroup, Vec<HelpEntry>)],
    scroll: u16,
    theme: &Theme,
) {
    let header_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(theme.selection)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.text);

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
                    .fg(theme.muted)
                    .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
            } else {
                key_style
            };
            let desc_style = if blocked {
                desc_style
                    .fg(theme.muted)
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
    let mut state = TableState::default().with_offset(scroll as usize);
    frame.render_stateful_widget(table, area, &mut state);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_submenu(
    frame: &mut Frame,
    area: Rect,
    key: &str,
    label: &str,
    children: &[(keymap_parser::Node, KeymapNode)],
    flags: CommandFlags,
    selection_suffix: Option<String>,
    selection: &SelectionContext,
    theme: &Theme,
) {
    // Build toggle indicators for the title bar.
    let mut toggle_spans: Vec<Span> = Vec::new();
    for (key_node, child) in children.iter() {
        if let KeymapNode::Toggle { flag, description } = child {
            let active = flags.contains(*flag);
            let key_str = keymap::display_key(key_node);
            let style = if active {
                Style::default()
                    .fg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
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
        .border_style(Style::default().fg(theme.muted))
        .title("")
        .title(title)
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(toggle_spans));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Content: action hints.
    let mut action_spans: Vec<Span> = Vec::new();
    for (key_node, child) in children.iter() {
        let (desc, blocked) = match child {
            KeymapNode::Action {
                action,
                description,
                ..
            } => {
                let blocked = selection.is_active()
                    && !keymap::action_supported_selection_kinds(*action)
                        .contains(&selection.kind());
                (*description, blocked)
            }
            KeymapNode::Prefix { label, .. } => (*label, false),
            KeymapNode::Toggle { .. } => continue,
        };
        if !action_spans.is_empty() {
            action_spans.push(Span::raw("  "));
        }
        let key_str = keymap::display_key(key_node);
        let key_style = if blocked {
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
        } else {
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD)
        };
        let desc_style = if blocked {
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT)
        } else {
            Style::default().fg(theme.text)
        };
        action_spans.push(Span::styled(format!("({key_str})"), key_style));
        action_spans.push(Span::styled(format!(" {desc}"), desc_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(action_spans)).wrap(Wrap { trim: false }),
        inner,
    );
}

/// Render a single-line text input with horizontal scrolling and cursor placement.
///
/// `prefix_width` is the number of columns already consumed before the input
/// (e.g. a prompt or label). The input text scrolls within the remaining space.
fn render_scrollable_input(
    frame: &mut Frame,
    area: Rect,
    prefix_width: u16,
    input: &tui_input::Input,
    theme: &Theme,
) {
    let input_width = area.width.saturating_sub(prefix_width);
    let scroll = input.visual_scroll(input_width.saturating_sub(1) as usize);
    let text = Span::styled(&input.value()[scroll..], Style::default().fg(theme.text));
    frame.render_widget(
        Paragraph::new(Line::from(text)),
        Rect {
            x: area.x + prefix_width,
            y: area.y,
            width: input_width,
            height: area.height,
        },
    );
    let cursor_x = area.x + prefix_width + (input.visual_cursor() - scroll) as u16;
    frame.set_cursor_position((cursor_x, area.y));
}

pub(super) fn draw_text_input(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    input: &tui_input::Input,
    theme: &Theme,
) {
    let block = top_border(theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_span = Span::styled(
        prompt,
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let prompt_width = prompt.len() as u16;
    frame.render_widget(Paragraph::new(Line::from(prompt_span)), inner);

    render_scrollable_input(frame, inner, prompt_width, input, theme);
}

pub(super) fn draw_search_input(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(search) = &app.search else {
        return;
    };
    let mut scope_spans: Vec<Span> = Vec::new();
    for spec in scope_specs_for_view(app.active_view).iter() {
        let enabled = search.scopes.contains(spec.flag);
        let mut style = if enabled {
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        if matches!(search.focus, SearchFocus::Scopes) {
            style = style.bg(theme.selection_bg);
        }
        scope_spans.push(Span::styled(
            format!(" [{}] {} ", spec.hint, spec.label),
            style,
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
        .title("")
        .title(" Search ")
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(scope_spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if matches!(search.focus, SearchFocus::Query) {
        render_scrollable_input(frame, inner, 0, &search.input, theme);
    } else {
        let line = Line::from(Span::styled(
            search.query(),
            Style::default().fg(theme.text),
        ));
        frame.render_widget(Paragraph::new(line), inner);
    }
}

pub(super) fn draw_target_select(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    source: &str,
    theme: &Theme,
) {
    let block = top_border(theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let spans = vec![
        Span::styled(
            format!("{prompt} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("from {source}"),
            Style::default()
                .fg(theme.change_id)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " → select target (Enter = confirm, Esc = cancel)",
            Style::default().fg(theme.muted),
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

pub(super) fn draw_commit_select(frame: &mut Frame, area: Rect, prompt: &str, theme: &Theme) {
    let block = top_border(theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let spans = vec![
        Span::styled(
            format!("{prompt} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "select commit (Enter = confirm, Esc = cancel)",
            Style::default().fg(theme.muted),
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

pub(super) fn draw_follow_up(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    options: &[FollowUpOption],
    theme: &Theme,
) {
    let block = top_border(theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![Span::styled(
        format!("{prompt} "),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];

    for (i, opt) in options.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("({})", opt.key),
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", opt.label),
            Style::default().fg(theme.text),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_select_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: &[String],
    filtered_indices: &[usize],
    cursor: usize,
    scroll_offset: &mut usize,
    marked: &HashSet<usize>,
    multi: bool,
    filter: &str,
    filtering: bool,
    theme: &Theme,
) {
    use ratatui::widgets::Padding;

    // Build title: " title [filter: text] (count) "
    let title_prefix = format!(" {title} ");
    let title_prefix_len = title_prefix.len();
    let mut title_spans: Vec<Span> = vec![Span::styled(
        title_prefix,
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];

    // Always show [filter: ] — add a space after the text for the cursor to sit in.
    let filter_label = format!("[filter: {filter} ] ");
    let filter_cursor_offset = title_prefix_len + "[filter: ".len();
    let filter_style = if filtering {
        Style::default().fg(theme.text)
    } else if !filter.is_empty() {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.muted)
    };
    title_spans.push(Span::styled(&filter_label, filter_style));

    if filtered_indices.len() != items.len() {
        title_spans.push(Span::styled(
            format!("({}/{}) ", filtered_indices.len(), items.len()),
            Style::default().fg(theme.muted),
        ));
    } else if multi && !marked.is_empty() {
        title_spans.push(Span::styled(
            format!("({} selected) ", marked.len()),
            Style::default().fg(theme.muted),
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
        .title(Line::from(title_spans))
        .padding(Padding::new(1, 1, 0, 0));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let list_h = inner.height as usize;

    // Scroll clamping.
    if cursor < *scroll_offset {
        *scroll_offset = cursor;
    }
    if list_h > 0 && cursor >= *scroll_offset + list_h {
        *scroll_offset = cursor + 1 - list_h;
    }
    let max_offset = filtered_indices.len().saturating_sub(list_h);
    *scroll_offset = (*scroll_offset).min(max_offset);

    let lines: Vec<Line> = filtered_indices
        .iter()
        .enumerate()
        .skip(*scroll_offset)
        .take(list_h)
        .map(|(filter_idx, &orig_idx)| {
            let item = &items[orig_idx];
            let is_cursor = filter_idx == cursor;
            let is_marked = marked.contains(&orig_idx);
            let prefix = if multi {
                match (is_cursor, is_marked) {
                    (true, true) => "▸ ● ",
                    (true, false) => "▸ ○ ",
                    (false, true) => "  ● ",
                    (false, false) => "  ○ ",
                }
            } else if is_cursor {
                "▸ "
            } else {
                "  "
            };
            let style = if is_cursor {
                Style::default()
                    .fg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if is_marked {
                Style::default().fg(theme.selection)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(format!("{prefix}{item}"), style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);

    // Show blinking cursor in the title bar when filter is focused.
    if filtering {
        // Title border starts at area.x, title text offset by border char.
        // filter_cursor_offset = " title " + "[filter: " — points right after the colon+space.
        let cursor_x = area.x + filter_cursor_offset as u16 + filter.len() as u16;
        frame.set_cursor_position((cursor_x, area.y));
    }
}

pub(super) fn draw_command_output(
    frame: &mut Frame,
    area: Rect,
    command: &str,
    output: &[u8],
    success: bool,
    theme: &Theme,
) {
    use ansi_to_tui::IntoText;
    use ratatui::widgets::Padding;

    let mut lines = vec![Line::from(Span::styled(
        command,
        Style::default()
            .fg(theme.accent)
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

    let border_color = if success { theme.muted } else { theme.error };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::new(1, 1, 0, 0));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
