use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::spans::push_short_id;
use crate::app::{App, StatusLevel};
use crate::dag::ShortId;
use crate::theme::Theme;
use crate::types::{ActiveView, GLOBAL_TOGGLES};

/// Minimum separator between repo and revset when on a single line.
const HEADER_SEP: &str = "  ";

/// The header's lines for the current view. The layout sizes the header
/// from the number of lines, so what is drawn and the space it gets can't
/// disagree.
pub(super) fn header_lines<'a>(app: &'a App, theme: &Theme, width: u16) -> Vec<Line<'a>> {
    let muted = Style::default().fg(theme.muted);
    let repo = [
        Span::styled("repository: ", muted),
        Span::styled(app.repo_root.as_str(), Style::default().fg(theme.text)),
    ];
    let revset_label = match app
        .revset
        .active_preset
        .and_then(|i| app.config.revsets.presets.get(i))
    {
        Some(preset) => format!("revset ({}): ", preset.name),
        None => "revset: ".to_string(),
    };
    let revset = [
        Span::styled(revset_label, muted),
        Span::styled(
            app.revset.current.as_str(),
            Style::default().fg(theme.accent),
        ),
    ];

    let spans_width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let mut header =
        if spans_width(&repo) + HEADER_SEP.len() + spans_width(&revset) <= width as usize {
            let mut spans = repo.to_vec();
            spans.push(Span::raw(HEADER_SEP));
            spans.extend(revset);
            vec![Line::from(spans)]
        } else {
            vec![Line::from(repo.to_vec()), Line::from(revset.to_vec())]
        };

    match app.active_view {
        ActiveView::Operations if !app.op_log.workspace_filter.is_empty() => {
            header.push(workspace_filter_line(app, theme));
        }
        ActiveView::Annotate => header.extend(annotate_header_lines(app, theme)),
        _ => {}
    }
    header
}

fn workspace_filter_line(app: &App, theme: &Theme) -> Line<'static> {
    let mut names: Vec<&str> = app
        .op_log
        .workspace_filter
        .iter()
        .map(|s| s.as_str())
        .collect();
    names.sort_unstable();
    Line::from(vec![
        Span::styled("workspace: ", Style::default().fg(theme.muted)),
        Span::styled(
            names.join(", "),
            Style::default()
                .fg(theme.workspace)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// The annotated file and its commit, then a breadcrumb trail when the
/// user has walked back through history.
fn annotate_header_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted);
    let mut lines = Vec::new();
    let Some(target) = &app.annotate.target else {
        return lines;
    };

    let mut spans = vec![
        Span::styled("annotate: ", muted),
        Span::styled(
            target.path.as_str().to_string(),
            Style::default().fg(theme.text),
        ),
    ];
    if let Some(info) = app.annotate.commit_info.get(&target.commit_id) {
        spans.push(Span::raw("  "));
        push_short_id(&mut spans, &info.change_id, theme.change_id, theme);
        spans.push(Span::raw("  "));
        push_short_id(&mut spans, &info.commit_id, theme.commit_id, theme);
        spans.push(Span::styled(
            format!("  {} <{}>", info.author_name, info.author_email),
            muted,
        ));
        spans.push(Span::styled(format!("  {}", info.author_date), muted));
        if let Some(first_line) = info.description_lines.first() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                first_line.clone(),
                Style::default().fg(theme.text),
            ));
        }
        let depth = app.annotate.history.len();
        if depth > 0 {
            spans.push(Span::styled(format!("  [depth {depth}]"), muted));
        }
    } else {
        // Not loaded yet: fall back to the raw commit id.
        let cid = target.commit_id.as_str();
        spans.push(Span::styled(" @ ", muted));
        spans.push(Span::styled(
            cid.get(..12).unwrap_or(cid).to_string(),
            Style::default().fg(theme.commit_id),
        ));
    }
    lines.push(Line::from(spans));

    if !app.annotate.history.is_empty() {
        let mut crumbs = Vec::new();
        for (hist_cid, _) in &app.annotate.history {
            match app.annotate.commit_info.get(hist_cid) {
                Some(info) => push_crumb(&mut crumbs, &info.change_id, theme),
                None => {
                    let cid = hist_cid.as_str();
                    crumbs.push(Span::styled(cid.get(..8).unwrap_or(cid).to_string(), muted));
                }
            }
            crumbs.push(Span::styled(" → ", muted));
        }
        if let Some(info) = app.annotate.commit_info.get(&target.commit_id) {
            push_crumb(&mut crumbs, &info.change_id, theme);
        }
        crumbs.push(Span::styled(" (current)", muted));
        lines.push(Line::from(crumbs));
    }
    lines
}

fn push_crumb(spans: &mut Vec<Span<'static>>, change_id: &ShortId, theme: &Theme) {
    let (prefix, rest) = change_id.split();
    spans.push(Span::styled(
        prefix.to_string(),
        Style::default()
            .fg(theme.change_id)
            .add_modifier(Modifier::BOLD),
    ));
    if !rest.is_empty() {
        spans.push(Span::styled(
            rest.to_string(),
            Style::default().fg(theme.muted),
        ));
    }
}

pub(super) fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Build toggle indicators for the title bar.
    let mut toggle_spans: Vec<Span> = Vec::new();
    for toggle in GLOBAL_TOGGLES {
        let active = app.toggles.contains(toggle.flag);
        let style = if active {
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        toggle_spans.push(Span::styled(
            format!(" [{}] {} ", toggle.hint, toggle.label),
            style,
        ));
    }

    if let Some(text) = app.selection.display_text() {
        toggle_spans.push(Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let view_title: std::borrow::Cow<'static, str> = match app.active_view {
        crate::types::ActiveView::Dag => " Log ".into(),
        crate::types::ActiveView::Bookmarks => {
            format!(" Bookmarks ({}) ", app.views.bookmark_entries.len()).into()
        }
        crate::types::ActiveView::Tags => {
            format!(" Tags ({}) ", app.views.tag_entries.len()).into()
        }
        crate::types::ActiveView::Operations => " Operations ".into(),
        crate::types::ActiveView::Evolog => " Evolog ".into(),
        crate::types::ActiveView::Workspaces => " Workspaces ".into(),
        crate::types::ActiveView::CommandLog => " Command Log ".into(),
        crate::types::ActiveView::Interdiff => " Interdiff ".into(),
        crate::types::ActiveView::Annotate => " Annotate ".into(),
    };

    let mut wc_spans: Vec<Span> = Vec::new();
    if !app.has_working_copy() {
        wc_spans.push(Span::styled(
            " @ not visible ",
            Style::default().fg(theme.warning),
        ));
    }
    let conflicted = app.conflicted_commit_count();
    if conflicted > 0 {
        let noun = crate::pluralize!(conflicted, "conflict", "conflicts");
        wc_spans.push(Span::styled(
            format!(" {conflicted} {noun} "),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.muted))
        .title("")
        .title(view_title.as_ref())
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .title(Line::from(toggle_spans))
        .title(Line::from(wc_spans))
        .title(
            Line::from(Span::styled(" ? Help ", Style::default().fg(theme.text))).right_aligned(),
        )
        .title(Line::from("").right_aligned());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (content, color) = if let Some(search) = &app.search {
        let pos = search.current_match.map(|i| i + 1).unwrap_or(0);
        (
            format!(
                "search: {} ({}/{})",
                search.query(),
                pos,
                search.matches.len()
            ),
            theme.text,
        )
    } else if let Some((status, level)) = &app.status_message {
        let c = match level {
            StatusLevel::Info => theme.text,
            StatusLevel::Error => theme.error,
        };
        (status.clone(), c)
    } else {
        (String::new(), theme.text)
    };
    let line = Line::from(Span::styled(content, Style::default().fg(color)));
    frame.render_widget(Paragraph::new(line), inner);
}
