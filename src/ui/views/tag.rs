use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TagViewEntry;
use crate::theme::Theme;
use crate::types::SearchScopes;

use crate::ui::search::{SearchRender, gutter_span, push_searchable};
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

    use crate::ui::spans::dot;
    if let Some(cid) = &entry.change_id {
        spans.push(dot(theme));
        push_short_id(&mut spans, cid, theme.change_id, theme);
    }
    if let Some(cid) = &entry.short_commit_id {
        spans.push(Span::raw(" "));
        push_short_id(&mut spans, cid, theme.commit_id, theme);
    }
    if let Some(desc) = &entry.description {
        spans.push(dot(theme));
        push_searchable(
            &mut spans,
            desc,
            SearchScopes::DESCRIPTION,
            Style::default().fg(theme.text),
            search,
        );
    }

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

    // A remote-only target's commit may not be in the repo, leaving no change ID.
    if !target.summary.change_id.display().is_empty() {
        push_short_id(
            &mut spans,
            &target.summary.change_id,
            theme.change_id,
            theme,
        );
        spans.push(Span::raw(" "));
    }
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
