use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
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
    /// Pre-lowered query (equals the original query when case-sensitive).
    pub query_lower: &'a str,
    pub scopes: SearchScopes,
    pub case_sensitive: bool,
    pub row_state: SearchRowState,
}

pub(super) fn search_row_state(app: &App, row_idx: RowIdx) -> SearchRowState {
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

pub(super) fn contains_query(haystack: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(needle)
    }
}

/// Push a span that may be search-highlighted, depending on scope and query match.
pub(super) fn push_searchable(
    out: &mut Vec<Span<'static>>,
    text: &str,
    scope: SearchScopes,
    style: Style,
    search: Option<&SearchRender<'_>>,
) {
    if let Some(s) = search {
        if s.scopes.contains(scope) && contains_query(text, s.query_lower, s.case_sensitive) {
            push_highlighted(out, text, s.query_lower, style, s.case_sensitive);
            return;
        }
    }
    out.push(Span::styled(text.to_string(), style));
}

/// `needle` must already be lowercased when `case_sensitive` is false.
pub(super) fn push_highlighted<'a>(
    out: &mut Vec<Span<'a>>,
    text: &str,
    needle: &str,
    base: Style,
    case_sensitive: bool,
) {
    if needle.is_empty() {
        out.push(Span::styled(text.to_string(), base));
        return;
    }

    let hay_lower;
    let hay = if case_sensitive {
        text
    } else {
        hay_lower = text.to_lowercase();
        &hay_lower
    };
    if let Some(start) = hay.find(needle) {
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

/// Case-insensitive byte-offset search without allocating a lowercased copy.
fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
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
    let search = search
        .filter(|s| s.scopes.contains(SearchScopes::LINE) && s.row_state != SearchRowState::None);

    if search.is_none() {
        for t in tokens {
            let text = super::list::expand_tabs(&t.text, tab_spaces);
            let style = Style::default().fg(Color::Indexed(t.color_idx));
            out.push(Span::styled(text, style));
        }
        return;
    }
    let s = search.unwrap();

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
    let Some(start) = find_ignore_ascii_case(&full_content, s.query_lower) else {
        for (text, style) in expanded {
            out.push(Span::styled(text, style));
        }
        return;
    };
    let highlight = start..start + s.query_lower.len();

    let mut pos = 0usize;
    for (text, style) in expanded {
        let offset = pos;
        pos += text.len();
        push_span_with_highlight(out, text, style, &highlight, offset);
    }
}
