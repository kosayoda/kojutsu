use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
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
    pub query: &'a str,
    pub scopes: SearchScopes,
    pub case_sensitive: bool,
    pub row_state: SearchRowState,
}

pub(super) fn search_row_state(app: &App, row_idx: usize) -> SearchRowState {
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

pub(super) fn contains_query(haystack: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(query)
    } else {
        haystack.to_lowercase().contains(&query.to_lowercase())
    }
}

pub(super) fn push_highlighted<'a>(
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
