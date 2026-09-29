use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{RenderedRow, RowContext};
use crate::idx::{AnnotateDetailIdx, AnnotateLineIdx};
use crate::types::SearchScopes;
use crate::ui::list::{expand_tabs, pad_or_truncate};
use crate::ui::search::{SearchRender, gutter_span, push_searchable, push_tokens_with_search};
use crate::ui::spans::push_short_id;

/// Paint the cursor row's highlight across the whole width. The annotate
/// view draws its own highlight: the list's would stop at the text.
fn highlight_cursor_row(spans: &mut Vec<Span<'static>>, ctx: &RowContext<'_>) {
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used < ctx.width {
        spans.push(Span::raw(" ".repeat(ctx.width - used)));
    }
    for span in spans {
        span.style = span.style.bg(ctx.config.theme.selection_bg_strong);
    }
}

/// One annotated line, preceded by a separator where the commit changes
/// (if separators are on).
pub(in crate::ui) fn render_annotate_line(
    ctx: &RowContext<'_>,
    line_idx: AnnotateLineIdx,
    is_cursor: bool,
    search: Option<&SearchRender<'_>>,
) -> RenderedRow {
    let theme = &ctx.config.theme;
    let lines = ctx.app.annotate.lines.loaded();
    let Some(line) = lines.and_then(|l| l.get(line_idx.raw())) else {
        return vec![Line::raw("")].into();
    };
    let same_commit = ctx
        .annotate_highlight
        .as_ref()
        .is_some_and(|c| *c == line.commit_id);
    let is_boundary = line_idx
        .raw()
        .checked_sub(1)
        .and_then(|prev| lines.and_then(|l| l.get(prev)))
        .is_some_and(|prev| prev.commit_id != line.commit_id);

    let mut spans = vec![];
    if is_cursor {
        spans.push(Span::styled("▌", Style::default().fg(theme.accent)));
        spans.push(Span::raw(" "));
    } else {
        spans.push(gutter_span(search, theme));
    }
    spans.push(Span::raw("  "));
    push_short_id(&mut spans, &line.change_id, theme.change_id, theme);
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        pad_or_truncate(&line.author, 15),
        Style::default().fg(theme.selection),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{} ", pad_or_truncate(&line.relative_time, 15)),
        Style::default().fg(theme.muted),
    ));
    let gutter_end = spans.len();
    spans.push(Span::styled(
        format!("{:>5}: ", line.line_number),
        Style::default().fg(theme.muted),
    ));
    if line.syntax_tokens.is_empty() {
        let content = expand_tabs(&line.content, ctx.tab_spaces);
        push_searchable(
            &mut spans,
            &content,
            SearchScopes::LINE,
            Style::default().fg(theme.text),
            search,
        );
    } else {
        push_tokens_with_search(&mut spans, &line.syntax_tokens, ctx.tab_spaces, search);
    }
    if is_cursor {
        highlight_cursor_row(&mut spans, ctx);
    } else if same_commit {
        for span in &mut spans[..gutter_end] {
            span.style = span.style.bg(theme.selection_bg);
        }
    }

    let mut rendered = Vec::new();
    if is_boundary && ctx.app.annotate.show_commit_separators {
        rendered.push(Line::styled(
            "─".repeat(ctx.width),
            Style::default().fg(theme.muted),
        ));
    }
    rendered.push(Line::from(spans));
    RenderedRow {
        label_line: rendered.len() - 1,
        lines: rendered,
    }
}

/// One row of an unfolded line's commit details.
pub(in crate::ui) fn render_annotate_detail(
    ctx: &RowContext<'_>,
    line_idx: AnnotateLineIdx,
    detail_idx: AnnotateDetailIdx,
    is_cursor: bool,
    search: Option<&SearchRender<'_>>,
) -> Vec<Line<'static>> {
    let theme = &ctx.config.theme;
    let annotate = &ctx.app.annotate;
    let Some(info) = annotate
        .lines
        .loaded()
        .and_then(|l| l.get(line_idx.raw()))
        .and_then(|line| annotate.commit_info.get(&line.commit_id))
    else {
        return vec![Line::raw("")];
    };
    let label = Style::default().fg(theme.muted);
    let text = Style::default().fg(theme.text);
    let mut spans = vec![gutter_span(search, theme)];
    match detail_idx.raw() {
        0 => {
            spans.push(Span::styled("      Change:    ", label));
            push_short_id(&mut spans, &info.change_id, theme.change_id, theme);
        }
        1 => {
            spans.push(Span::styled("      Commit:    ", label));
            push_short_id(&mut spans, &info.commit_id, theme.commit_id, theme);
        }
        2 => {
            spans.push(Span::styled("      Author:    ", label));
            spans.push(Span::styled(
                format!(
                    "{} <{}>  {}",
                    info.author_name, info.author_email, info.author_date
                ),
                text,
            ));
        }
        3 => {
            spans.push(Span::styled("      Committer: ", label));
            spans.push(Span::styled(
                format!(
                    "{} <{}>  {}",
                    info.committer_name, info.committer_email, info.committer_date
                ),
                text,
            ));
        }
        di => {
            let description = if info.description_lines.is_empty() {
                "(no description set)"
            } else {
                info.description_lines
                    .get(di - 4)
                    .map_or("", String::as_str)
            };
            spans.push(Span::styled(format!("      {description}"), text));
        }
    }
    if is_cursor {
        highlight_cursor_row(&mut spans, ctx);
    }
    vec![Line::from(spans)]
}
