use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TagViewEntry;
use crate::theme::Theme;
use crate::types::SearchScopes;

use super::push_ref_entry_suffix;
use crate::ui::search::{gutter_span, push_searchable, SearchRender};
use crate::ui::spans::push_short_id;

pub(crate) fn render_tag_item(
    entry: &TagViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    let style = if entry.is_deleted {
        Style::default().fg(theme.muted)
    } else {
        Style::default().fg(theme.tag).add_modifier(Modifier::BOLD)
    };
    push_searchable(
        &mut spans,
        entry.name.as_str(),
        SearchScopes::TAG,
        style,
        search,
    );

    if entry.is_deleted {
        spans.push(Span::styled(" (deleted)", Style::default().fg(theme.muted)));
    }

    push_ref_entry_suffix(
        &mut spans,
        entry.change_id.as_ref(),
        entry.description.as_deref(),
        search,
        theme,
    );

    vec![Line::from(spans)]
}

pub(crate) fn render_tag_remote_target(
    target: Option<&crate::dag::TagRemoteTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    spans.push(Span::styled(
        format!("    @{}", target.remote),
        Style::default().fg(theme.remote),
    ));

    spans.push(Span::styled(": ", Style::default().fg(theme.muted)));

    push_short_id(
        &mut spans,
        &target.summary.change_id,
        theme.change_id,
        theme,
    );
    spans.push(Span::raw(" "));
    push_short_id(
        &mut spans,
        &target.summary.short_commit_id,
        theme.commit_id,
        theme,
    );

    if let Some(ref desc) = target.summary.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}
