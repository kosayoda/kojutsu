use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::dag::{RemoteEntry, RemoteRefCounts};
use crate::theme::Theme;
use crate::types::{SearchScopes, redact_password};

use crate::ui::search::{SearchRender, gutter_span, push_searchable};
use crate::ui::spans::dot;

/// Label of the push URL row, which the name column is wide enough for.
const PUSH_LABEL: &str = "push";

/// How wide the name column is: the widest remote name, so that every URL
/// starts in the same column.
pub(crate) fn remote_name_width(remotes: &[RemoteEntry]) -> usize {
    remotes
        .iter()
        .map(|entry| entry.remote.name.as_str().width())
        .chain([PUSH_LABEL.width()])
        .max()
        .unwrap_or_default()
}

/// A remote: its name, the URL it fetches from, and the bookmarks and
/// tags it has. URLs are shown with any password redacted.
pub(crate) fn render_remote_item(
    entry: &RemoteEntry,
    name_width: usize,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];
    let name = entry.remote.name.as_str();
    push_searchable(
        &mut spans,
        name,
        SearchScopes::DESCRIPTION,
        Style::default()
            .fg(theme.remote)
            .add_modifier(Modifier::BOLD),
        search,
    );
    spans.push(Span::raw(padding(name, name_width)));
    match entry.remote.fetch_url.as_deref() {
        Some(url) => push_searchable(
            &mut spans,
            &redact_password(url),
            SearchScopes::PATH_COMMAND,
            Style::default().fg(theme.text),
            search,
        ),
        None => spans.push(Span::styled(
            "(no fetch url)",
            Style::default().fg(theme.muted),
        )),
    }
    for (counts, singular) in [(entry.bookmarks, "bookmark"), (entry.tags, "tag")] {
        if let Some(text) = ref_counts(counts, singular) {
            spans.push(dot(theme));
            spans.push(Span::styled(text, Style::default().fg(theme.muted)));
        }
    }
    vec![Line::from(spans)]
}

/// The separate push URL under a remote, in the URL column.
pub(crate) fn render_remote_push_url(
    entry: &RemoteEntry,
    name_width: usize,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(url) = entry.remote.push_url.as_deref() else {
        return vec![Line::raw("")];
    };
    let mut spans = vec![gutter_span(search, theme)];
    spans.push(Span::styled(
        format!("{PUSH_LABEL:>name_width$}"),
        Style::default().fg(theme.muted),
    ));
    spans.push(Span::raw("  "));
    push_searchable(
        &mut spans,
        &redact_password(url),
        SearchScopes::PATH_COMMAND,
        Style::default().fg(theme.text),
        search,
    );
    vec![Line::from(spans)]
}

/// The gap after `name` that brings the URL to its column.
fn padding(name: &str, name_width: usize) -> String {
    " ".repeat(name_width.saturating_sub(name.width()) + 2)
}

/// "3 bookmarks, 1 tracked", or nothing when the remote has none.
fn ref_counts(counts: RemoteRefCounts, singular: &str) -> Option<String> {
    let RemoteRefCounts { total, tracked } = counts;
    if total == 0 {
        return None;
    }
    let plural = if total == 1 { "" } else { "s" };
    Some(match tracked {
        0 => format!("{total} {singular}{plural}"),
        _ => format!("{total} {singular}{plural}, {tracked} tracked"),
    })
}
