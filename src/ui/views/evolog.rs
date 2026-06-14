use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::EvoLogEntry;
use crate::theme::Config;
use crate::types::SearchScopes;

use super::push_graph_node_spans;
use crate::ui::search::{contains_query, gutter_span, push_searchable, SearchRender};
use crate::ui::spans::{dot, push_highlighted_short_id, push_short_id};

pub(crate) fn render_evolog_item(
    entry: &EvoLogEntry,
    search: Option<&SearchRender<'_>>,
    config: &Config,
) -> Vec<Line<'static>> {
    let theme = &config.theme;
    let mut spans = vec![gutter_span(search, theme)];

    let graph_style = if entry.is_current {
        Style::default()
            .fg(theme.added)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    push_graph_node_spans(&mut spans, &entry.graph.node, graph_style, config);

    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(
                &entry.change_id.display,
                search.query_lower,
                search.case_sensitive,
            )
        {
            push_highlighted_short_id(
                &mut spans,
                &entry.change_id,
                None,
                theme.change_id,
                search.query_lower,
                search.case_sensitive,
                theme,
            );
        } else {
            push_short_id(&mut spans, &entry.change_id, theme.change_id, theme);
        }
    } else {
        push_short_id(&mut spans, &entry.change_id, theme.change_id, theme);
    }

    spans.push(dot(theme));
    let desc = entry
        .description
        .as_deref()
        .unwrap_or("(no description set)");
    let desc_style = if entry.is_current {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };
    push_searchable(
        &mut spans,
        desc,
        SearchScopes::DESCRIPTION,
        desc_style,
        search,
    );

    let line1 = Line::from(spans);

    let mut spans2 = vec![
        gutter_span(search, theme),
        Span::styled(entry.graph.cont.clone(), Style::default().fg(theme.muted)),
    ];

    spans2.push(Span::styled(
        entry.author.to_string(),
        Style::default().fg(theme.user),
    ));

    spans2.push(dot(theme));
    spans2.push(Span::styled(
        entry.relative_time.to_string(),
        Style::default().fg(theme.accent),
    ));

    if let Some(ref op) = entry.op_description {
        spans2.push(dot(theme));
        spans2.push(Span::styled(
            op.to_string(),
            Style::default().fg(theme.muted),
        ));
    }

    let line2 = Line::from(spans2);
    vec![line1, line2]
}
