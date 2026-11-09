use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, SearchQuery};
use crate::idx::RowIdx;
use crate::theme::Theme;
use crate::types::SearchScopes;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchRowState {
    None,
    Match,
    Current,
}

#[derive(Clone, Copy)]
pub(super) struct SearchRender<'a> {
    pub query: &'a SearchQuery,
    pub scopes: SearchScopes,
    pub row_state: SearchRowState,
}

pub(super) fn search_row_state(app: &App, row_idx: RowIdx) -> SearchRowState {
    let Some(search) = &app.search else {
        return SearchRowState::None;
    };
    if !app.is_match(row_idx) {
        return SearchRowState::None;
    }
    if let Some(current) = search.current_match
        && search.matches.get(current).copied() == Some(row_idx)
    {
        return SearchRowState::Current;
    }
    SearchRowState::Match
}

pub(super) fn search_gutter<'a>(state: SearchRowState, theme: &Theme) -> Span<'a> {
    match state {
        SearchRowState::None => Span::raw("  "),
        SearchRowState::Match => Span::styled("│ ", Style::default().fg(theme.muted)),
        SearchRowState::Current => Span::styled(
            "┃ ",
            Style::default()
                .fg(theme.change_id)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

/// Convenience: extract the search row state and build the gutter span.
pub(super) fn gutter_span<'a>(search: Option<&SearchRender<'_>>, theme: &Theme) -> Span<'a> {
    search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
        theme,
    )
}

/// Push a span that may be search-highlighted, depending on scope and query match.
pub(super) fn push_searchable(
    out: &mut Vec<Span<'static>>,
    text: &str,
    scope: SearchScopes,
    style: Style,
    search: Option<&SearchRender<'_>>,
) {
    if let Some(s) = search
        && s.scopes.contains(scope)
        && s.query.matches(text)
    {
        push_highlighted(out, text, s.query, style);
        return;
    }
    out.push(Span::styled(text.to_string(), style));
}

/// Push the remote `name`, highlighted where the query matches it as a
/// remote: see [`SearchQuery::find_remote`].
pub(super) fn push_remote_searchable(
    out: &mut Vec<Span<'static>>,
    name: &str,
    style: Style,
    search: Option<&SearchRender<'_>>,
) {
    if let Some(s) = search
        && s.scopes.contains(SearchScopes::REMOTE)
        && let Some(highlight) = s.query.find_remote(name)
    {
        push_span_with_highlight(out, name.to_string(), style, &highlight, 0);
        return;
    }
    out.push(Span::styled(name.to_string(), style));
}

/// Push `text`, with where `query` matches in it highlighted.
pub(super) fn push_highlighted(
    out: &mut Vec<Span<'static>>,
    text: &str,
    query: &SearchQuery,
    base: Style,
) {
    match query.find(text) {
        Some(highlight) => push_span_with_highlight(out, text.to_string(), base, &highlight, 0),
        None => out.push(Span::styled(text.to_string(), base)),
    }
}

/// Push a single text span, splitting it at a highlight range and applying
/// `REVERSED` to the overlapping portion. `global_offset` is the span's
/// starting byte position within the full line.
pub(super) fn push_span_with_highlight(
    out: &mut Vec<Span<'static>>,
    text: String,
    style: Style,
    highlight: &std::ops::Range<usize>,
    global_offset: usize,
) {
    let span_end = global_offset + text.len();
    let overlap_start = highlight.start.max(global_offset);
    let overlap_end = highlight.end.min(span_end);

    if overlap_start >= overlap_end {
        out.push(Span::styled(text, style));
        return;
    }

    let local_start = overlap_start - global_offset;
    let local_end = overlap_end - global_offset;

    if local_start > 0 {
        out.push(Span::styled(text[..local_start].to_string(), style));
    }
    out.push(Span::styled(
        text[local_start..local_end].to_string(),
        style.add_modifier(Modifier::REVERSED),
    ));
    if local_end < text.len() {
        out.push(Span::styled(text[local_end..].to_string(), style));
    }
}

/// Push syntax-colored token spans with search match highlighting overlaid.
///
/// Builds the full content from expanded tokens, finds the match position,
/// then splits each token span at the highlight boundary.
pub(super) fn push_tokens_with_search(
    out: &mut Vec<Span<'static>>,
    tokens: &[crate::dag::SyntaxToken],
    tab_spaces: &str,
    search: Option<&SearchRender<'_>>,
) {
    let Some(s) = search
        .filter(|s| s.scopes.contains(SearchScopes::LINE) && s.row_state != SearchRowState::None)
    else {
        for t in tokens {
            let text = super::list::expand_tabs(&t.text, tab_spaces);
            let style = Style::default().fg(Color::Indexed(t.color_idx));
            out.push(Span::styled(text, style));
        }
        return;
    };

    let expanded: Vec<(String, Style)> = tokens
        .iter()
        .map(|t| {
            (
                super::list::expand_tabs(&t.text, tab_spaces),
                Style::default().fg(Color::Indexed(t.color_idx)),
            )
        })
        .collect();

    let full_content: String = expanded.iter().map(|(t, _)| t.as_str()).collect();
    let Some(highlight) = s.query.find(&full_content).filter(|r| !r.is_empty()) else {
        for (text, style) in expanded {
            out.push(Span::styled(text, style));
        }
        return;
    };

    let mut pos = 0usize;
    for (text, style) in expanded {
        let offset = pos;
        pos += text.len();
        push_span_with_highlight(out, text, style, &highlight, offset);
    }
}

#[cfg(test)]
mod highlight_tests {
    use ratatui::style::{Modifier, Style};

    use super::push_highlighted;
    use crate::app::SearchQuery;

    /// The match is cut out of the text as it is, even where lowercasing it
    /// would shift the byte offsets.
    #[test]
    fn highlights_land_on_the_match() {
        let mut spans = Vec::new();
        push_highlighted(&mut spans, "İé", &SearchQuery::new("é"), Style::default());
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(highlighted, ["é"]);
    }
}
