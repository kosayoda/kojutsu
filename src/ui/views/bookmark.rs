use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::BookmarkViewEntry;
use crate::theme::Theme;
use crate::types::SearchScopes;

use crate::ui::search::{gutter_span, push_searchable, SearchRender};
use crate::ui::spans::push_short_id;

pub(crate) fn render_bookmark_item(
    entry: &BookmarkViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    if entry.kind.is_conflicted() {
        spans.push(Span::styled("! ", Style::default().fg(theme.error)));
    } else {
        spans.push(Span::raw("  "));
    }
    if let Some(remote) = entry.kind.remote() {
        let name_style = Style::default().fg(theme.text);
        push_searchable(
            &mut spans,
            entry.name.as_str(),
            SearchScopes::BOOKMARK,
            name_style,
            search,
        );
        spans.push(Span::styled(
            format!("@{remote}"),
            Style::default().fg(theme.muted),
        ));
    } else if entry.kind.is_dirty() {
        let name_display = format!("{}*", entry.name);
        let style = Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD);
        push_searchable(
            &mut spans,
            &name_display,
            SearchScopes::BOOKMARK,
            style,
            search,
        );
    } else {
        let style = Style::default()
            .fg(theme.bookmark)
            .add_modifier(Modifier::BOLD);
        push_searchable(
            &mut spans,
            entry.name.as_str(),
            SearchScopes::BOOKMARK,
            style,
            search,
        );
    }

    use crate::ui::spans::dot;
    if let Some(cid) = &entry.change_id {
        spans.push(dot(theme));
        push_short_id(&mut spans, cid, theme.change_id, theme);
    }
    if let Some(cid) = &entry.short_commit_id {
        spans.push(Span::raw(" "));
        push_short_id(&mut spans, cid, theme.commit_id, theme);
    }
    if let Some(desc) = &entry.description {
        spans.push(dot(theme));
        push_searchable(
            &mut spans,
            desc,
            SearchScopes::DESCRIPTION,
            Style::default().fg(theme.text),
            search,
        );
    }

    vec![Line::from(spans)]
}

fn push_short_id_with_suffix(
    spans: &mut Vec<Span<'static>>,
    id: &crate::dag::ShortId,
    color: Color,
    suffix: Option<usize>,
    theme: &Theme,
) {
    push_short_id(spans, id, color, theme);
    if let Some(n) = suffix {
        spans.push(Span::styled(format!("/{n}"), Style::default().fg(color)));
    }
}

pub(crate) fn render_bookmark_conflict_target(
    target: Option<&crate::dag::BookmarkConflictTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    let (indicator, color) = match target.kind {
        crate::dag::DiffKind::Added => ("      + ", theme.added),
        crate::dag::DiffKind::Removed => ("      - ", theme.error),
    };
    spans.push(Span::styled(indicator, Style::default().fg(color)));

    push_short_id_with_suffix(
        &mut spans,
        &target.summary.change_id,
        theme.change_id,
        target.change_id_suffix,
        theme,
    );
    spans.push(Span::raw(" "));
    push_short_id(
        &mut spans,
        &target.summary.short_commit_id,
        theme.commit_id,
        theme,
    );

    if target.is_hidden {
        spans.push(Span::styled(" (hidden)", Style::default().fg(theme.muted)));
    }

    if let Some(ref desc) = target.summary.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

pub(crate) fn render_bookmark_remote_target(
    target: Option<&crate::dag::BookmarkRemoteTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    spans.push(Span::styled(
        format!("      @{}", target.remote),
        Style::default().fg(theme.remote),
    ));

    let mut parts: Vec<String> = Vec::new();
    if !target.is_tracked {
        parts.push("untracked".to_string());
    }
    if let Some(n) = target.ahead_count {
        if n > 0 {
            parts.push(format!(
                "ahead by {} commit{}",
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
    }
    if let Some(n) = target.behind_count {
        if n > 0 {
            parts.push(format!(
                "behind by {} commit{}",
                n,
                if n == 1 { "" } else { "s" }
            ));
        }
    }
    if !parts.is_empty() {
        spans.push(Span::styled(
            format!(" ({})", parts.join(", ")),
            Style::default().fg(theme.text),
        ));
    }

    spans.push(Span::styled(": ", Style::default().fg(theme.muted)));

    push_short_id_with_suffix(
        &mut spans,
        &target.summary.change_id,
        theme.change_id,
        target.change_id_suffix,
        theme,
    );
    spans.push(Span::raw(" "));
    push_short_id(
        &mut spans,
        &target.summary.short_commit_id,
        theme.commit_id,
        theme,
    );

    if let Some(ref desc) = target.summary.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}
