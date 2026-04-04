use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::dag::ShortId;
use crate::theme::Theme;

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

pub(super) fn push_highlighted_short_id(
    spans: &mut Vec<Span<'static>>,
    id: &ShortId,
    extra_suffix: Option<String>,
    color: Color,
    query: &str,
    case_sensitive: bool,
    theme: &Theme,
) {
    let prefix = &id.display[..id.prefix_len.min(id.display.len())];
    let suffix = &id.display[id.prefix_len.min(id.display.len())..];
    let extra = extra_suffix.unwrap_or_default();
    let text = format!("{prefix}{suffix}{extra}");

    let (hay, needle) = if case_sensitive {
        (text.clone(), query.to_string())
    } else {
        (text.to_lowercase(), query.to_lowercase())
    };
    let match_range = hay.find(&needle).map(|start| start..start + needle.len());

    let mut push_part = |part: &str, style: Style, global_start: usize| {
        if part.is_empty() {
            return;
        }
        if let Some(range) = &match_range {
            let part_start = global_start;
            let part_end = global_start + part.len();
            let overlap_start = range.start.max(part_start);
            let overlap_end = range.end.min(part_end);
            if overlap_start < overlap_end {
                let local_start = overlap_start - part_start;
                let local_end = overlap_end - part_start;
                if local_start > 0 {
                    spans.push(Span::styled(part[..local_start].to_string(), style));
                }
                spans.push(Span::styled(
                    part[local_start..local_end].to_string(),
                    style.add_modifier(Modifier::REVERSED),
                ));
                if local_end < part.len() {
                    spans.push(Span::styled(part[local_end..].to_string(), style));
                }
                return;
            }
        }
        spans.push(Span::styled(part.to_string(), style));
    };

    let prefix_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let suffix_style = Style::default().fg(theme.muted);
    let extra_style = Style::default().fg(color);

    let mut pos = 0;
    push_part(prefix, prefix_style, pos);
    pos += prefix.len();
    push_part(suffix, suffix_style, pos);
    pos += suffix.len();
    push_part(&extra, extra_style, pos);
}
