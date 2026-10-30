use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::TagViewEntry;
use crate::dag::TagPresence;
use crate::theme::Theme;
use crate::types::SearchScopes;

use crate::ui::search::{SearchRender, gutter_span, push_searchable};
use crate::ui::spans::push_short_id;

pub(crate) fn render_tag_item(
    entry: &TagViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    let style = match entry.presence {
        TagPresence::Local => Style::default().fg(theme.tag).add_modifier(Modifier::BOLD),
        TagPresence::Deleted | TagPresence::RemoteOnly => Style::default().fg(theme.muted),
    };
    push_searchable(
        &mut spans,
        entry.name.as_str(),
        SearchScopes::TAG,
        style,
        search,
    );

    let note = match entry.presence {
        TagPresence::Local => None,
        TagPresence::Deleted => Some(" (deleted)"),
        TagPresence::RemoteOnly => Some(" (remote only)"),
    };
    if let Some(note) = note {
        spans.push(Span::styled(note, Style::default().fg(theme.muted)));
    }
    if entry.is_conflicted {
        spans.push(Span::styled(
            " (conflicted)",
            Style::default().fg(theme.error),
        ));
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

pub(crate) fn render_tag_remote_target(
    target: Option<&crate::dag::TagRemoteTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    spans.push(Span::styled(
        format!("    @{}", target.remote),
        Style::default().fg(theme.remote),
    ));

    if !target.is_tracked {
        spans.push(Span::styled(
            " (untracked)",
            Style::default().fg(theme.muted),
        ));
    }
    spans.push(Span::styled(": ", Style::default().fg(theme.muted)));

    // A remote-only target's commit may not be in the repo, leaving no change ID.
    if !target.summary.change_id.display().is_empty() {
        push_short_id(
            &mut spans,
            &target.summary.change_id,
            theme.change_id,
            theme,
        );
        spans.push(Span::raw(" "));
    }
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

#[cfg(test)]
mod tests {
    use super::render_tag_item;
    use crate::app::TagViewEntry;
    use crate::dag::TagPresence;
    use crate::theme::Theme;
    use crate::types::TagName;

    fn rendered(is_conflicted: bool) -> String {
        let entry = TagViewEntry {
            name: TagName::new("v1"),
            commit_id: None,
            change_id: None,
            short_commit_id: None,
            revision: None,
            description: None,
            presence: TagPresence::Local,
            is_conflicted,
        };
        render_tag_item(&entry, None, &Theme::default())
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_conflicted_tag_is_marked_as_such() {
        assert!(rendered(true).ends_with("v1 (conflicted)"));
        assert!(rendered(false).ends_with("v1"));
    }
}
