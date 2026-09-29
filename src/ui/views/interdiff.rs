use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::RowContext;
use crate::ui::search::{SearchRender, gutter_span};

/// The interdiff view's heading: which two versions it compares.
pub(in crate::ui) fn render_interdiff_header(
    ctx: &RowContext<'_>,
    search: Option<&SearchRender<'_>>,
) -> Vec<Line<'static>> {
    let theme = &ctx.config.theme;
    let (from, to) = ctx
        .app
        .interdiff
        .as_ref()
        .map(|i| (i.from_label.to_string(), i.to_label.to_string()))
        .unwrap_or_default();
    vec![Line::from(vec![
        gutter_span(search, theme),
        Span::styled("  interdiff: ", Style::default().fg(theme.muted)),
        Span::styled(from, Style::default().fg(theme.change_id)),
        Span::styled(" \u{2192} ", Style::default().fg(theme.muted)),
        Span::styled(to, Style::default().fg(theme.change_id)),
    ])]
}
