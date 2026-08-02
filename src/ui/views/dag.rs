use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::conflict::{ConflictHunkKind, ConflictPick, ConflictTermKind};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, LineStats};
use crate::idx::{ConflictLineIdx, ConflictTermIdx};
use crate::theme::{Config, Theme};
use crate::types::{ConflictHunkRef, FileSelectionState, SearchScopes};

use super::{RenderFlags, push_diff_tokens, push_graph_node_spans, push_line_stats};
use crate::ui::search::{
    SearchRender, SearchRowState, contains_query, gutter_span, push_searchable, search_gutter,
};
use crate::ui::spans::{push_highlighted_short_id, push_short_id};

pub(crate) fn render_commit_item(
    graph_node: &str,
    graph_cont: &str,
    c: &CommitInfo,
    line_stats: Option<LineStats>,
    flags: &RenderFlags,
    search: Option<&SearchRender<'_>>,
    config: &Config,
) -> Vec<Line<'static>> {
    let theme = &config.theme;
    let graph_color = if flags.is_source {
        theme.selection
    } else if c.has_conflict {
        theme.error
    } else {
        match c.glyph() {
            crate::theme::Glyph::WorkingCopy => theme.added,
            crate::theme::Glyph::Conflict => theme.error,
            crate::theme::Glyph::Immutable | crate::theme::Glyph::Merge => theme.accent,
            crate::theme::Glyph::Normal => theme.accent,
        }
    };

    let graph_style = Style::default().fg(graph_color);
    let search_state = search.map(|s| s.row_state).unwrap_or(SearchRowState::None);

    let mut line1: Vec<Span<'static>> = Vec::new();
    line1.push(search_gutter(search_state, theme));

    line1.push(if flags.in_visual {
        Span::styled("│", Style::default().fg(theme.accent))
    } else {
        Span::raw(" ")
    });
    if flags.is_selected {
        line1.push(Span::styled("▎", Style::default().fg(theme.selection)));
    } else if flags.is_source {
        line1.push(Span::styled("►", Style::default().fg(theme.selection)));
    } else {
        line1.push(Span::raw(" "));
    }

    push_graph_node_spans(&mut line1, graph_node, graph_style, config);

    let change_color = if c.is_divergent() {
        theme.error
    } else if c.is_hidden() {
        theme.text
    } else {
        theme.change_id
    };
    let change_id_text = if let Some(suffix) = c.change_id_suffix() {
        format!("{}/{}", c.change_id.display(), suffix)
    } else {
        c.change_id.display().to_string()
    };
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(&change_id_text, search.query_lower, search.case_sensitive)
        {
            push_highlighted_short_id(
                &mut line1,
                &c.change_id,
                c.change_id_suffix().map(|s| format!("/{s}")),
                change_color,
                search.query_lower,
                search.case_sensitive,
                theme,
            );
        } else {
            push_short_id(&mut line1, &c.change_id, change_color, theme);
            if let Some(suffix) = c.change_id_suffix() {
                line1.push(Span::styled(
                    format!("/{suffix}"),
                    Style::default().fg(change_color),
                ));
            }
        }
    } else {
        push_short_id(&mut line1, &c.change_id, change_color, theme);
        if let Some(suffix) = c.change_id_suffix() {
            line1.push(Span::styled(
                format!("/{suffix}"),
                Style::default().fg(change_color),
            ));
        }
    }
    if c.is_divergent() {
        line1.push(Span::styled(
            " (divergent)",
            Style::default().fg(theme.error),
        ));
    }
    line1.push(Span::raw(" "));

    push_searchable(
        &mut line1,
        &c.author.email,
        SearchScopes::AUTHOR,
        Style::default().fg(theme.user),
        search,
    );
    line1.push(Span::raw(" "));

    let tz =
        jiff::tz::Offset::from_seconds(c.author.tz_offset_seconds).unwrap_or(jiff::tz::Offset::UTC);
    let formatted = c
        .author
        .timestamp
        .to_zoned(jiff::tz::TimeZone::fixed(tz))
        .strftime(&config.date_format)
        .to_string();
    line1.push(Span::styled(formatted, Style::default().fg(theme.muted)));

    line1.push(Span::raw(" "));
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::COMMIT_ID)
            && contains_query(
                c.commit_id.display(),
                search.query_lower,
                search.case_sensitive,
            )
        {
            push_highlighted_short_id(
                &mut line1,
                &c.commit_id,
                None,
                theme.commit_id,
                search.query_lower,
                search.case_sensitive,
                theme,
            );
        } else {
            push_short_id(&mut line1, &c.commit_id, theme.commit_id, theme);
        }
    } else {
        push_short_id(&mut line1, &c.commit_id, theme.commit_id, theme);
    }

    let bm_style = Style::default()
        .fg(theme.bookmark)
        .add_modifier(Modifier::BOLD);
    for bm in &c.bookmarks {
        line1.push(Span::raw(" "));
        let display = if bm.is_dirty {
            format!("{}*", bm.name)
        } else {
            bm.name.to_string()
        };
        push_searchable(
            &mut line1,
            &display,
            SearchScopes::BOOKMARK,
            bm_style,
            search,
        );
    }

    let remote_bm_style = Style::default().fg(theme.bookmark);
    for rb in &c.remote_bookmarks {
        line1.push(Span::raw(" "));
        let text = format!("{}@{}", rb.name, rb.remote);
        push_searchable(
            &mut line1,
            &text,
            SearchScopes::BOOKMARK,
            remote_bm_style,
            search,
        );
    }

    let tag_style = Style::default().fg(theme.tag).add_modifier(Modifier::BOLD);
    for tag in &c.tags {
        line1.push(Span::raw(" "));
        push_searchable(
            &mut line1,
            tag.as_str(),
            SearchScopes::TAG,
            tag_style,
            search,
        );
    }

    for ws in &c.workspaces {
        if !ws.is_current {
            line1.push(Span::raw(" "));
            line1.push(Span::styled(
                format!("{}@", ws.name),
                Style::default()
                    .fg(theme.workspace)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    if let Some(stats) = line_stats {
        push_line_stats(&mut line1, stats, false, theme);
    }

    if c.is_hidden() {
        line1.push(Span::styled(" (hidden)", Style::default().fg(theme.text)));
    }

    let mut line2: Vec<Span<'static>> = Vec::new();
    line2.push(search_gutter(search_state, theme));
    line2.push(if flags.in_visual {
        Span::styled("│", Style::default().fg(theme.accent))
    } else {
        Span::raw(" ")
    });
    line2.push(if flags.is_selected {
        Span::styled("▎", Style::default().fg(theme.selection))
    } else {
        Span::raw(" ")
    });

    line2.push(Span::styled(
        graph_cont.to_string(),
        Style::default().fg(theme.muted),
    ));

    if c.has_conflict {
        line2.push(Span::styled(
            "(conflict) ",
            Style::default().fg(theme.error),
        ));
    }
    if let Some(desc) = &c.description {
        if c.is_empty {
            line2.push(Span::styled("(empty) ", Style::default().fg(theme.muted)));
        }
        let desc_style = if c.is_empty {
            Style::default().fg(theme.muted)
        } else {
            Style::default().fg(theme.text)
        };
        push_searchable(
            &mut line2,
            desc,
            SearchScopes::DESCRIPTION,
            desc_style,
            search,
        );
    } else {
        let placeholder = if c.is_empty {
            "(empty)"
        } else {
            "(no description set)"
        };
        let placeholder_style = if !c.is_working_copy() && !c.is_empty {
            Style::default().fg(theme.warning)
        } else {
            Style::default().fg(theme.muted)
        };
        line2.push(Span::styled(placeholder, placeholder_style));
    }

    vec![Line::from(line1), Line::from(line2)]
}

pub(crate) fn render_file_line(
    file: &FileChange,
    is_unfolded: bool,
    sel_state: FileSelectionState,
    in_visual: bool,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (marker, color) = if file.has_conflict {
        ("C", theme.error)
    } else {
        match file.status {
            FileStatus::Added => ("A", theme.added),
            FileStatus::Modified => ("M", theme.accent),
            FileStatus::Deleted => ("D", theme.error),
            FileStatus::Renamed => ("R", theme.accent),
            FileStatus::Copied => ("C", theme.added),
            FileStatus::Error => ("E", theme.error),
        }
    };

    let fold_char = if is_unfolded { "▾" } else { "▸" };
    let select_char = match sel_state {
        FileSelectionState::Full => "●",
        FileSelectionState::Partial => "○",
        FileSelectionState::None => " ",
    };

    let mut spans = vec![gutter_span(search, theme)];
    spans.push(if in_visual {
        Span::styled("│", Style::default().fg(theme.accent))
    } else {
        Span::raw(" ")
    });
    spans.extend(vec![
        Span::styled(
            format!(" {select_char} "),
            Style::default().fg(theme.selection),
        ),
        Span::styled(fold_char, Style::default().fg(theme.muted)),
        Span::raw(" "),
        Span::styled(
            marker,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    if let Some(old_path) = &file.old_path {
        let (prefix, old_mid, new_mid, suffix) =
            diff_path_parts(old_path.as_str(), file.path.as_str());
        let muted = Style::default().fg(theme.muted);
        let text = Style::default().fg(theme.text);
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix.to_string(), text));
        }
        spans.push(Span::styled("{", muted));
        spans.push(Span::styled(old_mid.to_string(), muted));
        spans.push(Span::styled(" → ", muted));
        push_searchable(
            &mut spans,
            new_mid,
            SearchScopes::PATH_COMMAND,
            text,
            search,
        );
        spans.push(Span::styled("}", muted));
        if !suffix.is_empty() {
            spans.push(Span::styled(suffix.to_string(), text));
        }
    } else {
        push_searchable(
            &mut spans,
            file.path.as_str(),
            SearchScopes::PATH_COMMAND,
            Style::default().fg(theme.text),
            search,
        );
    }
    if file.is_conflict_resolution() {
        spans.push(Span::styled(
            " (resolved)",
            Style::default().fg(theme.added),
        ));
    }
    push_line_stats(&mut spans, file.stats, true, theme);
    vec![Line::from(spans)]
}

pub(crate) fn render_diff_line(
    diff_line: &DiffLine,
    show_line_numbers: bool,
    diff_underline: bool,
    flags: &RenderFlags,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
    tab_str: &str,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];
    let is_selectable = diff_line.is_selectable();
    if is_selectable {
        let bar = if flags.in_visual { "│" } else { " " };
        let sel = if flags.is_selected { "▎" } else { " " };
        spans.push(Span::styled(bar, Style::default().fg(theme.accent)));
        spans.push(Span::styled(sel, Style::default().fg(theme.selection)));
    } else {
        spans.push(Span::raw("  "));
    }

    let (marker, mut style) = match diff_line.kind {
        DiffLineKind::Header => (" ", Style::default().fg(theme.accent)),
        DiffLineKind::Context => (" ", Style::default().fg(theme.muted)),
        DiffLineKind::Added => ("+", Style::default().fg(theme.added)),
        DiffLineKind::Removed => ("-", Style::default().fg(theme.error)),
    };
    if diff_line.conflict_region {
        style = style.add_modifier(Modifier::DIM);
    }

    let line_num_style = Style::default().fg(theme.muted);
    if show_line_numbers && diff_line.kind != DiffLineKind::Header {
        let nums = format_line_numbers(diff_line.old_line, diff_line.new_line);
        spans.push(Span::styled(nums, line_num_style));
        spans.push(Span::styled(marker, style));
        super::push_diff_tokens(
            &mut spans,
            &diff_line.content,
            &diff_line.tokens,
            style,
            diff_underline,
            search,
            theme,
            tab_str,
        );
    } else {
        let prefix = match diff_line.kind {
            DiffLineKind::Header => "      ",
            DiffLineKind::Context => "       ",
            DiffLineKind::Added => "      +",
            DiffLineKind::Removed => "      -",
        };
        spans.push(Span::styled(prefix, style));
        super::push_diff_tokens(
            &mut spans,
            &diff_line.content,
            &diff_line.tokens,
            style,
            diff_underline,
            search,
            theme,
            tab_str,
        );
    }

    vec![Line::from(spans)]
}

fn format_line_numbers(old: Option<u32>, new: Option<u32>) -> String {
    match (old, new) {
        (Some(o), Some(n)) => format!("  {o:>4} {n:>4} "),
        (Some(o), None) => format!("  {o:>4}      "),
        (None, Some(n)) => format!("       {n:>4} "),
        (None, None) => "              ".to_string(),
    }
}

fn diff_path_parts<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str, &'a str, &'a str) {
    let shared_pre = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let pre = old[..shared_pre].rfind('/').map(|i| i + 1).unwrap_or(0);

    let old_rest = &old[pre..];
    let new_rest = &new[pre..];
    let shared_suf = old_rest
        .bytes()
        .rev()
        .zip(new_rest.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let suf = old_rest[old_rest.len() - shared_suf..]
        .find('/')
        .map(|i| old_rest.len() - shared_suf + i)
        .unwrap_or(old_rest.len());

    (
        &old[..pre],
        &old[pre..pre + suf],
        &new[pre..pre + (new_rest.len() - (old_rest.len() - suf))],
        &old[pre + suf..],
    )
}

pub(crate) fn render_conflict_header(
    app: &App,
    hunk: ConflictHunkRef,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let hunks = app.conflict_hunks_loaded(hunk.entry_idx, hunk.file_idx);
    let (num, total) = hunks
        .map(|hunks| {
            let mut num = 0usize;
            let mut total = 0usize;
            for (i, h) in hunks.iter().enumerate() {
                if matches!(h, ConflictHunkKind::Conflict { .. }) {
                    total += 1;
                    if i <= hunk.hunk_idx.raw() {
                        num += 1;
                    }
                }
            }
            (num, total)
        })
        .unwrap_or((0, 0));
    // The label color tracks the block it names: term picks render green
    // ([ours]/[theirs]/[base] are added-green), a hand edit renders in the
    // selection color like the [edited] block.
    let picked = app.hunk_pick(hunk).map(|pick| match pick {
        ConflictPick::Term(kind) => {
            let sides = app.conflict_hunk(hunk).map_or(0, |h| h.num_sides());
            (kind.label(sides), theme.added)
        }
        ConflictPick::Edited(_) => ("edited".to_string(), theme.selection),
    });
    let bold_error = Style::default()
        .fg(theme.error)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::raw("        "),
        Span::styled(format!("── conflict {num} of {total}"), bold_error),
    ];
    if let Some((label, color)) = picked {
        spans.push(Span::styled(
            format!(" · picked: {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(" ──", bold_error));
    vec![Line::from(spans)]
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_conflict_term(
    app: &App,
    hunk: ConflictHunkRef,
    term_idx: ConflictTermIdx,
    line_idx: ConflictLineIdx,
    diff_underline: bool,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
    tab_str: &str,
) -> Vec<Line<'static>> {
    // A term line is either real content (with word tokens) or an italic
    // note (folded-base stub, deleted/empty placeholder).
    enum TermLine {
        Text(String, Vec<crate::dag::DiffToken>),
        Note(String),
    }
    let base_folded = app.hunk_base_folded(hunk);
    let picked_term = app.hunk_picked_term(hunk);
    let info = app.conflict_hunk(hunk).and_then(|h| match h {
        ConflictHunkKind::Conflict { terms } => {
            let term = terms.get(term_idx.raw())?;
            let line = if !term.kind.is_side() && base_folded {
                let n = term.text.lines.len();
                TermLine::Note(if term.absent {
                    "(file deleted)".to_string()
                } else {
                    format!("… {n} {} (tab)", crate::pluralize!(n, "line", "lines"))
                })
            } else {
                match term.text.lines.get(line_idx.raw()) {
                    Some(l) => TermLine::Text(
                        l.clone(),
                        term.token_lines
                            .get(line_idx.raw())
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    None if term.absent => TermLine::Note("(file deleted)".to_string()),
                    None => TermLine::Note("(empty)".to_string()),
                }
            };
            let is_selected = picked_term == Some(term.kind);
            Some((line, is_selected, term.kind, h.num_sides()))
        }
        _ => None,
    });
    let Some((line, is_selected, kind, num_sides)) = info else {
        return vec![Line::raw("")];
    };
    let term_color = match kind {
        ConflictTermKind::Side(0) => theme.added,
        ConflictTermKind::Side(_) => theme.change_id,
        ConflictTermKind::Base(_) => theme.muted,
    };
    let mut base_style = Style::default().fg(term_color);
    if is_selected {
        base_style = base_style.add_modifier(Modifier::BOLD);
    }
    let label_style = if is_selected {
        base_style.add_modifier(Modifier::UNDERLINED)
    } else {
        base_style
    };
    let label = format!("[{}]", kind.label(num_sides));
    let mut spans = vec![
        Span::raw("          "),
        Span::styled(format!("{label:<8}"), label_style),
        Span::raw(" "),
    ];
    match line {
        TermLine::Note(text) => {
            spans.push(Span::styled(
                text,
                base_style.add_modifier(Modifier::ITALIC),
            ));
        }
        TermLine::Text(content, tokens) => {
            push_diff_tokens(
                &mut spans,
                &content,
                &tokens,
                base_style,
                diff_underline,
                search,
                theme,
                tab_str,
            );
        }
    }
    vec![Line::from(spans)]
}

pub(crate) fn render_conflict_context(
    app: &App,
    hunk: ConflictHunkRef,
    line_idx: ConflictLineIdx,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let text = app
        .conflict_hunk(hunk)
        .and_then(|h| match h {
            ConflictHunkKind::Resolved { text } => text.lines.get(line_idx.raw()).cloned(),
            _ => None,
        })
        .unwrap_or_default();
    vec![Line::from(vec![
        Span::raw("        "),
        Span::styled(text, Style::default().fg(theme.muted)),
    ])]
}

pub(crate) fn render_conflict_gap(
    app: &App,
    hunk: ConflictHunkRef,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let hidden = app
        .hunk_trimmed_context(hunk)
        .map(|trim| trim.hidden)
        .unwrap_or_default();
    vec![Line::from(vec![
        Span::raw("        "),
        Span::styled(
            format!("── … {hidden} lines … (tab) ──"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ),
    ])]
}

pub(crate) fn render_conflict_edited(
    app: &App,
    hunk: ConflictHunkRef,
    line_idx: ConflictLineIdx,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let line = match app.hunk_pick(hunk) {
        Some(ConflictPick::Edited(text)) => Some(text.lines.get(line_idx.raw()).cloned()),
        _ => None,
    };
    // Selection color: an edited block is the user's chosen content and
    // must not read as a side ([ours] is added-green).
    let style = Style::default()
        .fg(theme.selection)
        .add_modifier(Modifier::BOLD);
    let (text, style) = match line {
        Some(Some(text)) => (text, style),
        // Placeholder for an edit that resolved to nothing.
        Some(None) => ("(empty)".to_string(), style.add_modifier(Modifier::ITALIC)),
        None => (String::new(), style),
    };
    vec![Line::from(vec![
        Span::raw("          "),
        Span::styled(format!("{:<8}", "[edited]"), style),
        Span::raw(" "),
        Span::styled(text, style),
    ])]
}
