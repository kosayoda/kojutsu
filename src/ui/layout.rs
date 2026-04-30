use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, StatusLevel, GLOBAL_TOGGLES};
use crate::theme::Theme;

/// Minimum separator between repo and revset when on a single line.
pub(super) const HEADER_SEP: &str = "  ";

pub(super) fn draw_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    single_line: bool,
) {
    let show_ws_filter = app.active_view == crate::app::ActiveView::Operations
        && !app.op_log.workspace_filter.is_empty();
    let workspace_filter_line: Option<Line> = if !show_ws_filter {
        None
    } else {
        let mut names: Vec<&str> = app
            .op_log
            .workspace_filter
            .iter()
            .map(|s| s.as_str())
            .collect();
        names.sort();
        Some(Line::from(vec![
            Span::styled("workspace: ", Style::default().fg(theme.muted)),
            Span::styled(
                names.join(", "),
                Style::default()
                    .fg(theme.workspace)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
    };

    let mut header = if single_line {
        // Single-line: "repository: <path>  revset: <revset>"
        vec![Line::from(vec![
            Span::styled("repository: ", Style::default().fg(theme.muted)),
            Span::styled(&app.repo_root, Style::default().fg(theme.text)),
            Span::raw(HEADER_SEP),
            Span::styled(
                if let Some(preset) = app
                    .revset
                    .active_preset
                    .and_then(|i| app.revset.presets.get(i))
                {
                    format!("revset ({}): ", preset.name)
                } else {
                    "revset: ".to_string()
                },
                Style::default().fg(theme.muted),
            ),
            Span::styled(&app.revset.current, Style::default().fg(theme.accent)),
        ])]
    } else {
        vec![
            Line::from(vec![
                Span::styled("repository: ", Style::default().fg(theme.muted)),
                Span::styled(&app.repo_root, Style::default().fg(theme.text)),
            ]),
            Line::from(vec![
                Span::styled(
                    if let Some(preset) = app
                        .revset
                        .active_preset
                        .and_then(|i| app.revset.presets.get(i))
                    {
                        format!("revset ({}): ", preset.name)
                    } else {
                        "revset: ".to_string()
                    },
                    Style::default().fg(theme.muted),
                ),
                Span::styled(&app.revset.current, Style::default().fg(theme.accent)),
            ]),
        ]
    };
    if let Some(line) = workspace_filter_line {
        header.push(line);
    }
    frame.render_widget(Paragraph::new(header), area);
}

pub(super) fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Build toggle indicators for the title bar.
    let mut toggle_spans: Vec<Span> = Vec::new();
    for toggle in GLOBAL_TOGGLES {
        let active = app.toggles.contains(toggle.flag);
        let style = if active {
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        toggle_spans.push(Span::styled(
            format!(" [{}] {} ", toggle.hint, toggle.label),
            style,
        ));
    }

    if let Some(text) = app.selection.display_text() {
        toggle_spans.push(Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let view_title = match app.active_view {
        crate::app::ActiveView::Dag => " Log ",
        crate::app::ActiveView::Bookmarks => " Bookmarks ",
        crate::app::ActiveView::Tags => " Tags ",
        crate::app::ActiveView::Operations => " Operations ",
        crate::app::ActiveView::Evolog => " Evolog ",
        crate::app::ActiveView::Workspaces => " Workspaces ",
    };

    let mut wc_spans: Vec<Span> = Vec::new();
    if !app.has_working_copy() {
        wc_spans.push(Span::styled(
            " @ not visible ",
            Style::default().fg(theme.warning),
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
        .title("")
        .title(view_title)
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(toggle_spans))
        .title(Line::from(wc_spans))
        .title(
            Line::from(Span::styled(" ? Help ", Style::default().fg(theme.text))).right_aligned(),
        )
        .title(Line::from("").right_aligned());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (content, color) = if let Some(search) = &app.search {
        let pos = search.current_match.map(|i| i + 1).unwrap_or(0);
        (
            format!(
                "search: {} ({}/{})",
                search.query(),
                pos,
                search.matches.len()
            ),
            theme.text,
        )
    } else if let Some((status, level)) = &app.status_message {
        let c = match level {
            StatusLevel::Info => theme.text,
            StatusLevel::Error => theme.error,
        };
        (status.clone(), c)
    } else {
        (app.last_command.clone().unwrap_or_default(), theme.text)
    };
    let line = Line::from(Span::styled(content, Style::default().fg(color)));
    frame.render_widget(Paragraph::new(line), inner);
}
