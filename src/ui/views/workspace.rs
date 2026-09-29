use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::dag::WorkspaceInfo;
use crate::theme::Theme;
use crate::types::SearchScopes;

use super::push_ref_entry_suffix;
use crate::ui::search::{SearchRender, gutter_span, push_searchable};

pub(crate) fn render_workspace_item(
    entry: &WorkspaceInfo,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    let name_style = if entry.is_current {
        Style::default()
            .fg(theme.workspace)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.workspace)
    };
    push_searchable(
        &mut spans,
        entry.name.as_str(),
        SearchScopes::DESCRIPTION,
        name_style,
        search,
    );
    if entry.is_current {
        spans.push(Span::styled(" (current)", Style::default().fg(theme.muted)));
    }

    push_ref_entry_suffix(
        &mut spans,
        entry.change_id.as_ref(),
        entry.description.as_deref(),
        search,
        theme,
    );

    vec![Line::from(spans)]
}
