use itertools::Itertools;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use super::search::*;
use super::spans::*;
use std::collections::HashSet;

use crate::app::{
    App, AppMode, BookmarkViewEntry, EvoLogEntry, OpDetailLine, OpDiffKind, OpLogEntry,
    TagViewEntry, TargetMode, WorkspaceViewEntry,
};
use crate::dag::{
    CommitInfo, DiffLine, DiffLineKind, DiffTokenKind, FileChange, FileStatus, LineStats,
};
use crate::idx::EntryIdx;
use crate::theme::{Config, Theme};
use crate::types::ChangeId;
use crate::types::{DisplayRow, FileSelectionState, SearchScopes};

/// Push `+N -M` spans for line stats, skipping zeros.
/// When `muted` is true, both counts use `theme.muted` instead of green/red.
fn push_line_stats(out: &mut Vec<Span<'static>>, stats: LineStats, muted: bool, theme: &Theme) {
    if stats.added == 0 && stats.removed == 0 {
        return;
    }
    let added_color = if muted { theme.muted } else { theme.added };
    let removed_color = if muted { theme.muted } else { theme.error };
    out.push(Span::raw(" "));
    if stats.added > 0 {
        out.push(Span::styled(
            format!("+{}", stats.added),
            Style::default().fg(added_color),
        ));
        if stats.removed > 0 {
            out.push(Span::raw(" "));
        }
    }
    if stats.removed > 0 {
        out.push(Span::styled(
            format!("-{}", stats.removed),
            Style::default().fg(removed_color),
        ));
    }
}

/// Visual state flags for rendering a row.
struct RenderFlags {
    is_source: bool,
    is_selected: bool,
    in_visual: bool,
}

/// Expand tab characters to spaces.
fn expand_tabs(s: &str, tab_spaces: &str) -> String {
    if s.contains('\t') {
        s.replace('\t', tab_spaces)
    } else {
        s.to_string()
    }
}

pub(super) fn draw_list(frame: &mut Frame, area: Rect, app: &mut App, config: &Config) {
    let theme = &config.theme;
    let git_diff = app.toggles.contains(crate::keymap::CommandFlags::GIT_DIFF);
    let tab_spaces: String = " ".repeat(config.tab_width as usize);
    // If in target selection mode, get the source change_id for highlighting
    // and the set of marked targets (multi-select).
    let (target_select_source, target_marks): (Option<&str>, Option<&HashSet<ChangeId>>) =
        match &app.mode {
            AppMode::TargetSelect {
                source,
                target_mode: TargetMode::Multi { targets },
                ..
            } => (Some(source.as_str()), Some(targets)),
            AppMode::TargetSelect { source, .. } => (Some(source.as_str()), None),
            _ => (None, None),
        };

    let is_marked = |entry_idx: EntryIdx| -> bool {
        target_marks.map_or_else(
            || app.is_commit_selected(entry_idx),
            |marks| marks.contains(&app.nodes[entry_idx].commit.unique_change_id()),
        )
    };

    let search_case_sensitive = app
        .search
        .as_ref()
        .is_some_and(|s| s.query().chars().any(|c| c.is_ascii_uppercase()));
    let query_lowered = app.search.as_ref().map(|s| {
        if search_case_sensitive {
            s.query().to_string()
        } else {
            s.query().to_lowercase()
        }
    });
    let search_ctx = app.search.as_ref().map(|s| SearchRender {
        query_lower: query_lowered.as_deref().unwrap_or(""),
        scopes: s.scopes,
        case_sensitive: search_case_sensitive,
        row_state: SearchRowState::None,
    });

    // Only build full ListItems for rows near the visible window.
    // Off-screen rows get a cheap placeholder — ratatui's List still sees
    // the correct total item count for scroll math.
    let offset = app.list_state.offset();
    let vis_start = offset.min(app.cursor).saturating_sub(20);
    let vis_end = (offset.max(app.cursor) + area.height as usize + 20).min(app.rows.len());

    let max_w = area.width as usize;
    let raw_items: Vec<Vec<Line>> = app
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| -> Vec<Line<'static>> {
            if row_idx < vis_start || row_idx >= vis_end {
                return match row {
                    DisplayRow::CommitNode { .. }
                    | DisplayRow::OpLogItem { .. }
                    | DisplayRow::EvoLogItem { .. } => {
                        vec![Line::raw(""), Line::raw("")]
                    }
                    _ => vec![Line::raw("")],
                };
            }
            let row_search = search_ctx.as_ref().map(|ctx| SearchRender {
                row_state: search_row_state(app, row_idx),
                ..*ctx
            });
            match row {
                DisplayRow::CommitNode { entry_idx } => {
                    let node = &app.nodes[*entry_idx];
                    let gl = &node.graph;
                    let graph_node = gl.node.as_str();
                    let graph_cont = gl.cont.as_str();
                    let flags = RenderFlags {
                        is_source: target_select_source
                            .is_some_and(|src| src == node.commit.unique_prefix().as_str()),
                        is_selected: is_marked(*entry_idx),
                        in_visual: app.is_in_visual_commit_range(*entry_idx),
                    };
                    render_commit_item(
                        graph_node,
                        graph_cont,
                        &node.commit,
                        app.is_commit_unfolded(*entry_idx)
                            .then(|| app.commit_stats(*entry_idx))
                            .flatten(),
                        &flags,
                        row_search.as_ref(),
                        config,
                    )
                }
                DisplayRow::DescriptionLine {
                    entry_idx,
                    line_idx,
                } => {
                    let text = app.nodes[*entry_idx]
                        .commit
                        .full_description
                        .as_ref()
                        .and_then(|d| d.lines().nth(line_idx.raw() + 1))
                        .unwrap_or("");
                    let graph_cont = app.nodes[*entry_idx].graph.cont.as_str();
                    let selected = is_marked(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    let mut spans = vec![gutter_span(row_search.as_ref(), theme)];
                    // Visual + selection bars (same as line 2 of commit item).
                    spans.push(if in_visual {
                        Span::styled("│", Style::default().fg(theme.accent))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(if selected {
                        Span::styled("▎", Style::default().fg(theme.selection))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(Span::styled(
                        graph_cont.to_string(),
                        Style::default().fg(theme.muted),
                    ));
                    spans.push(Span::styled(
                        format!("  {text}"),
                        Style::default().fg(theme.muted),
                    ));
                    vec![Line::from(spans)]
                }
                DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                } => {
                    let graph_str = app.nodes[*entry_idx]
                        .graph
                        .extra
                        .get(line_idx.raw())
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let mut spans = vec![gutter_span(row_search.as_ref(), theme)];
                    // Continue visual + selection bars through graph links.
                    let selected = is_marked(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    spans.push(if in_visual {
                        Span::styled("│", Style::default().fg(theme.accent))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(if selected {
                        Span::styled("▎", Style::default().fg(theme.selection))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(Span::styled(
                        graph_str.to_string(),
                        Style::default().fg(theme.muted),
                    ));
                    vec![Line::from(spans)]
                }
                DisplayRow::FileChange {
                    entry_idx,
                    file_idx,
                } => {
                    let files = app
                        .files_for_entry(*entry_idx)
                        .expect("visible file row must be loaded");
                    let file = &files[file_idx.raw()];
                    let is_unfolded = app.is_file_unfolded(*entry_idx, *file_idx);
                    let sel_state = app.file_selection_state(*entry_idx, *file_idx);
                    render_file_line(file, is_unfolded, sel_state, row_search.as_ref(), theme)
                }
                DisplayRow::DiffLine {
                    entry_idx,
                    file_idx,
                    line_idx,
                } => {
                    let diff_lines = app
                        .diff_lines(*entry_idx, *file_idx)
                        .expect("visible diff row must be loaded");
                    let diff_line = &diff_lines[line_idx.raw()];
                    let flags = RenderFlags {
                        is_source: false,
                        is_selected: app.is_line_selected(*entry_idx, *file_idx, *line_idx),
                        in_visual: app.is_in_visual_range(*entry_idx, *file_idx, *line_idx),
                    };
                    render_diff_line(
                        diff_line,
                        app.show_line_numbers,
                        &flags,
                        row_search.as_ref(),
                        theme,
                        &tab_spaces,
                    )
                }
                DisplayRow::BookmarkItem { bookmark_idx } => render_bookmark_item(
                    &app.views.bookmark_entries[bookmark_idx.raw()],
                    row_search.as_ref(),
                    theme,
                ),
                DisplayRow::BookmarkConflictTarget {
                    bookmark_idx,
                    target_idx,
                } => {
                    let entry = &app.views.bookmark_entries[bookmark_idx.raw()];
                    let target = app
                        .views
                        .bookmark_details
                        .get(&entry.name)
                        .and_then(|d| d.conflict_targets.get(target_idx.raw()));
                    render_bookmark_conflict_target(target, theme)
                }
                DisplayRow::BookmarkRemoteTarget {
                    bookmark_idx,
                    target_idx,
                } => {
                    let entry = &app.views.bookmark_entries[bookmark_idx.raw()];
                    let target = app
                        .views
                        .bookmark_details
                        .get(&entry.name)
                        .and_then(|d| d.remote_targets.get(target_idx.raw()));
                    render_bookmark_remote_target(target, theme)
                }
                DisplayRow::TagItem { tag_idx } => {
                    if let Some(entry) = app.views.tag_entries.get(tag_idx.raw()) {
                        render_tag_item(entry, row_search.as_ref(), theme)
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::TagRemoteTarget {
                    tag_idx,
                    target_idx,
                } => {
                    let target = app
                        .views
                        .tag_entries
                        .get(tag_idx.raw())
                        .and_then(|entry| app.views.tag_details.get(&entry.name))
                        .and_then(|d| d.remote_targets.get(target_idx.raw()));
                    render_tag_remote_target(target, theme)
                }
                DisplayRow::OpLogItem { op_log_idx } => {
                    if let Some(entry) = app.op_log.entries.get(op_log_idx.raw()) {
                        render_op_log_item(entry, row_search.as_ref(), theme)
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::OpLogDetailLine {
                    op_log_idx,
                    line_idx,
                } => {
                    let detail = app
                        .op_log
                        .entries
                        .get(op_log_idx.raw())
                        .and_then(|entry| app.op_log.details.get(&entry.id))
                        .and_then(|l| l.loaded())
                        .and_then(|lines| lines.get(line_idx.raw()));
                    render_op_detail_line(detail, theme)
                }
                DisplayRow::OpLogGraphLink {
                    op_log_idx,
                    line_idx,
                } => render_simple_graph_link(
                    app.op_log
                        .entries
                        .get(op_log_idx.raw())
                        .and_then(|e| e.graph.extra.get(line_idx.raw())),
                    row_search.as_ref(),
                    theme,
                ),
                DisplayRow::OpLogLoadMore => vec![Line::from(vec![
                    Span::styled("  [Tab] ", Style::default().fg(theme.accent)),
                    Span::styled("Load more…", Style::default().fg(theme.muted)),
                ])],
                DisplayRow::EvoLogItem { evolog_idx } => {
                    if let Some(entry) = app.evolog.entries.get(evolog_idx.raw()) {
                        render_evolog_item(entry, row_search.as_ref(), config)
                    } else {
                        vec![Line::raw(""), Line::raw("")]
                    }
                }
                DisplayRow::EvoLogFileChange {
                    evolog_idx,
                    file_idx,
                } => {
                    let file = app
                        .evolog
                        .entries
                        .get(evolog_idx.raw())
                        .and_then(|e| app.evolog.files.get(&e.commit_id))
                        .and_then(|l| l.loaded())
                        .and_then(|files| files.get(file_idx.raw()));
                    if let Some(file) = file {
                        let status_str = match file.status {
                            FileStatus::Added => "A",
                            FileStatus::Modified => "M",
                            FileStatus::Deleted => "D",
                            FileStatus::Renamed => "R",
                            FileStatus::Copied => "C",
                        };
                        let status_color = match file.status {
                            FileStatus::Added => theme.added,
                            FileStatus::Modified => theme.change_id,
                            FileStatus::Deleted => theme.error,
                            FileStatus::Renamed | FileStatus::Copied => theme.accent,
                        };
                        let mut spans = vec![
                            gutter_span(row_search.as_ref(), theme),
                            Span::styled(
                                format!("  {status_str} "),
                                Style::default().fg(status_color),
                            ),
                            Span::styled(
                                file.path.as_str().to_string(),
                                Style::default().fg(theme.text),
                            ),
                        ];
                        if file.stats.added > 0 || file.stats.removed > 0 {
                            spans.push(Span::raw(" "));
                            push_line_stats(&mut spans, file.stats, false, theme);
                        }
                        vec![Line::from(spans)]
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::EvoLogFileDiffLine {
                    evolog_idx,
                    file_idx,
                    line_idx,
                } => {
                    let diff_line = app.evolog.entries.get(evolog_idx.raw()).and_then(|e| {
                        let files = app.evolog.files.get(&e.commit_id)?.loaded()?;
                        let file = files.get(file_idx.raw())?;
                        let key = (e.commit_id.clone(), file.path.clone());
                        let diffs = if git_diff {
                            &app.evolog.file_diffs
                        } else {
                            &app.evolog.file_diffs_cw
                        };
                        diffs.get(&key)?.loaded()?.get(line_idx.raw())
                    });
                    if let Some(diff_line) = diff_line {
                        let (base_style, prefix) = match diff_line.kind {
                            DiffLineKind::Added => (Style::default().fg(theme.added), "+"),
                            DiffLineKind::Removed => (Style::default().fg(theme.error), "-"),
                            DiffLineKind::Context => (Style::default().fg(theme.muted), " "),
                            DiffLineKind::Header => (
                                Style::default()
                                    .fg(theme.accent)
                                    .add_modifier(Modifier::BOLD),
                                "@",
                            ),
                        };
                        let mut spans = vec![
                            gutter_span(row_search.as_ref(), theme),
                            Span::styled(format!("    {prefix} "), base_style),
                        ];
                        push_diff_tokens(
                            &mut spans,
                            diff_line,
                            base_style,
                            row_search.as_ref(),
                            theme,
                            &tab_spaces,
                        );
                        vec![Line::from(spans)]
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::EvoLogGraphLink {
                    evolog_idx,
                    line_idx,
                } => render_simple_graph_link(
                    app.evolog
                        .entries
                        .get(evolog_idx.raw())
                        .and_then(|e| e.graph.extra.get(line_idx.raw())),
                    row_search.as_ref(),
                    theme,
                ),
                DisplayRow::WorkspaceItem { workspace_idx } => {
                    if let Some(entry) = app.views.workspace_entries.get(workspace_idx.raw()) {
                        render_workspace_item(entry, row_search.as_ref(), theme)
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::CommandLogItem { log_idx } => {
                    if let Some(entry) = app.command_log.entries.get(log_idx.raw()) {
                        render_command_log_item(entry, theme)
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::CommandLogDetail { log_idx, line_idx } => {
                    if let Some(entry) = app.command_log.entries.get(log_idx.raw()) {
                        render_command_log_detail(entry, line_idx.raw(), theme)
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::ConflictHeader {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                } => {
                    let hunks = app
                        .nodes
                        .get(*entry_idx)
                        .and_then(|n| n.conflict_hunks.get(file_idx.raw()))
                        .and_then(|l| l.loaded());
                    let total = hunks
                        .map(|h| {
                            h.iter()
                                .filter(|h| {
                                    matches!(h, crate::dag::ConflictHunkKind::Conflict { .. })
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    let num = hunks
                        .map(|h| {
                            h.iter()
                                .take(hunk_idx.raw() + 1)
                                .filter(|h| {
                                    matches!(h, crate::dag::ConflictHunkKind::Conflict { .. })
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    vec![Line::from(vec![
                        Span::raw("        "),
                        Span::styled(
                            format!("── conflict {num} of {total} ──"),
                            Style::default()
                                .fg(theme.error)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])]
                }
                DisplayRow::ConflictSide {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                    side_idx,
                    line_idx,
                } => {
                    let text = app
                        .nodes
                        .get(*entry_idx)
                        .and_then(|n| n.conflict_hunks.get(file_idx.raw()))
                        .and_then(|l| l.loaded())
                        .and_then(|hunks| hunks.get(hunk_idx.raw()))
                        .and_then(|hunk| match &hunk.kind {
                            crate::dag::ConflictHunkKind::Conflict {
                                sides, selected, ..
                            } => {
                                let line = sides
                                    .get(side_idx.raw())
                                    .and_then(|s| s.get(line_idx.raw()))
                                    .cloned()
                                    .unwrap_or_default();
                                let is_selected = *selected == Some(side_idx.raw());
                                Some((line, is_selected, side_idx.raw()))
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    let (line_text, is_selected, si) = text;
                    let label = match si {
                        0 => "[ours]  ",
                        1 => "[theirs]",
                        _ => "[base]  ",
                    };
                    let side_color = match si {
                        0 => theme.added,     // ours = green
                        1 => theme.change_id, // theirs = magenta
                        _ => theme.accent,    // base = cyan
                    };
                    let style = if is_selected {
                        Style::default()
                            .fg(side_color)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    } else {
                        Style::default().fg(side_color)
                    };
                    vec![Line::from(vec![
                        Span::raw("          "),
                        Span::styled(label, style),
                        Span::raw(" "),
                        Span::styled(line_text, style),
                    ])]
                }
                DisplayRow::ConflictContext {
                    entry_idx,
                    file_idx,
                    hunk_idx,
                    line_idx,
                } => {
                    let text = app
                        .nodes
                        .get(*entry_idx)
                        .and_then(|n| n.conflict_hunks.get(file_idx.raw()))
                        .and_then(|l| l.loaded())
                        .and_then(|hunks| hunks.get(hunk_idx.raw()))
                        .and_then(|hunk| match &hunk.kind {
                            crate::dag::ConflictHunkKind::Resolved { lines } => {
                                lines.get(line_idx.raw()).cloned()
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    vec![Line::from(vec![
                        Span::raw("        "),
                        Span::styled(text, Style::default().fg(theme.muted)),
                    ])]
                }
            }
        })
        .collect();

    // Compute max content width across all visible lines, then clamp h_scroll.
    let max_content_width: usize = raw_items
        .iter()
        .flat_map(|lines| lines.iter().map(line_width))
        .max()
        .unwrap_or(0);
    if max_content_width > max_w {
        app.h_scroll = app.h_scroll.min(max_content_width - max_w);
    } else {
        app.h_scroll = 0;
    }
    let h_skip = app.h_scroll;

    // Apply horizontal scroll trimming.
    let items: Vec<ListItem> = raw_items
        .into_iter()
        .map(|lines| {
            let trimmed: Vec<Line> = if h_skip > 0 {
                lines
                    .into_iter()
                    .map(|line| trim_line(line, h_skip, max_w))
                    .collect()
            } else {
                lines
            };
            ListItem::new(trimmed)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .scroll_padding(2)
        .highlight_style(
            Style::default()
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        );

    app.list_state.select(Some(app.cursor));
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

/// Render a commit as a 2-line ListItem matching `jj log` default format:
///
/// ```text
fn push_graph_node_spans(
    spans: &mut Vec<Span<'static>>,
    node: &str,
    glyph_style: Style,
    config: &Config,
) {
    let theme = &config.theme;
    for (is_glyph, group) in node
        .char_indices()
        .chunk_by(|&(_, c)| config.glyphs.is_glyph(c))
        .into_iter()
    {
        let mut iter = group.into_iter();
        let (start, c) = iter.next().expect("chunk_by groups are non-empty");
        let end = {
            let (end, c) = iter.last().unwrap_or((start, c));
            end + c.len_utf8()
        };
        let s = &node[start..end];
        if is_glyph {
            spans.push(Span::styled(s.to_string(), glyph_style));
        } else {
            spans.push(Span::styled(
                s.to_string(),
                Style::default().fg(theme.muted),
            ));
        }
    }
}

/// ○  change_id author timestamp bookmarks commit_id
/// │  description
/// ```
fn render_commit_item<'a>(
    graph_node: &'a str,
    graph_cont: &str,
    c: &'a CommitInfo,
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
            crate::dag::Glyph::WorkingCopy => theme.added,
            crate::dag::Glyph::Conflict => theme.error,
            crate::dag::Glyph::Immutable | crate::dag::Glyph::Merge => theme.accent,
            crate::dag::Glyph::Normal => theme.accent,
        }
    };

    let graph_style = Style::default().fg(graph_color);
    let search_state = search.map(|s| s.row_state).unwrap_or(SearchRowState::None);

    // --- Line 1: graph  change_id author timestamp bookmarks commit_id ---
    let mut line1: Vec<Span<'static>> = Vec::new();
    line1.push(search_gutter(search_state, theme));

    // Visual range column + selection column + spacer.
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

    // Change ID (prefix bright, rest dimmed; red if divergent)
    let change_color = if c.is_divergent {
        theme.error
    } else if c.is_hidden {
        theme.text
    } else {
        theme.change_id
    };
    let change_id_text = if let Some(suffix) = c.change_id_suffix {
        format!("{}/{}", c.change_id.display, suffix)
    } else {
        c.change_id.display.clone()
    };
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(&change_id_text, search.query_lower, search.case_sensitive)
        {
            push_highlighted_short_id(
                &mut line1,
                &c.change_id,
                c.change_id_suffix.map(|s| format!("/{s}")),
                change_color,
                search.query_lower,
                search.case_sensitive,
                theme,
            );
        } else {
            push_short_id(&mut line1, &c.change_id, change_color, theme);
            if let Some(suffix) = c.change_id_suffix {
                line1.push(Span::styled(
                    format!("/{suffix}"),
                    Style::default().fg(change_color),
                ));
            }
        }
    } else {
        push_short_id(&mut line1, &c.change_id, change_color, theme);
        if let Some(suffix) = c.change_id_suffix {
            line1.push(Span::styled(
                format!("/{suffix}"),
                Style::default().fg(change_color),
            ));
        }
    }
    if c.is_divergent {
        line1.push(Span::styled(
            " (divergent)",
            Style::default().fg(theme.error),
        ));
    }
    line1.push(Span::raw(" "));

    // Author
    push_searchable(
        &mut line1,
        &c.author.email,
        SearchScopes::AUTHOR,
        Style::default().fg(theme.user),
        search,
    );
    line1.push(Span::raw(" "));

    // Timestamp (apply author's timezone offset)
    let tz =
        jiff::tz::Offset::from_seconds(c.author.tz_offset_seconds).unwrap_or(jiff::tz::Offset::UTC);
    let formatted = c
        .author
        .timestamp
        .to_zoned(jiff::tz::TimeZone::fixed(tz))
        .strftime(&config.date_format)
        .to_string();
    line1.push(Span::styled(formatted, Style::default().fg(theme.muted)));

    // Commit ID (at end, like jj log -- prefix bright, rest dimmed)
    line1.push(Span::raw(" "));
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::COMMIT_ID)
            && contains_query(
                c.commit_id.display.as_str(),
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

    // Local bookmarks (with * suffix if dirty)
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

    // Remote bookmarks (name@remote, shown when no local bookmark covers them)
    // Remote bookmarks (unsynced only — shown dimmer than local bookmarks).
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

    // Tags
    let tag_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
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

    // Workspace annotations (non-current workspaces shown as "name@")
    for ws in &c.workspaces {
        if !ws.is_current {
            line1.push(Span::raw(" "));
            line1.push(Span::styled(
                format!("{}@", ws.name),
                Style::default()
                    .fg(theme.added)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    if let Some(stats) = line_stats {
        push_line_stats(&mut line1, stats, false, theme);
    }

    // Hidden indicator
    if c.is_hidden {
        line1.push(Span::styled(" (hidden)", Style::default().fg(theme.text)));
    }

    // --- Line 2: graph_cont  description ---
    let mut line2: Vec<Span<'static>> = Vec::new();
    line2.push(search_gutter(search_state, theme));
    // Continue visual + selection bars on line 2.
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

    // Graph continuation prefix (properly padded by the renderer).
    line2.push(Span::styled(
        graph_cont.to_string(),
        Style::default().fg(theme.muted),
    ));

    // Status labels before the description.
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
        // Working copy without description is normal; committed without description
        // is likely a mistake — highlight as a warning.
        let placeholder_style = if !c.is_working_copy() && !c.is_empty {
            Style::default().fg(theme.warning)
        } else {
            Style::default().fg(theme.muted)
        };
        line2.push(Span::styled(placeholder, placeholder_style));
    }

    vec![Line::from(line1), Line::from(line2)]
}

fn render_file_line(
    file: &FileChange,
    is_unfolded: bool,
    sel_state: FileSelectionState,
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
        }
    };

    let fold_char = if is_unfolded { "▾" } else { "▸" };
    let select_char = match sel_state {
        FileSelectionState::Full => "●",
        FileSelectionState::Partial => "○",
        FileSelectionState::None => " ",
    };

    let mut spans = vec![gutter_span(search, theme)];
    spans.extend(vec![
        Span::styled(
            format!("  {select_char} "),
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
    push_line_stats(&mut spans, file.stats, true, theme);
    vec![Line::from(spans)]
}

/// Push diff line content with word-level highlighting.
/// For add/remove lines with token data, `Different` tokens are rendered bold.
/// Falls back to `push_searchable` for lines without token data or during search.
fn push_diff_tokens(
    spans: &mut Vec<Span<'static>>,
    diff_line: &DiffLine,
    base_style: Style,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
    tab_str: &str,
) {
    let has_tokens = diff_line
        .tokens
        .iter()
        .any(|t| t.kind != DiffTokenKind::Unchanged);

    if !has_tokens || search.is_some_and(|s| s.row_state != SearchRowState::None) {
        let content = diff_line.content.replace('\t', tab_str);
        push_searchable(spans, &content, SearchScopes::LINE, base_style, search);
        return;
    }

    for token in &diff_line.tokens {
        if token.text.is_empty() {
            continue;
        }
        let style = match token.kind {
            DiffTokenKind::Unchanged => base_style,
            DiffTokenKind::Removed => Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            DiffTokenKind::Added => Style::default()
                .fg(theme.added)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        };
        spans.push(Span::styled(expand_tabs(&token.text, tab_str), style));
    }
}

fn render_diff_line(
    diff_line: &DiffLine,
    show_line_numbers: bool,
    flags: &RenderFlags,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
    tab_str: &str,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];
    let is_selectable = diff_line.kind.is_selectable();
    if is_selectable {
        let bar = if flags.in_visual { "│" } else { " " };
        let sel = if flags.is_selected { "▎" } else { " " };
        spans.push(Span::styled(bar, Style::default().fg(theme.accent)));
        spans.push(Span::styled(sel, Style::default().fg(theme.selection)));
    } else {
        spans.push(Span::raw("  "));
    }

    let (marker, style) = match diff_line.kind {
        DiffLineKind::Header => (" ", Style::default().fg(theme.change_id)),
        DiffLineKind::Context => (" ", Style::default().fg(theme.muted)),
        DiffLineKind::Added => ("+", Style::default().fg(theme.added)),
        DiffLineKind::Removed => ("-", Style::default().fg(theme.error)),
    };

    let line_num_style = Style::default().fg(theme.muted);
    if show_line_numbers && diff_line.kind != DiffLineKind::Header {
        use std::fmt::Write;
        let mut nums = String::with_capacity(14);
        write!(nums, "  ").unwrap();
        match diff_line.old_line {
            Some(n) => write!(nums, "{n:>4}").unwrap(),
            None => write!(nums, "    ").unwrap(),
        }
        write!(nums, " ").unwrap();
        match diff_line.new_line {
            Some(n) => write!(nums, "{n:>4}").unwrap(),
            None => write!(nums, "    ").unwrap(),
        }
        write!(nums, " ").unwrap();
        spans.push(Span::styled(nums, line_num_style));
        spans.push(Span::styled(marker, style));
        push_diff_tokens(&mut spans, diff_line, style, search, theme, tab_str);
    } else {
        let prefix = match diff_line.kind {
            DiffLineKind::Header => "      ",
            DiffLineKind::Context => "       ",
            DiffLineKind::Added => "      +",
            DiffLineKind::Removed => "      -",
        };
        spans.push(Span::styled(prefix, style));
        push_diff_tokens(&mut spans, diff_line, style, search, theme, tab_str);
    }

    vec![Line::from(spans)]
}

fn render_tag_item(
    entry: &TagViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    // Tag name.
    let style = if entry.is_deleted {
        Style::default().fg(theme.muted)
    } else {
        Style::default().fg(theme.tag).add_modifier(Modifier::BOLD)
    };
    push_searchable(
        &mut spans,
        entry.name.as_str(),
        SearchScopes::TAG,
        style,
        search,
    );

    if entry.is_deleted {
        spans.push(Span::styled(" (deleted)", Style::default().fg(theme.muted)));
    }

    // Change ID (if available), with prefix highlighting.
    if let Some(ref cid) = entry.change_id {
        spans.push(dot(theme));
        push_short_id(&mut spans, cid, theme.change_id, theme);
    }

    // Description.
    if let Some(ref desc) = entry.description {
        spans.push(dot(theme));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

fn render_tag_remote_target(
    target: Option<&crate::dag::TagRemoteTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    // Indent + @remote.
    spans.push(Span::styled(
        format!("    @{}", target.remote),
        Style::default().fg(theme.remote),
    ));

    spans.push(Span::styled(": ", Style::default().fg(theme.muted)));

    // Change ID + commit ID.
    push_short_id(&mut spans, &target.change_id, theme.change_id, theme);
    spans.push(Span::raw(" "));
    push_short_id(&mut spans, &target.short_commit_id, theme.commit_id, theme);

    // Description.
    if let Some(ref desc) = target.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

fn render_op_log_item(
    entry: &OpLogEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    // Graph prefix (contains the glyph character).
    let graph_style = if entry.is_current {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    spans.push(Span::styled(entry.graph.node.clone(), graph_style));

    // Operation ID (truncated hex).
    spans.push(Span::styled(
        entry.id.to_string(),
        Style::default().fg(theme.commit_id),
    ));

    // Description.
    let desc_style = if entry.is_snapshot {
        Style::default().fg(theme.muted)
    } else if entry.is_current {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };
    spans.push(dot(theme));
    push_searchable(
        &mut spans,
        &entry.description,
        SearchScopes::DESCRIPTION,
        desc_style,
        search,
    );

    // Relative time.
    spans.push(dot(theme));
    let time_color = if entry.is_snapshot {
        theme.muted
    } else {
        Color::Cyan
    };
    spans.push(Span::styled(
        entry.relative_time.to_string(),
        Style::default().fg(time_color),
    ));

    // User.
    if !entry.user.is_empty() {
        spans.push(dot(theme));
        spans.push(Span::styled(
            entry.user.to_string(),
            Style::default().fg(theme.user),
        ));
    }

    let line1 = Line::from(spans);

    // Second line: gutter + graph continuation + workspace · command args.
    let mut spans2 = vec![
        gutter_span(search, theme),
        Span::styled(entry.graph.cont.clone(), Style::default().fg(theme.muted)),
    ];
    if let Some(ref ws) = entry.workspace {
        spans2.push(Span::styled(
            ws.to_string(),
            Style::default().fg(theme.workspace),
        ));
    }
    if let Some(ref args) = entry.args {
        if entry.workspace.is_some() {
            spans2.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        }
        push_searchable(
            &mut spans2,
            args,
            SearchScopes::PATH_COMMAND,
            Style::default().fg(theme.muted),
            search,
        );
    }
    let line2 = Line::from(spans2);

    vec![line1, line2]
}

fn render_op_detail_line(detail: Option<&OpDetailLine>, theme: &Theme) -> Vec<Line<'static>> {
    let Some(detail) = detail else {
        return vec![Line::raw("")];
    };
    let muted = Style::default().fg(theme.muted);
    match detail {
        OpDetailLine::SectionHeader(text) => vec![Line::from(vec![
            Span::raw("    "),
            Span::styled(
                text.to_string(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ])],
        OpDetailLine::Commit(c) => {
            let (indicator, change_color, commit_color) = match c.kind {
                OpDiffKind::Added => (
                    Span::styled("      + ", Style::default().fg(theme.added)),
                    theme.change_id,
                    theme.commit_id,
                ),
                OpDiffKind::Removed => (
                    Span::styled("      - ", Style::default().fg(theme.error)),
                    theme.muted,
                    theme.muted,
                ),
            };
            let mut spans = vec![indicator];
            if !c.change_id.display.is_empty() {
                push_short_id(&mut spans, &c.change_id, change_color, theme);
                spans.push(Span::raw(" "));
            }
            push_short_id(&mut spans, &c.commit_id, commit_color, theme);
            if let Some(ref desc) = c.description {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    desc.clone(),
                    match c.kind {
                        OpDiffKind::Added => Style::default().fg(theme.text),
                        OpDiffKind::Removed => muted,
                    },
                ));
            }
            vec![Line::from(spans)]
        }
        OpDetailLine::WorkingCopy(wc) => {
            let mut spans = vec![Span::raw("      ")];
            spans.push(Span::styled(
                wc.workspace.to_string(),
                Style::default().fg(theme.workspace),
            ));
            spans.push(Span::styled(": ", muted));
            if let Some(ref new) = wc.new_commit {
                push_short_id(&mut spans, new, theme.commit_id, theme);
            }
            if let Some(ref old) = wc.old_commit {
                spans.push(Span::styled(" ← ", muted));
                push_short_id(&mut spans, old, theme.muted, theme);
            }
            vec![Line::from(spans)]
        }
        OpDetailLine::Bookmark(bm) => {
            let mut spans = vec![Span::raw("      ")];
            spans.push(Span::styled(
                bm.name.to_string(),
                Style::default()
                    .fg(theme.bookmark)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(": ", muted));
            if let Some(ref new) = bm.new_target {
                push_short_id(&mut spans, new, theme.commit_id, theme);
            } else {
                spans.push(Span::styled("(deleted)", muted));
            }
            if let Some(ref old) = bm.old_target {
                spans.push(Span::styled(" ← ", muted));
                push_short_id(&mut spans, old, theme.muted, theme);
            }
            vec![Line::from(spans)]
        }
    }
}

fn render_evolog_item(
    entry: &EvoLogEntry,
    search: Option<&SearchRender<'_>>,
    config: &Config,
) -> Vec<Line<'static>> {
    let theme = &config.theme;
    let mut spans = vec![gutter_span(search, theme)];

    // Graph prefix.
    let graph_style = if entry.is_current {
        Style::default()
            .fg(theme.added)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    push_graph_node_spans(&mut spans, &entry.graph.node, graph_style, config);

    // Change ID (prefix bright, rest dimmed).
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(
                &entry.change_id.display,
                search.query_lower,
                search.case_sensitive,
            )
        {
            push_highlighted_short_id(
                &mut spans,
                &entry.change_id,
                None,
                theme.change_id,
                search.query_lower,
                search.case_sensitive,
                theme,
            );
        } else {
            push_short_id(&mut spans, &entry.change_id, theme.change_id, theme);
        }
    } else {
        push_short_id(&mut spans, &entry.change_id, theme.change_id, theme);
    }

    // Description.
    spans.push(dot(theme));
    let desc = entry
        .description
        .as_deref()
        .unwrap_or("(no description set)");
    let desc_style = if entry.is_current {
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };
    push_searchable(
        &mut spans,
        desc,
        SearchScopes::DESCRIPTION,
        desc_style,
        search,
    );

    let line1 = Line::from(spans);

    // Second line: graph continuation + author · time · op description.
    let mut spans2 = vec![
        gutter_span(search, theme),
        Span::styled(entry.graph.cont.clone(), Style::default().fg(theme.muted)),
    ];

    spans2.push(Span::styled(
        entry.author.to_string(),
        Style::default().fg(theme.user),
    ));

    spans2.push(dot(theme));
    spans2.push(Span::styled(
        entry.relative_time.to_string(),
        Style::default().fg(Color::Cyan),
    ));

    if let Some(ref op) = entry.op_description {
        spans2.push(dot(theme));
        spans2.push(Span::styled(
            op.to_string(),
            Style::default().fg(theme.muted),
        ));
    }

    let line2 = Line::from(spans2);
    vec![line1, line2]
}

fn render_simple_graph_link(
    graph_str: Option<&String>,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let s = graph_str.map(|s| s.as_str()).unwrap_or("");
    vec![Line::from(vec![
        gutter_span(search, theme),
        Span::styled(s.to_string(), Style::default().fg(theme.muted)),
    ])]
}

fn render_workspace_item(
    entry: &WorkspaceViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    // Workspace name.
    let name_style = if entry.is_current {
        Style::default()
            .fg(theme.workspace)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.workspace)
    };
    spans.push(Span::styled(entry.name.to_string(), name_style));
    if entry.is_current {
        spans.push(Span::styled(" (current)", Style::default().fg(theme.muted)));
    }

    // Change ID.
    if let Some(ref cid) = entry.change_id {
        spans.push(dot(theme));
        push_short_id(&mut spans, cid, theme.change_id, theme);
    }

    // Description.
    if let Some(ref desc) = entry.description {
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

fn render_command_log_item(
    entry: &crate::app::CommandLogEntry,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::app::CommandLogKind;

    let (icon, icon_color) = match (entry.kind, entry.success) {
        (CommandLogKind::Warning, _) => ("⚠  ", theme.warning),
        (_, true) => ("✓  ", theme.added),
        (_, false) => ("✗  ", theme.error),
    };

    let summary_style = if entry.success {
        Style::default().fg(theme.text)
    } else {
        Style::default().fg(theme.error)
    };

    let relative_time = crate::repo::millis_to_relative_time(entry.timestamp.as_millisecond());

    let mut spans = vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            icon,
            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(ref parts) = entry.command_parts {
        spans.extend(command_parts_to_spans(parts, theme));
    } else {
        spans.push(Span::styled(entry.summary.clone(), summary_style));
    }
    spans.push(Span::styled(
        format!(" · {relative_time}"),
        Style::default().fg(theme.muted),
    ));

    vec![Line::from(spans)]
}

fn render_command_log_detail(
    entry: &crate::app::CommandLogEntry,
    line_idx: usize,
    _theme: &Theme,
) -> Vec<Line<'static>> {
    use ansi_to_tui::IntoText as _;

    // Parse ANSI output into ratatui styled text, then pick the requested line.
    if let Ok(styled) = entry.output.as_slice().into_text() {
        if let Some(line) = styled.lines.into_iter().nth(line_idx) {
            let mut spans: Vec<Span<'static>> = vec![Span::raw("      ")];
            spans.extend(line.spans);
            return vec![Line::from(spans)];
        }
    }
    // Fallback: plain text.
    let text = String::from_utf8_lossy(&entry.output);
    let line_text = text.lines().nth(line_idx).unwrap_or("");
    vec![Line::from(vec![
        Span::raw("      "),
        Span::raw(line_text.to_string()),
    ])]
}

fn render_bookmark_item(
    entry: &BookmarkViewEntry,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut spans = vec![gutter_span(search, theme)];

    // Conflict indicator for bookmark conflicts (divergent operations).
    if entry.is_conflicted {
        spans.push(Span::styled("! ", Style::default().fg(theme.error)));
    } else {
        spans.push(Span::raw("  "));
    }
    if let Some(remote) = &entry.remote {
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
    } else if entry.is_dirty {
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

    // Change ID (if available), with prefix highlighting.
    if let Some(ref cid) = entry.change_id {
        spans.push(dot(theme));
        push_short_id(&mut spans, cid, theme.change_id, theme);
    }

    // Description.
    if let Some(ref desc) = entry.description {
        spans.push(dot(theme));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

/// Push a ShortId with an optional divergence suffix (e.g., `/2`).
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

/// Render a conflict target child row, indented under the bookmark name.
fn render_bookmark_conflict_target(
    target: Option<&crate::dag::BookmarkConflictTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    // Gutter-width blank + conflict-indicator-width blank + indent + kind.
    let (indicator, color) = match target.kind {
        crate::dag::ConflictTargetKind::Added => ("      + ", theme.added),
        crate::dag::ConflictTargetKind::Removed => ("      - ", theme.error),
    };
    spans.push(Span::styled(indicator, Style::default().fg(color)));

    push_short_id_with_suffix(
        &mut spans,
        &target.change_id,
        theme.change_id,
        target.change_id_suffix,
        theme,
    );
    spans.push(Span::raw(" "));
    push_short_id(&mut spans, &target.short_commit_id, theme.commit_id, theme);

    if target.is_hidden {
        spans.push(Span::styled(" (hidden)", Style::default().fg(theme.muted)));
    }

    // Description.
    if let Some(ref desc) = target.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

/// Render a remote tracking child row, indented under the bookmark name.
fn render_bookmark_remote_target(
    target: Option<&crate::dag::BookmarkRemoteTarget>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(target) = target else {
        return vec![Line::raw("")];
    };
    let mut spans: Vec<Span<'static>> = Vec::new();

    // Gutter-width blank + conflict-indicator-width blank + indent + @remote.
    spans.push(Span::styled(
        format!("      @{}", target.remote),
        Style::default().fg(theme.remote),
    ));

    // Status annotations: ahead/behind counts, untracked marker.
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
        &target.change_id,
        theme.change_id,
        target.change_id_suffix,
        theme,
    );
    spans.push(Span::raw(" "));
    push_short_id(&mut spans, &target.short_commit_id, theme.commit_id, theme);

    if let Some(ref desc) = target.description {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(desc.clone(), Style::default().fg(theme.text)));
    }

    vec![Line::from(spans)]
}

/// Returns `(prefix, old_mid, new_mid, suffix)`.
///
/// `"src/old.rs"` → `"src/new.rs"` gives `("src/", "old.rs", "new.rs", "")`.
/// `"a/foo/b/c.rs"` → `"a/bar/b/c.rs"` gives `("a/", "foo", "bar", "/b/c.rs")`.
fn diff_path_parts<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str, &'a str, &'a str) {
    // Common prefix, snapped to '/' boundary.
    let shared_pre = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let pre = old[..shared_pre].rfind('/').map(|i| i + 1).unwrap_or(0);

    // Common suffix from the remaining parts, snapped to '/' boundary.
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
