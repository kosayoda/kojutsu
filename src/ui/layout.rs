use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

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

/// How much of the status border the toggles take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToggleForm {
    /// ` [I] ignore-immutable `.
    Full,
    /// ` [I] `, lit while on.
    Short,
    Hidden,
}

/// What goes on the status border. Built in full, then laid out by
/// [`border_line`] to whatever fits.
struct BorderParts<'a> {
    /// The view's name, with the activity spinner: always shown.
    title: Span<'a>,
    toggles_full: Vec<Span<'a>>,
    toggles_short: Vec<Span<'a>>,
    selection: Option<Span<'a>>,
    /// Things wrong with the view: `@` out of it, conflicts.
    warnings: Vec<Span<'a>>,
    /// Right-aligned.
    help: Span<'a>,
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// The status border at `width` columns: parts joined by `─`, the help
/// hint at the right. When everything doesn't fit, parts give way in order
/// of how much they matter: the toggles shrink to their keys and then go,
/// then the selection, the warnings, and the help hint. The title stays.
fn border_line<'a>(parts: BorderParts<'a>, width: usize, rule: Style) -> Line<'a> {
    // (toggles, selection, warnings, help), richest first.
    const LAYOUTS: [(ToggleForm, bool, bool, bool); 6] = [
        (ToggleForm::Full, true, true, true),
        (ToggleForm::Short, true, true, true),
        (ToggleForm::Hidden, true, true, true),
        (ToggleForm::Hidden, false, true, true),
        (ToggleForm::Hidden, false, false, true),
        (ToggleForm::Hidden, false, false, false),
    ];
    let left_groups = |(toggles, selection, warnings, _): (ToggleForm, bool, bool, bool)| {
        let mut groups: Vec<Vec<Span<'a>>> = vec![vec![parts.title.clone()]];
        match toggles {
            ToggleForm::Full => groups.push(parts.toggles_full.clone()),
            ToggleForm::Short => groups.push(parts.toggles_short.clone()),
            ToggleForm::Hidden => {}
        }
        if selection && let Some(span) = &parts.selection {
            groups.push(vec![span.clone()]);
        }
        if warnings && !parts.warnings.is_empty() {
            groups.push(parts.warnings.clone());
        }
        groups
    };
    // `─` before each group and once more before the help hint, which a
    // `─` closes off.
    let needed = |groups: &[Vec<Span<'a>>], help: bool| {
        groups.iter().map(|g| 1 + spans_width(g)).sum::<usize>()
            + if help { 1 + parts.help.width() + 1 } else { 0 }
    };
    let (groups, help) = LAYOUTS
        .iter()
        .map(|&layout| (left_groups(layout), layout.3))
        .find(|(groups, help)| needed(groups, *help) <= width)
        .unwrap_or_else(|| (vec![vec![parts.title.clone()]], false));

    let mut spans: Vec<Span<'a>> = Vec::new();
    for group in groups {
        spans.push(Span::styled("─", rule));
        spans.extend(group);
    }
    let used = spans_width(&spans) + if help { parts.help.width() + 1 } else { 0 };
    spans.push(Span::styled("─".repeat(width.saturating_sub(used)), rule));
    if help {
        spans.push(parts.help.clone());
        spans.push(Span::styled("─", rule));
    }
    Line::from(spans)
}

pub(super) fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let toggle_style = |active: bool| {
        if active {
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        }
    };
    // The badges as one group, a space between and around them.
    let toggle_group = |text: &dyn Fn(&crate::types::GlobalToggle) -> String| {
        let mut spans = vec![Span::raw(" ")];
        for (i, toggle) in GLOBAL_TOGGLES.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                text(toggle),
                toggle_style(app.toggles.contains(toggle.flag)),
            ));
        }
        spans.push(Span::raw(" "));
        spans
    };
    let toggles_full = toggle_group(&|toggle| format!("[{}] {}", toggle.hint, toggle.label));
    let toggles_short = toggle_group(&|toggle| format!("[{}]", toggle.hint));

    let selection = app.selection.display_text().map(|text| {
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(theme.selection)
                .add_modifier(Modifier::BOLD),
        )
    });

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
    let indicator = app.activity_indicator(std::time::Instant::now());
    let view_title = match indicator.as_ref().and_then(|i| i.glyph) {
        Some(glyph) => format!("{}{glyph} ", view_title).into(),
        None => view_title,
    };

    let mut warnings: Vec<Span> = Vec::new();
    if !app.has_working_copy() {
        warnings.push(Span::styled(
            " @ not visible ",
            Style::default().fg(theme.warning),
        ));
    }
    let conflicted = app.conflicted_commit_count();
    if conflicted > 0 {
        let noun = crate::pluralize!(conflicted, "conflict", "conflicts");
        warnings.push(Span::styled(
            format!(" {conflicted} {noun} "),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let parts = BorderParts {
        title: Span::styled(
            view_title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        toggles_full,
        toggles_short,
        selection,
        warnings,
        help: Span::styled(" ? Help ", Style::default().fg(theme.text)),
    };
    let rule = Style::default().fg(theme.muted);
    let border = Rect { height: 1, ..area };
    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Paragraph::new(border_line(parts, area.width as usize, rule)),
        border,
    );

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
    } else if let Some(label) = indicator.and_then(|i| i.label) {
        // Only on an otherwise empty line: a message or a search matters
        // more than what is loading, which the spinner still shows.
        (label, theme.muted)
    } else {
        (String::new(), theme.text)
    };
    let line = Line::from(Span::styled(content, Style::default().fg(color)));
    frame.render_widget(Paragraph::new(line), inner);
}

#[cfg(test)]
mod status_bar_tests {
    use std::time::{Duration, Instant};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::draw_status_bar;
    use crate::app::{App, Loadable};
    use crate::theme::Theme;

    /// The status bar's two lines, as drawn `width` columns wide.
    fn status_bar(app: &App, width: u16) -> [String; 2] {
        let mut terminal = Terminal::new(TestBackend::new(width, 2)).expect("terminal");
        terminal
            .draw(|frame| draw_status_bar(frame, frame.area(), app, &Theme::default()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        [0, 1].map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
    }

    fn busy_app() -> App {
        let mut app = App::for_test();
        app.revset.load_state = Loadable::Loading;
        // Long enough ago that the indicator is due.
        app.track_activity(Instant::now() - Duration::from_secs(1));
        app
    }

    #[test]
    fn the_spinner_sits_by_the_view_title_and_the_label_below() {
        let [border, line] = status_bar(&busy_app(), 100);
        let spinner = &busy_app().config.spinner;
        let after_title = border
            .split(" Log ")
            .nth(1)
            .and_then(|rest| rest.chars().next())
            .expect("a view title");
        assert!(spinner.contains(&after_title), "{border}");
        assert_eq!(line, "loading commits…");
    }

    /// The view title and its spinner stay when everything else on the
    /// border gives way.
    #[test]
    fn a_narrow_terminal_keeps_the_spinner() {
        let [border, _] = status_bar(&busy_app(), 20);
        assert!(border.contains(" Log "), "{border}");
        assert!(
            busy_app()
                .config
                .spinner
                .iter()
                .any(|frame| border.contains(*frame)),
            "{border}"
        );
    }

    /// A commit off `@`, selected: every part of the border has something
    /// to show.
    fn crowded_app() -> App {
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.toggle_commit_selection(crate::idx::EntryIdx::new(0));
        app
    }

    /// At any width the border fills it exactly, the title first and the
    /// help hint, when there's room for it, closing it off: parts never run
    /// into each other.
    #[test]
    fn the_border_fills_any_width_without_overlap() {
        let app = crowded_app();
        for width in 1..=160u16 {
            let mut terminal = Terminal::new(TestBackend::new(width, 2)).expect("terminal");
            terminal
                .draw(|frame| draw_status_bar(frame, frame.area(), &app, &Theme::default()))
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let border: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
            if width >= 6 {
                assert!(border.starts_with("─ Log "), "{width}: {border}");
            }
            if border.contains("? Help") {
                assert!(border.ends_with(" ? Help ─"), "{width}: {border}");
            }
        }
    }

    fn border_at(width: u16) -> String {
        let [border, _] = status_bar(&crowded_app(), width);
        border
    }

    /// Room runs out: the toggles shrink to their keys, then go, then the
    /// selection, the warning, and the help hint, in that order.
    #[test]
    fn parts_give_way_in_order() {
        let full = border_at(140);
        assert!(full.contains("[I] ignore-immutable [W] ignore-working-copy [D] debug"));
        assert!(full.contains("1 commit selected") && full.contains("@ not visible"));

        let short = border_at(80);
        assert!(short.contains("─ [I] [W] [D] ─"), "{short}");
        assert!(short.contains("1 commit selected") && short.contains("? Help"));

        let no_toggles = border_at(55);
        assert!(!no_toggles.contains("[I]"), "{no_toggles}");
        assert!(no_toggles.contains("1 commit selected") && no_toggles.contains("@ not visible"));

        let no_selection = border_at(40);
        assert!(!no_selection.contains("selected"), "{no_selection}");
        assert!(no_selection.contains("@ not visible") && no_selection.contains("? Help"));

        let help_only = border_at(20);
        assert!(!help_only.contains("@ not visible") && help_only.contains("? Help"));

        let title_only = border_at(12);
        assert!(title_only.starts_with("─ Log") && !title_only.contains("Help"));
    }

    /// A message outranks what is loading; the spinner still shows.
    #[test]
    fn a_message_takes_the_line() {
        let mut app = busy_app();
        app.set_error("something broke");
        let [_, line] = status_bar(&app, 100);
        assert_eq!(line, "something broke");
    }
}
