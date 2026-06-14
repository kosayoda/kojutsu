use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::types::SearchScopes;

use crate::ui::search::{gutter_span, push_searchable, SearchRender};
use crate::ui::spans::command_parts_to_spans;

pub(crate) fn render_command_log_item(
    entry: &crate::app::CommandLogEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::app::CommandLogKind;

    let (icon, icon_color) = match (entry.kind, entry.success) {
        (CommandLogKind::Warning, _) => ("!  ", theme.warning),
        (_, true) => ("✓  ", theme.added),
        (_, false) => ("✗  ", theme.error),
    };

    let summary_style = if entry.success {
        Style::default().fg(theme.text)
    } else {
        Style::default().fg(theme.error)
    };

    let relative_time = crate::repo::millis_to_relative_time(entry.timestamp.as_millisecond());

    let mut spans = vec![
        gutter_span(search, theme),
        Span::styled(
            icon,
            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(ref parts) = entry.command_parts {
        spans.extend(command_parts_to_spans(parts, theme));
    } else {
        push_searchable(
            &mut spans,
            &entry.summary,
            SearchScopes::DESCRIPTION,
            summary_style,
            search,
        );
    }
    spans.push(Span::styled(
        format!(" · {relative_time}"),
        Style::default().fg(theme.muted),
    ));

    vec![Line::from(spans)]
}

pub(crate) fn render_command_log_detail(
    entry: &crate::app::CommandLogEntry,
    line_idx: usize,
    _theme: &Theme,
) -> Vec<Line<'static>> {
    if let Some(line) = entry.parsed_lines.get(line_idx) {
        let mut spans: Vec<Span<'static>> = vec![Span::raw("      ")];
        spans.extend(line.spans.iter().cloned());
        return vec![Line::from(spans)];
    }
    let text = String::from_utf8_lossy(&entry.output);
    let line_text = text.lines().nth(line_idx).unwrap_or("");
    vec![Line::from(vec![
        Span::raw("      "),
        Span::raw(line_text.to_string()),
    ])]
}
