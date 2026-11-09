use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use super::search::*;
use super::spans::*;
use super::views::*;
use crate::idx::RowIdx;
use std::collections::HashMap;

use crate::app::{App, AppMode, SlotState};
use crate::config::Config;
use crate::types::{
    ActiveView, ConflictHunkRef, ContentSlot, DisplayRow, FileOwner, FileSelectionState,
};

pub(super) fn expand_tabs(s: &str, tab_spaces: &str) -> String {
    if s.contains('\t') {
        s.replace('\t', tab_spaces)
    } else {
        s.to_string()
    }
}

pub(super) fn pad_or_truncate(s: &str, width: usize) -> String {
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

pub(super) fn draw_list(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    config: &Config,
    retry_key: Option<String>,
) {
    let theme = &config.theme;
    let tab_spaces: String = " ".repeat(config.tab_width as usize);

    // Only rows in the viewport window are rendered; scrolling is managed
    // here (not by ratatui's List) so per-frame work stays O(visible rows).
    let viewport = area.height as usize;
    app.update_scroll(viewport);
    let vis_start = app.scroll;
    let mut lines_used = 0;
    let mut vis_end = vis_start;
    while vis_end < app.rows.len() && lines_used < viewport {
        lines_used += app.row_display_lines(vis_end);
        vis_end += 1;
    }

    let search_query = app
        .search
        .as_ref()
        .map(|s| crate::app::SearchQuery::new(s.query()));
    let search_ctx = search_query.as_ref().map(|query| SearchRender {
        query,
        scopes: app.search_scopes(),
        row_state: SearchRowState::None,
    });

    let (jump_labels, jump_input_len): (HashMap<DisplayRow, &str>, usize) = match &app.mode {
        AppMode::Jump(state) => (
            state
                .labels
                .iter()
                .map(|(s, row)| (*row, s.as_str()))
                .collect(),
            state.input.len(),
        ),
        _ => (HashMap::new(), 0),
    };

    let ctx = RowContext {
        app,
        config,
        tab_spaces: &tab_spaces,
        width: area.width as usize,
        picks: app.mode.picks(),
        annotate_highlight: (app.active_view == ActiveView::Annotate)
            .then(|| {
                app.selected_annotate_line()
                    .map(|line| line.commit_id.clone())
            })
            .flatten(),
        loading_shown: app.activity_shown(std::time::Instant::now()),
        retry_key,
    };
    let mut rendered: Vec<RenderedRow> = (vis_start..vis_end)
        .map(|row_idx| {
            let search = search_ctx.as_ref().map(|s| SearchRender {
                row_state: search_row_state(app, RowIdx::new(row_idx)),
                ..*s
            });
            render_row(&ctx, row_idx, search.as_ref())
        })
        .collect();

    let label_style = Style::default()
        .fg(theme.warning)
        .add_modifier(Modifier::BOLD);
    // Each label goes on its row wherever that row is drawn now.
    for (i, row) in rendered.iter_mut().enumerate() {
        let Some(label) = jump_labels.get(&app.rows[vis_start + i]) else {
            continue;
        };
        if let Some(first_span) = row
            .lines
            .get_mut(row.label_line)
            .and_then(|line| line.spans.first_mut())
        {
            let remaining = &label[jump_input_len..];
            let display = if remaining.len() >= 2 {
                remaining.to_string()
            } else {
                format!("{remaining} ")
            };
            *first_span = Span::styled(display, label_style);
        }
    }

    let max_w = area.width as usize;
    let max_content_width: usize = rendered
        .iter()
        .flat_map(|row| row.lines.iter().map(line_width))
        .max()
        .unwrap_or(0);
    app.h_scroll = if max_content_width > max_w {
        app.h_scroll.min(max_content_width - max_w)
    } else {
        0
    };
    let h_skip = app.h_scroll;

    let items: Vec<ListItem> = rendered
        .into_iter()
        .map(|row| {
            let lines: Vec<Line> = if h_skip > 0 {
                row.lines
                    .into_iter()
                    .map(|line| trim_line(line, h_skip, max_w))
                    .collect()
            } else {
                row.lines
            };
            ListItem::new(lines)
        })
        .collect();

    // The annotate view paints its own cursor highlight across the width.
    let highlight = if app.active_view == ActiveView::Annotate {
        Style::default()
    } else {
        Style::default()
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .highlight_style(highlight);

    // `update_scroll` guarantees the cursor row is inside the window, so the
    // window-local list never needs to scroll on its own.
    let mut list_state =
        ListState::default().with_selected(Some(app.cursor.raw().saturating_sub(vis_start)));
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// A placeholder where content belongs, indented like what it stands in
/// for. Loading, it stays blank until loading has gone on long enough to
/// mention; failed, it gives the error and how to ask again.
fn render_placeholder_row(ctx: &RowContext<'_>, slot: ContentSlot) -> Vec<Line<'static>> {
    let app = ctx.app;
    let theme = &ctx.config.theme;
    let (indent, what) = match slot {
        // Where a file row's fold arrow sits.
        ContentSlot::Files(_) => ("     ", "changed files"),
        // Under the file, where its diff starts.
        ContentSlot::Diff(owner, file_idx) => {
            let conflicted = app.file(owner, file_idx).is_some_and(|f| f.has_conflict);
            let what = if conflicted && matches!(owner, FileOwner::Dag(_)) {
                "conflict"
            } else {
                "diff"
            };
            ("         ", what)
        }
        // Where an operation's detail lines start.
        ContentSlot::Operation(_) => ("    ", "operation"),
        ContentSlot::View => (
            "  ",
            match app.active_view {
                ActiveView::Operations => "operations",
                ActiveView::Evolog => "evolution log",
                ActiveView::Annotate => "annotation",
                _ => "commits",
            },
        ),
    };
    let spans = match app.slot_state(slot) {
        SlotState::Loading if ctx.loading_shown => vec![
            Span::raw(indent),
            Span::styled(
                format!("loading {what}…"),
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            ),
        ],
        SlotState::Failed(error) => {
            let reason = error.message.lines().next().unwrap_or_default();
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(
                    format!("failed to load {what}: {reason}"),
                    Style::default().fg(theme.error),
                ),
            ];
            if let Some(key) = &ctx.retry_key {
                spans.push(Span::styled(
                    format!("  ({key} to retry)"),
                    Style::default().fg(theme.muted),
                ));
            }
            spans
        }
        SlotState::Loading | SlotState::Settled => Vec::new(),
    };
    vec![Line::from(spans)]
}

/// Render one row of whichever view is showing.
fn render_row(
    ctx: &RowContext<'_>,
    row_idx: usize,
    search: Option<&SearchRender<'_>>,
) -> RenderedRow {
    let app = ctx.app;
    let theme = &ctx.config.theme;
    let is_cursor = row_idx == app.cursor.raw();
    let lines = match app.rows[row_idx] {
        DisplayRow::CommitNode { entry_idx } => {
            let node = &app.dag.nodes[entry_idx];
            let flags = RenderFlags {
                is_source: ctx.is_source(entry_idx),
                is_selected: ctx.is_marked(entry_idx),
                in_visual: app.is_in_visual_commit_range(entry_idx),
            };
            render_commit_item(
                &node.graph.node,
                &node.graph.cont,
                &node.commit,
                app.is_commit_unfolded(entry_idx)
                    .then(|| node.files.stats())
                    .flatten(),
                &flags,
                search,
                ctx.config,
            )
        }
        DisplayRow::DescriptionLine {
            entry_idx,
            line_idx,
        } => render_description_line(ctx, entry_idx, line_idx, search),
        DisplayRow::GraphLink {
            entry_idx,
            line_idx,
        } => render_graph_link(ctx, entry_idx, line_idx, search),
        DisplayRow::FileChange { owner, file_idx } => {
            let file = app
                .file(owner, file_idx)
                .expect("visible file row must be loaded");
            // Selections and visual ranges feed commit-scoped commands, so
            // only the DAG's files take part.
            let (sel_state, in_visual) = match owner {
                FileOwner::Dag(entry_idx) => (
                    app.file_selection_state(entry_idx, file_idx),
                    app.is_in_visual_file_range(entry_idx, file_idx),
                ),
                FileOwner::EvoLog(_) | FileOwner::Interdiff => (FileSelectionState::None, false),
            };
            render_file_line(
                file,
                app.is_file_unfolded(owner, file_idx),
                sel_state,
                in_visual,
                search,
                theme,
            )
        }
        row @ DisplayRow::DiffLine {
            owner,
            file_idx,
            line_idx,
        } => {
            let diff_line = app
                .row_diff_line(row)
                .expect("visible diff row must be loaded");
            let flags = match owner {
                FileOwner::Dag(entry_idx) => RenderFlags {
                    is_source: false,
                    is_selected: app.is_line_selected(entry_idx, file_idx, line_idx),
                    in_visual: app.is_in_visual_range(
                        entry_idx,
                        file_idx,
                        line_idx,
                        RowIdx::new(row_idx),
                    ),
                },
                FileOwner::EvoLog(_) | FileOwner::Interdiff => RenderFlags::default(),
            };
            render_diff_line(
                diff_line,
                app.show_line_numbers,
                app.diff_underline,
                &flags,
                search,
                theme,
                ctx.tab_spaces,
            )
        }
        DisplayRow::BookmarkItem { bookmark_idx } => render_bookmark_item(
            &app.views.bookmark_entries[bookmark_idx.raw()],
            search,
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
            render_bookmark_remote_target(target, search, theme)
        }
        DisplayRow::BookmarkSeparator => vec![Line::from(Span::styled(
            "─".repeat(ctx.width),
            Style::default().fg(theme.muted),
        ))],
        DisplayRow::TagItem { tag_idx } => match app.views.tag_entries.get(tag_idx.raw()) {
            Some(entry) => render_tag_item(entry, search, theme),
            None => vec![Line::raw("")],
        },
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
            render_tag_remote_target(target, search, theme)
        }
        DisplayRow::OpLogItem { op_log_idx } => match app.op_log.entries.get(op_log_idx.raw()) {
            Some(entry) => render_op_log_item(entry, search, theme),
            None => vec![Line::raw("")],
        },
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
            search,
            theme,
        ),
        DisplayRow::OpLogLoadMore => vec![Line::from(vec![
            Span::styled("  [Tab] ", Style::default().fg(theme.accent)),
            Span::styled("Load more…", Style::default().fg(theme.muted)),
        ])],
        DisplayRow::EvoLogItem { evolog_idx } => match app.evolog.entries.get(evolog_idx.raw()) {
            Some(entry) => render_evolog_item(entry, search, ctx.config),
            None => vec![Line::raw(""), Line::raw("")],
        },
        DisplayRow::EvoLogGraphLink {
            evolog_idx,
            line_idx,
        } => render_simple_graph_link(
            app.evolog
                .entries
                .get(evolog_idx.raw())
                .and_then(|e| e.graph.extra.get(line_idx.raw())),
            search,
            theme,
        ),
        DisplayRow::WorkspaceItem { workspace_idx } => {
            match app.views.workspace_entries.get(workspace_idx.raw()) {
                Some(entry) => render_workspace_item(entry, search, theme),
                None => vec![Line::raw("")],
            }
        }
        DisplayRow::RemoteItem { remote_idx } => match app.views.remotes.get(remote_idx.raw()) {
            Some(entry) => {
                let name_width = remote_name_width(&app.views.remotes);
                render_remote_item(entry, name_width, search, theme)
            }
            None => vec![Line::raw("")],
        },
        DisplayRow::RemotePushUrl { remote_idx } => match app.views.remotes.get(remote_idx.raw()) {
            Some(entry) => {
                let name_width = remote_name_width(&app.views.remotes);
                render_remote_push_url(entry, name_width, search, theme)
            }
            None => vec![Line::raw("")],
        },
        DisplayRow::CommandLogItem { log_idx } => match app.command_log.get(log_idx) {
            Some(entry) => render_command_log_item(entry, search, theme),
            None => vec![Line::raw("")],
        },
        DisplayRow::CommandLogDetail { log_idx, line_idx } => match app.command_log.get(log_idx) {
            Some(entry) => render_command_log_detail(entry, line_idx.raw(), theme),
            None => vec![Line::raw("")],
        },
        DisplayRow::ConflictHeader {
            entry_idx,
            file_idx,
            hunk_idx,
        } => render_conflict_header(
            app,
            ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx,
            },
            theme,
        ),
        DisplayRow::ConflictTerm {
            entry_idx,
            file_idx,
            hunk_idx,
            term_idx,
            line_idx,
        } => render_conflict_term(
            app,
            ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx,
            },
            term_idx,
            line_idx,
            app.diff_underline,
            search,
            theme,
            ctx.tab_spaces,
        ),
        DisplayRow::ConflictContext {
            entry_idx,
            file_idx,
            hunk_idx,
            line_idx,
        } => render_conflict_context(
            app,
            ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx,
            },
            line_idx,
            theme,
        ),
        DisplayRow::ConflictGap {
            entry_idx,
            file_idx,
            hunk_idx,
        } => render_conflict_gap(
            app,
            ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx,
            },
            theme,
        ),
        DisplayRow::ConflictEdited {
            entry_idx,
            file_idx,
            hunk_idx,
            line_idx,
        } => render_conflict_edited(
            app,
            ConflictHunkRef {
                entry_idx,
                file_idx,
                hunk_idx,
            },
            line_idx,
            theme,
        ),
        DisplayRow::InterdiffHeader => render_interdiff_header(ctx, search),
        DisplayRow::AnnotateLine { line_idx } => {
            return render_annotate_line(ctx, line_idx, is_cursor, search);
        }
        DisplayRow::AnnotateDetail {
            line_idx,
            detail_idx,
        } => render_annotate_detail(ctx, line_idx, detail_idx, is_cursor, search),
        DisplayRow::Placeholder(slot) => render_placeholder_row(ctx, slot),
    };
    lines.into()
}
