use itertools::Itertools;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use super::search::*;
use super::spans::*;
use crate::app::{App, AppMode};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, LineStats};
use crate::theme::{Config, Theme};
use crate::types::{DisplayRow, FileSelectionState, SearchScopes};

/// Push `+N -M` spans for line stats, skipping zeros.
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

pub(super) fn draw_list(frame: &mut Frame, area: Rect, app: &mut App, config: &Config) {
    let theme = &config.theme;
    // If in target selection mode, get the source change_id for highlighting.
    let target_select_source: Option<&str> = match &app.mode {
        AppMode::TargetSelect { source, .. } => Some(source.as_str()),
        _ => None,
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

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            if row_idx < vis_start || row_idx >= vis_end {
                // Placeholder must match the real item's line count so
                // ratatui's scroll offset stays correct (CommitNode = 2 lines).
                return match row {
                    DisplayRow::CommitNode { .. } => {
                        ListItem::new(vec![Line::raw(""), Line::raw("")])
                    }
                    _ => ListItem::new(""),
                };
            }
            let row_search = search_ctx.as_ref().map(|ctx| SearchRender {
                row_state: search_row_state(app, row_idx),
                ..*ctx
            });
            match row {
                DisplayRow::CommitNode { entry_idx } => {
                    let node = &app.nodes[*entry_idx];
                    let entry = node;
                    let gl = &node.graph;
                    let graph_node = gl.node.as_str();
                    let graph_cont = gl.cont.as_str();
                    let flags = RenderFlags {
                        is_source: target_select_source
                            .is_some_and(|src| src == entry.commit.unique_prefix().as_str()),
                        is_selected: app.is_commit_selected(*entry_idx),
                        in_visual: app.is_in_visual_commit_range(*entry_idx),
                    };
                    render_commit_item(
                        graph_node,
                        graph_cont,
                        &entry.commit,
                        app.is_commit_unfolded(*entry_idx)
                            .then(|| app.commit_stats(*entry_idx))
                            .flatten(),
                        &flags,
                        row_search.as_ref(),
                        config,
                    )
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
                    let mut spans = vec![search_gutter(
                        row_search
                            .as_ref()
                            .map(|s| s.row_state)
                            .unwrap_or(SearchRowState::None),
                        theme,
                    )];
                    // Continue visual + selection bars through graph links.
                    let is_selected = app.is_commit_selected(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    spans.push(if in_visual {
                        Span::styled("│", Style::default().fg(theme.accent))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(if is_selected {
                        Span::styled("▎", Style::default().fg(theme.selection))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(Span::styled(
                        graph_str.to_string(),
                        Style::default().fg(theme.muted),
                    ));
                    ListItem::new(Line::from(spans))
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
                    )
                }
            }
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
) -> ListItem<'static> {
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

    // Graph prefix (properly padded by the renderer).
    // Split into glyph characters vs connector characters for coloring.
    for (is_glyph, group) in graph_node
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
        let span = &graph_node[start..end];
        if is_glyph {
            line1.push(Span::styled(span.to_string(), graph_style));
        } else {
            line1.push(Span::styled(
                span.to_string(),
                Style::default().fg(theme.muted),
            ));
        }
    }

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
        Style::default().fg(theme.selection),
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
        .fg(theme.change_id)
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
    let remote_bm_style = Style::default().fg(theme.change_id);
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

    ListItem::new(vec![Line::from(line1), Line::from(line2)])
}

fn render_file_line(
    file: &FileChange,
    is_unfolded: bool,
    sel_state: FileSelectionState,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> ListItem<'static> {
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

    let mut spans = vec![search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
        theme,
    )];
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
        push_searchable(&mut spans, new_mid, SearchScopes::PATH, text, search);
        spans.push(Span::styled("}", muted));
        if !suffix.is_empty() {
            spans.push(Span::styled(suffix.to_string(), text));
        }
    } else {
        push_searchable(
            &mut spans,
            file.path.as_str(),
            SearchScopes::PATH,
            Style::default().fg(theme.text),
            search,
        );
    }
    push_line_stats(&mut spans, file.stats, true, theme);
    ListItem::new(Line::from(spans))
}

fn render_diff_line(
    diff_line: &DiffLine,
    show_line_numbers: bool,
    flags: &RenderFlags,
    search: Option<&SearchRender<'_>>,
    theme: &Theme,
) -> ListItem<'static> {
    let (marker, style) = match diff_line.kind {
        DiffLineKind::Header => (" ", Style::default().fg(theme.change_id)),
        DiffLineKind::Context => (" ", Style::default().fg(theme.muted)),
        DiffLineKind::Added => ("+", Style::default().fg(theme.added)),
        DiffLineKind::Removed => ("-", Style::default().fg(theme.error)),
    };

    let line_num_style = Style::default().fg(theme.muted);

    let mut spans = vec![search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
        theme,
    )];
    // Left margin: visual range bar │ + selection indicator ▎.
    let is_selectable = diff_line.kind.is_selectable();
    if is_selectable {
        let bar = if flags.in_visual { "│" } else { " " };
        let sel = if flags.is_selected { "▎" } else { " " };
        spans.push(Span::styled(bar, Style::default().fg(theme.accent)));
        spans.push(Span::styled(sel, Style::default().fg(theme.selection)));
    } else {
        spans.push(Span::raw("  "));
    }
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
        push_searchable(
            &mut spans,
            &diff_line.content,
            SearchScopes::LINE,
            style,
            search,
        );
    } else {
        // Original layout: fixed indent + marker + content
        let prefix = match diff_line.kind {
            DiffLineKind::Header => "      ",
            DiffLineKind::Context => "       ",
            DiffLineKind::Added => "      +",
            DiffLineKind::Removed => "      -",
        };
        spans.push(Span::styled(prefix, style));
        push_searchable(
            &mut spans,
            &diff_line.content,
            SearchScopes::LINE,
            style,
            search,
        );
    }

    ListItem::new(Line::from(spans))
}

/// Factor out common directory prefix and suffix from a rename pair.
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
