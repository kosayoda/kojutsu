mod bookmark;
mod command_log;
mod dag;
mod evolog;
mod oplog;
mod tag;
mod workspace;

pub(super) use bookmark::*;
pub(super) use command_log::*;
pub(super) use dag::*;
pub(super) use evolog::*;
pub(super) use oplog::*;
pub(super) use tag::*;
pub(super) use workspace::*;

use itertools::Itertools;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::dag::{DiffToken, DiffTokenKind, LineStats, ShortId};
use crate::theme::{Config, Theme};
use crate::types::SearchScopes;

use super::search::{SearchRender, gutter_span, push_searchable};
use super::spans::push_short_id;

#[derive(Default)]
pub(super) struct RenderFlags {
    pub is_source: bool,
    pub is_selected: bool,
    pub in_visual: bool,
}

pub(super) fn push_line_stats(
    out: &mut Vec<Span<'static>>,
    stats: LineStats,
    muted: bool,
    theme: &Theme,
) {
    if stats.added == 0 && stats.removed == 0 {
        return;
    }
    let added_color = if muted { theme.muted } else { theme.added };
    let removed_color = if muted { theme.muted } else { theme.error };
    out.push(Span::raw(" "));
    if stats.added > 0 {
        out.push(Span::styled(
            format!("+{}", stats.added),
            Style::default().fg(added_color),
        ));
        if stats.removed > 0 {
            out.push(Span::raw(" "));
        }
    }
    if stats.removed > 0 {
        out.push(Span::styled(
            format!("-{}", stats.removed),
            Style::default().fg(removed_color),
        ));
    }
}

pub(super) fn push_graph_node_spans(
    spans: &mut Vec<Span<'static>>,
    node: &str,
    glyph_style: Style,
    config: &Config,
) {
    let theme = &config.theme;
    for (is_glyph, group) in node
        .char_indices()
        .chunk_by(|&(_, c)| config.glyphs.is_glyph(c))
        .into_iter()
    {
        let mut iter = group.into_iter();
        let (start, c) = iter.next().expect("chunk_by groups are non-empty");
        let end = {
            let (end, c) = iter.last().unwrap_or((start, c));
            end + c.len_utf8()
        };
        let s = &node[start..end];
        if is_glyph {
            spans.push(Span::styled(s.to_string(), glyph_style));
        } else {
            spans.push(Span::styled(
                s.to_string(),
                Style::default().fg(theme.muted),
            ));
        }
    }
}

pub(super) fn render_simple_graph_link(
    graph_str: Option<&String>,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let s = graph_str.map(|s| s.as_str()).unwrap_or("");
    vec![Line::from(vec![
        gutter_span(search, theme),
        Span::styled(s.to_string(), Style::default().fg(theme.muted)),
    ])]
}

pub(super) fn push_ref_entry_suffix(
    spans: &mut Vec<Span<'static>>,
    change_id: Option<&ShortId>,
    description: Option<&str>,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) {
    use super::spans::dot;
    if let Some(cid) = change_id {
        spans.push(dot(theme));
        push_short_id(spans, cid, theme.change_id, theme);
    }
    if let Some(desc) = description {
        spans.push(dot(theme));
        push_searchable(
            spans,
            desc,
            SearchScopes::DESCRIPTION,
            Style::default().fg(theme.text),
            search,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_diff_tokens(
    spans: &mut Vec<Span<'static>>,
    content: &str,
    tokens: &[DiffToken],
    base_style: Style,
    underline: bool,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
    tab_str: &str,
) {
    let has_tokens = tokens.iter().any(|t| t.kind != DiffTokenKind::Unchanged);

    if !has_tokens || search.is_some_and(|s| s.row_state != super::search::SearchRowState::None) {
        let content = content.replace('\t', tab_str);
        push_searchable(spans, &content, SearchScopes::LINE, base_style, search);
        return;
    }

    for token in tokens {
        if token.text.is_empty() {
            continue;
        }
        let style = match token.kind {
            DiffTokenKind::Unchanged => base_style,
            DiffTokenKind::Removed => {
                let s = Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD);
                if underline {
                    s.add_modifier(Modifier::UNDERLINED)
                } else {
                    s
                }
            }
            DiffTokenKind::Added => {
                let s = Style::default()
                    .fg(theme.added)
                    .add_modifier(Modifier::BOLD);
                if underline {
                    s.add_modifier(Modifier::UNDERLINED)
                } else {
                    s
                }
            }
        };
        spans.push(Span::styled(
            super::list::expand_tabs(&token.text, tab_str),
            style,
        ));
    }
}
