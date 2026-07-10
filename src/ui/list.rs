use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use super::search::*;
use super::spans::*;
use super::views::*;
use crate::idx::RowIdx;
use std::collections::{HashMap, HashSet};

use crate::app::{App, AppMode, TargetMode};
use crate::idx::EntryIdx;
use crate::theme::Config;
use crate::types::ChangeId;
use crate::types::{DisplayRow, SearchScopes};

pub(super) fn expand_tabs(s: &str, tab_spaces: &str) -> String {
    if s.contains('\t') {
        s.replace('\t', tab_spaces)
    } else {
        s.to_string()
    }
}

fn pad_or_truncate(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let display_width: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if display_width > width {
        let mut used = 0;
        let truncated: String = s
            .chars()
            .take_while(|c| {
                let w = c.width().unwrap_or(0);
                if used + w > width {
                    return false;
                }
                used += w;
                true
            })
            .collect();
        let pad = width.saturating_sub(used);
        if pad > 0 {
            format!("{truncated}{:pad$}", "")
        } else {
            truncated
        }
    } else {
        let pad = width.saturating_sub(display_width);
        format!("{s}{:pad$}", "")
    }
}

pub(super) fn draw_list(frame: &mut Frame, area: Rect, app: &mut App, config: &Config) {
    let theme = &config.theme;
    let tab_spaces: String = " ".repeat(config.tab_width as usize);
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
            |marks| marks.contains(&app.nodes[entry_idx].commit.unique_prefix()),
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
    let search_ctx = app.search.as_ref().map(|_| SearchRender {
        query_lower: query_lowered.as_deref().unwrap_or(""),
        scopes: app.search_scopes(),
        case_sensitive: search_case_sensitive,
        row_state: SearchRowState::None,
    });

    let (jump_labels, jump_input_len): (HashMap<RowIdx, &str>, usize) = match &app.mode {
        AppMode::Jump { labels, input, .. } => (
            labels.iter().map(|(s, idx)| (*idx, s.as_str())).collect(),
            input.len(),
        ),
        _ => (HashMap::new(), 0),
    };

    let annotate_highlight_commit: Option<crate::types::CommitId> =
        if app.active_view == crate::app::ActiveView::Annotate {
            app.selected_annotate_line()
                .map(|line| line.commit_id.clone())
        } else {
            None
        };

    let offset = app.list_state.offset();
    let vis_start = offset.min(app.cursor.raw()).saturating_sub(20);
    let vis_end = (offset.max(app.cursor.raw()) + area.height as usize + 20).min(app.rows.len());

    let max_w = area.width as usize;
    let mut raw_items: Vec<Vec<Line>> = app
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| -> Vec<Line<'static>> {
            if row_idx < vis_start || row_idx >= vis_end {
                return match row {
                    DisplayRow::CommitNode { .. }
                    | DisplayRow::OpLogItem { .. }
                    | DisplayRow::EvoLogItem { .. } => {
                        vec![Line::default(), Line::default()]
                    }
                    _ => vec![Line::default()],
                };
            }
            let row_search = search_ctx.as_ref().map(|ctx| SearchRender {
                row_state: search_row_state(app, RowIdx::new(row_idx)),
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
                    let graph_cont = app.nodes[*entry_idx].graph.rest.as_str();
                    let selected = is_marked(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    let mut spans = vec![gutter_span(row_search.as_ref(), theme)];
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
                    let in_visual = app.is_in_visual_file_range(*entry_idx, *file_idx);
                    render_file_line(
                        file,
                        is_unfolded,
                        sel_state,
                        in_visual,
                        row_search.as_ref(),
                        theme,
                    )
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
                        in_visual: app.is_in_visual_range(
                            *entry_idx,
                            *file_idx,
                            *line_idx,
                            RowIdx::new(row_idx),
                        ),
                    };
                    render_diff_line(
                        diff_line,
                        app.show_line_numbers,
                        app.diff_underline,
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
                DisplayRow::BookmarkSeparator => vec![Line::from(Span::styled(
                    "─".repeat(area.width as usize),
                    Style::default().fg(theme.muted),
                ))],
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
                        let (status_str, status_color) = file_status_display(file.status, theme);
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
                    let diff_line = app
                        .evolog_diff_lines(*evolog_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()));
                    render_simple_diff_line(
                        diff_line,
                        app.diff_underline,
                        row_search.as_ref(),
                        theme,
                        &tab_spaces,
                    )
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
                        render_command_log_item(entry, row_search.as_ref(), theme)
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
                    let (num, total) = app
                        .nodes
                        .get(*entry_idx)
                        .and_then(|n| n.conflict_hunks(*file_idx))
                        .and_then(|l| l.loaded())
                        .map(|hunks| {
                            let mut num = 0usize;
                            let mut total = 0usize;
                            for (i, h) in hunks.iter().enumerate() {
                                if matches!(h, crate::dag::ConflictHunkKind::Conflict { .. }) {
                                    total += 1;
                                    if i <= hunk_idx.raw() {
                                        num += 1;
                                    }
                                }
                            }
                            (num, total)
                        })
                        .unwrap_or((0, 0));
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
                        .and_then(|n| n.conflict_hunks(*file_idx))
                        .and_then(|l| l.loaded())
                        .and_then(|hunks: &Vec<crate::dag::ConflictHunkKind>| {
                            hunks.get(hunk_idx.raw())
                        })
                        .and_then(|hunk| match hunk {
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
                        0 => theme.added,
                        1 => theme.change_id,
                        _ => theme.accent,
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
                        .and_then(|n| n.conflict_hunks(*file_idx))
                        .and_then(|l| l.loaded())
                        .and_then(|hunks: &Vec<crate::dag::ConflictHunkKind>| {
                            hunks.get(hunk_idx.raw())
                        })
                        .and_then(|hunk| match hunk {
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
                DisplayRow::InterdiffHeader => {
                    let (from, to) = app
                        .interdiff
                        .target
                        .as_ref()
                        .map(|t| (t.from_label.to_string(), t.to_label.to_string()))
                        .unwrap_or_default();
                    let mut spans = vec![gutter_span(row_search.as_ref(), theme)];
                    spans.push(Span::styled(
                        "  interdiff: ",
                        Style::default().fg(theme.muted),
                    ));
                    spans.push(Span::styled(from, Style::default().fg(theme.change_id)));
                    spans.push(Span::styled(" \u{2192} ", Style::default().fg(theme.muted)));
                    spans.push(Span::styled(to, Style::default().fg(theme.change_id)));
                    vec![Line::from(spans)]
                }
                DisplayRow::InterdiffFileChange { file_idx } => {
                    let file = app
                        .interdiff
                        .files
                        .loaded()
                        .and_then(|files| files.get(file_idx.raw()));
                    if let Some(file) = file {
                        let (status_str, status_color) = file_status_display(file.status, theme);
                        let is_unfolded = app.interdiff.unfolded_files.contains(&file.path);
                        let fold_char = if is_unfolded { "\u{25be}" } else { "\u{25b8}" };
                        let mut spans = vec![
                            gutter_span(row_search.as_ref(), theme),
                            Span::styled(
                                format!("  {fold_char} "),
                                Style::default().fg(theme.muted),
                            ),
                            Span::styled(
                                format!("{status_str} "),
                                Style::default().fg(status_color),
                            ),
                            Span::styled(
                                file.path.as_str().to_string(),
                                Style::default().fg(theme.text),
                            ),
                        ];
                        if file.stats.added > 0 || file.stats.removed > 0 {
                            push_line_stats(&mut spans, file.stats, false, theme);
                        }
                        vec![Line::from(spans)]
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::InterdiffDiffLine { file_idx, line_idx } => {
                    let diff_line = app
                        .interdiff_diff_lines(*file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()));
                    render_simple_diff_line(
                        diff_line,
                        app.diff_underline,
                        row_search.as_ref(),
                        theme,
                        &tab_spaces,
                    )
                }
                DisplayRow::AnnotateLine { line_idx } => {
                    let lines_data = app.annotate.lines.loaded();
                    let line = lines_data.and_then(|lines| lines.get(line_idx.raw()));
                    if let Some(line) = line {
                        let same_commit = annotate_highlight_commit
                            .as_ref()
                            .is_some_and(|c| *c == line.commit_id);
                        let is_cursor = row_idx == app.cursor.raw();

                        let prev_commit = if line_idx.raw() > 0 {
                            lines_data
                                .and_then(|l| l.get(line_idx.raw() - 1))
                                .map(|l| &l.commit_id)
                        } else {
                            None
                        };
                        let is_boundary = prev_commit.is_some_and(|pc| *pc != line.commit_id);

                        let mut spans = vec![];
                        if is_cursor {
                            spans.push(Span::styled("▌", Style::default().fg(theme.accent)));
                            spans.push(Span::raw(" "));
                        } else {
                            spans.push(gutter_span(row_search.as_ref(), theme));
                        }
                        spans.push(Span::raw("  "));
                        push_short_id(&mut spans, &line.change_id, theme.change_id, theme);
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            pad_or_truncate(&line.author, 15),
                            Style::default().fg(theme.selection),
                        ));
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            format!("{} ", pad_or_truncate(&line.relative_time, 15)),
                            Style::default().fg(theme.muted),
                        ));
                        let gutter_end = spans.len();
                        spans.push(Span::styled(
                            format!("{:>5}: ", line.line_number),
                            Style::default().fg(theme.muted),
                        ));
                        if line.syntax_tokens.is_empty() {
                            let content = expand_tabs(&line.content, &tab_spaces);
                            push_searchable(
                                &mut spans,
                                &content,
                                SearchScopes::LINE,
                                Style::default().fg(theme.text),
                                row_search.as_ref(),
                            );
                        } else {
                            push_tokens_with_search(
                                &mut spans,
                                &line.syntax_tokens,
                                &tab_spaces,
                                row_search.as_ref(),
                            );
                        }
                        if is_cursor {
                            let used: usize = spans.iter().map(|s| s.width()).sum();
                            if used < max_w {
                                spans
                                    .push(Span::styled(" ".repeat(max_w - used), Style::default()));
                            }
                            for span in &mut spans {
                                span.style = span.style.bg(theme.selection_bg_strong);
                            }
                        } else if same_commit {
                            for span in &mut spans[..gutter_end] {
                                span.style = span.style.bg(theme.selection_bg);
                            }
                        }
                        let mut result = Vec::new();
                        if is_boundary && app.annotate.show_commit_separators {
                            result.push(Line::styled(
                                "─".repeat(max_w),
                                Style::default().fg(theme.muted),
                            ));
                        }
                        result.push(Line::from(spans));
                        result
                    } else {
                        vec![Line::raw("")]
                    }
                }
                DisplayRow::AnnotateDetail {
                    line_idx,
                    detail_idx,
                } => {
                    let info = app
                        .annotate
                        .lines
                        .loaded()
                        .and_then(|l| l.get(line_idx.raw()))
                        .and_then(|line| app.annotate.commit_info.get(&line.commit_id));
                    if let Some(info) = info {
                        let di = detail_idx.raw();
                        let is_cursor = row_idx == app.cursor.raw();
                        let mut spans = vec![gutter_span(row_search.as_ref(), theme)];
                        let label_style = Style::default().fg(theme.muted);
                        match di {
                            0 => {
                                spans.push(Span::styled("      Change:    ", label_style));
                                push_short_id(&mut spans, &info.change_id, theme.change_id, theme);
                            }
                            1 => {
                                spans.push(Span::styled("      Commit:    ", label_style));
                                push_short_id(&mut spans, &info.commit_id, theme.commit_id, theme);
                            }
                            2 => {
                                spans.push(Span::styled("      Author:    ", label_style));
                                spans.push(Span::styled(
                                    format!(
                                        "{} <{}>  {}",
                                        info.author_name, info.author_email, info.author_date
                                    ),
                                    Style::default().fg(theme.text),
                                ));
                            }
                            3 => {
                                spans.push(Span::styled("      Committer: ", label_style));
                                spans.push(Span::styled(
                                    format!(
                                        "{} <{}>  {}",
                                        info.committer_name,
                                        info.committer_email,
                                        info.committer_date
                                    ),
                                    Style::default().fg(theme.text),
                                ));
                            }
                            _ => {
                                let desc_line_idx = di - 4;
                                let text = if info.description_lines.is_empty() {
                                    "(no description set)"
                                } else {
                                    info.description_lines
                                        .get(desc_line_idx)
                                        .map(|s| s.as_str())
                                        .unwrap_or("")
                                };
                                spans.push(Span::styled(
                                    format!("      {text}"),
                                    Style::default().fg(theme.text),
                                ));
                            }
                        }
                        if is_cursor {
                            let used: usize = spans.iter().map(|s| s.width()).sum();
                            if used < max_w {
                                spans
                                    .push(Span::styled(" ".repeat(max_w - used), Style::default()));
                            }
                            for span in &mut spans {
                                span.style = span.style.bg(theme.selection_bg_strong);
                            }
                        }
                        vec![Line::from(spans)]
                    } else {
                        vec![Line::raw("")]
                    }
                }
            }
        })
        .collect();

    if !jump_labels.is_empty() {
        let label_style = Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD);
        for (row_idx, label) in &jump_labels {
            if let Some(lines) = raw_items.get_mut(row_idx.raw()) {
                let target_line = if lines.len() > 1 {
                    let first_is_separator = lines[0]
                        .spans
                        .first()
                        .is_some_and(|s| s.content.starts_with('─'));
                    if first_is_separator {
                        lines.last_mut()
                    } else {
                        lines.first_mut()
                    }
                } else {
                    lines.first_mut()
                };
                if let Some(target_line) = target_line {
                    if let Some(first_span) = target_line.spans.first_mut() {
                        let remaining = &label[jump_input_len..];
                        let display = if remaining.len() >= 2 {
                            remaining.to_string()
                        } else {
                            format!("{remaining} ")
                        };
                        *first_span = Span::styled(display, label_style);
                    }
                }
            }
        }
    }

    let max_content_width: usize = raw_items
        .get(vis_start..vis_end)
        .unwrap_or(&[])
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

    let items: Vec<ListItem> = raw_items
        .into_iter()
        .enumerate()
        .map(|(i, lines)| {
            let trimmed: Vec<Line> = if h_skip > 0 && i >= vis_start && i < vis_end {
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

    let highlight = if app.active_view == crate::app::ActiveView::Annotate {
        Style::default()
    } else {
        Style::default()
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .scroll_padding(2)
        .highlight_style(highlight);

    app.list_state.select(Some(app.cursor.raw()));
    frame.render_stateful_widget(list, area, &mut app.list_state);
}
