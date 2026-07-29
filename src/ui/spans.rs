use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::dag::ShortId;
use crate::theme::Theme;

/// Middle dot separator span: ` · ` in muted color.
pub(super) fn dot(theme: &Theme) -> Span<'static> {
    Span::styled(" · ", Style::default().fg(theme.muted))
}

/// Skip `skip` display columns from the left and cap at `max_width` visible columns.
pub(super) fn trim_line(line: Line<'static>, skip: usize, max_width: usize) -> Line<'static> {
    if skip == 0 {
        return line;
    }

    let mut result: Vec<Span<'static>> = Vec::new();
    let mut col: usize = 0;
    let mut visible: usize = 0;

    for span in line.spans {
        if visible >= max_width {
            break;
        }

        let style = span.style;
        let text = span.content.into_owned();
        let span_width: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();

        if col + span_width <= skip {
            col += span_width;
            continue;
        }

        // Find visible portion of this span.
        let mut start_byte = 0;
        let mut end_byte = text.len();
        let mut char_col = col;
        let mut span_visible = 0;

        for (bi, ch) in text.char_indices() {
            let w = ch.width().unwrap_or(0);
            if char_col < skip {
                char_col += w;
                start_byte = bi + ch.len_utf8();
                continue;
            }
            if visible + span_visible + w > max_width {
                end_byte = bi;
                break;
            }
            span_visible += w;
            char_col += w;
        }

        if start_byte < end_byte {
            result.push(Span::styled(text[start_byte..end_byte].to_string(), style));
            visible += span_visible;
        }

        col += span_width;
    }

    Line::from(result)
}

/// Compute the display width of a line (sum of all span character widths).
pub(super) fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| {
            s.content
                .chars()
                .map(|c| c.width().unwrap_or(0))
                .sum::<usize>()
        })
        .sum()
}

/// Push a `ShortId` as two spans: bright prefix + dimmed suffix.
pub(super) fn push_short_id(
    spans: &mut Vec<Span<'static>>,
    id: &ShortId,
    color: Color,
    theme: &Theme,
) {
    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];

    spans.push(Span::styled(
        prefix.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(
            suffix.to_string(),
            Style::default().fg(theme.muted),
        ));
    }
}

/// `needle` must already be lowercased when `case_sensitive` is false.
pub(super) fn push_highlighted_short_id(
    spans: &mut Vec<Span<'static>>,
    id: &ShortId,
    extra_suffix: Option<String>,
    color: Color,
    needle: &str,
    case_sensitive: bool,
    theme: &Theme,
) {
    use super::search::push_span_with_highlight;

    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];
    let extra = extra_suffix.unwrap_or_default();
    let text = format!("{prefix}{suffix}{extra}");

    let hay_lower;
    let hay = if case_sensitive {
        text.as_str()
    } else {
        hay_lower = text.to_lowercase();
        hay_lower.as_str()
    };
    let Some(start) = hay.find(needle) else {
        // No match — render normally.
        push_short_id(spans, id, color, theme);
        if !extra.is_empty() {
            spans.push(Span::styled(extra, Style::default().fg(color)));
        }
        return;
    };
    let highlight = start..start + needle.len();

    let prefix_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let suffix_style = Style::default().fg(theme.muted);
    let extra_style = Style::default().fg(color);

    let mut pos = 0;
    push_span_with_highlight(spans, prefix.to_string(), prefix_style, &highlight, pos);
    pos += prefix.len();
    push_span_with_highlight(spans, suffix.to_string(), suffix_style, &highlight, pos);
    pos += suffix.len();
    if !extra.is_empty() {
        push_span_with_highlight(spans, extra, extra_style, &highlight, pos);
    }
}

/// Render structured command parts as syntax-highlighted spans.
pub(super) fn command_parts_to_spans(
    parts: &[crate::jj_command::CommandPart],
    theme: &Theme,
) -> Vec<Span<'static>> {
    use crate::jj_command::CommandPartKind;

    let mut spans = Vec::with_capacity(parts.len() * 2);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = match part.kind {
            CommandPartKind::Prompt => Style::default().fg(theme.muted),
            CommandPartKind::Binary => Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
            CommandPartKind::Subcommand => Style::default().fg(theme.text),
            CommandPartKind::Flag => Style::default().fg(theme.text),
            CommandPartKind::Revision => Style::default()
                .fg(theme.change_id)
                .add_modifier(Modifier::BOLD),
            CommandPartKind::String => Style::default().fg(theme.added),
            CommandPartKind::Fileset => Style::default().fg(theme.text),
        };
        spans.push(Span::styled(part.text.clone(), style));
    }
    spans
}
