use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{OpDetailLine, OpLogEntry};
use crate::theme::Theme;
use crate::types::SearchScopes;

use crate::ui::search::{SearchRender, gutter_span, push_searchable};
use crate::ui::spans::{dot, push_short_id};

pub(crate) fn render_op_log_item(
    entry: &OpLogEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    let graph_style = if entry.is_current {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    spans.push(Span::styled(entry.graph.node.clone(), graph_style));

    spans.push(Span::styled(
        entry.id.to_string(),
        Style::default().fg(theme.commit_id),
    ));

    let desc_style = if entry.is_snapshot {
        Style::default().fg(theme.muted)
    } else if entry.is_current {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };
    spans.push(dot(theme));
    push_searchable(
        &mut spans,
        &entry.description,
        SearchScopes::DESCRIPTION,
        desc_style,
        search,
    );

    spans.push(dot(theme));
    let time_color = if entry.is_snapshot {
        theme.muted
    } else {
        theme.accent
    };
    spans.push(Span::styled(
        entry.relative_time.to_string(),
        Style::default().fg(time_color),
    ));

    if !entry.user.is_empty() {
        spans.push(dot(theme));
        spans.push(Span::styled(
            entry.user.to_string(),
            Style::default().fg(theme.user),
        ));
    }

    let line1 = Line::from(spans);

    let mut spans2 = vec![
        gutter_span(search, theme),
        Span::styled(entry.graph.cont.clone(), Style::default().fg(theme.muted)),
    ];
    if let Some(ref ws) = entry.workspace {
        spans2.push(Span::styled(
            ws.to_string(),
            Style::default().fg(theme.workspace),
        ));
    }
    if let Some(ref args) = entry.args {
        if entry.workspace.is_some() {
            spans2.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        }
        push_searchable(
            &mut spans2,
            args,
            SearchScopes::PATH_COMMAND,
            Style::default().fg(theme.muted),
            search,
        );
    }
    let line2 = Line::from(spans2);

    vec![line1, line2]
}

pub(crate) fn render_op_detail_line(
    detail: Option<&OpDetailLine>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(detail) = detail else {
        return vec![Line::raw("")];
    };
    let muted = Style::default().fg(theme.muted);
    match detail {
        OpDetailLine::SectionHeader(text) => vec![Line::from(vec![
            Span::raw("    "),
            Span::styled(
                text.to_string(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ])],
        OpDetailLine::Commit(c) => {
            let (indicator, change_color, commit_color) = match c.kind {
                crate::dag::DiffKind::Added => (
                    Span::styled("      + ", Style::default().fg(theme.added)),
                    theme.change_id,
                    theme.commit_id,
                ),
                crate::dag::DiffKind::Removed => (
                    Span::styled("      - ", Style::default().fg(theme.error)),
                    theme.muted,
                    theme.muted,
                ),
            };
            let mut spans = vec![indicator];
            if !c.change_id.display.is_empty() {
                push_short_id(&mut spans, &c.change_id, change_color, theme);
                spans.push(Span::raw(" "));
            }
            push_short_id(&mut spans, &c.commit_id, commit_color, theme);
            if let Some(ref desc) = c.description {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    desc.clone(),
                    match c.kind {
                        crate::dag::DiffKind::Added => Style::default().fg(theme.text),
                        crate::dag::DiffKind::Removed => muted,
                    },
                ));
            }
            vec![Line::from(spans)]
        }
        OpDetailLine::WorkingCopy(wc) => {
            let mut spans = vec![Span::raw("      ")];
            spans.push(Span::styled(
                wc.workspace.to_string(),
                Style::default().fg(theme.workspace),
            ));
            spans.push(Span::styled(": ", muted));
            if let Some(ref new) = wc.new_commit {
                push_short_id(&mut spans, new, theme.commit_id, theme);
            }
            if let Some(ref old) = wc.old_commit {
                spans.push(Span::styled(" ← ", muted));
                push_short_id(&mut spans, old, theme.muted, theme);
            }
            vec![Line::from(spans)]
        }
        OpDetailLine::Bookmark(bm) => {
            let mut spans = vec![Span::raw("      ")];
            spans.push(Span::styled(
                bm.name.to_string(),
                Style::default()
                    .fg(theme.bookmark)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(": ", muted));
            if let Some(ref new) = bm.new_target {
                push_short_id(&mut spans, new, theme.commit_id, theme);
            } else {
                spans.push(Span::styled("(deleted)", muted));
            }
            if let Some(ref old) = bm.old_target {
                spans.push(Span::styled(" ← ", muted));
                push_short_id(&mut spans, old, theme.muted, theme);
            }
            vec![Line::from(spans)]
        }
    }
}
