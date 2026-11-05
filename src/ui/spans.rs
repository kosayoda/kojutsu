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

/// `s` cut to at most `width` display columns, ending in `…` when anything
/// had to go. Cuts between characters, never inside one.
pub(super) fn truncate_with_ellipsis(s: &str, width: usize) -> String {
    let full: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if full <= width {
        return s.to_string();
    }
    let Some(room) = width.checked_sub(1) else {
        return String::new();
    };
    let mut used = 0;
    let mut cut: String = s
        .chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= room
        })
        .collect();
    cut.push('…');
    cut
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
    let (prefix, suffix) = id.split();

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

/// Push a `ShortId` as [`push_short_id`] does, with where `query` matches
/// in it, and in `extra_suffix` after it, highlighted.
pub(super) fn push_highlighted_short_id(
    spans: &mut Vec<Span<'static>>,
    id: &ShortId,
    extra_suffix: Option<String>,
    color: Color,
    query: &crate::app::SearchQuery,
    theme: &Theme,
) {
    use super::search::push_span_with_highlight;

    let (prefix, suffix) = id.split();
    let extra = extra_suffix.unwrap_or_default();
    let text = format!("{prefix}{suffix}{extra}");

    let Some(highlight) = query.find(&text) else {
        // No match: render normally.
        push_short_id(spans, id, color, theme);
        if !extra.is_empty() {
            spans.push(Span::styled(extra, Style::default().fg(color)));
        }
        return;
    };

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
            CommandPartKind::String | CommandPartKind::Url => Style::default().fg(theme.added),
            CommandPartKind::Fileset => Style::default().fg(theme.text),
        };
        spans.push(Span::styled(part.text.clone(), style));
    }
    spans
}

#[cfg(test)]
mod truncate_tests {
    use super::truncate_with_ellipsis;

    /// Widths are display columns: a 3-byte `…` and a 2-column CJK
    /// character are cut around, never through, and text that fits in
    /// columns is left alone even when it is longer in bytes.
    #[test]
    fn cuts_by_display_width() {
        assert_eq!(truncate_with_ellipsis("interdiff…", 10), "interdiff…");
        assert_eq!(truncate_with_ellipsis("interdiff…", 9), "interdif…");
        assert_eq!(truncate_with_ellipsis("ファイル一覧", 5), "ファ…");
        assert_eq!(truncate_with_ellipsis("ファイル一覧", 6), "ファ…");
        assert_eq!(truncate_with_ellipsis("ファイル一覧", 7), "ファイ…");
    }

    #[test]
    fn no_room_leaves_the_ellipsis_or_nothing() {
        assert_eq!(truncate_with_ellipsis("abc", 1), "…");
        assert_eq!(truncate_with_ellipsis("abc", 0), "");
    }
}
