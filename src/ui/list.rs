use itertools::Itertools;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use super::search::*;
use super::spans::*;
use crate::app::{App, AppMode};
use crate::dag::{CommitInfo, DiffLine, DiffLineKind, FileChange, FileStatus, LineStats};
use crate::types::{DisplayRow, FileSelectionState, SearchScopes};

pub(super) fn draw_list(frame: &mut Frame, area: Rect, app: &mut App) {
    // If in target selection mode, get the source change_id for highlighting.
    let target_select_source: Option<&str> = match &app.mode {
        AppMode::TargetSelect { source, .. } => Some(source.as_str()),
        _ => None,
    };

    let search_ctx = app.search.as_ref().map(|s| SearchRender {
        query: s.query(),
        scopes: s.scopes,
        case_sensitive: s.query().chars().any(|c| c.is_ascii_uppercase()),
        row_state: SearchRowState::None,
    });

    // Only build full ListItems for rows near the visible window.
    // Off-screen rows get a cheap placeholder — ratatui's List still sees
    // the correct total item count for scroll math.
    let offset = app.list_state.offset();
    let vis_start = offset.saturating_sub(20);
    let vis_end = (offset + area.height as usize + 20).min(app.rows.len());

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            if row_idx < vis_start || row_idx >= vis_end {
                return ListItem::new("");
            }
            let row_search = search_ctx.as_ref().map(|ctx| SearchRender {
                row_state: search_row_state(app, row_idx),
                ..*ctx
            });
            match row {
                DisplayRow::CommitNode { entry_idx } => {
                    let entry = &app.entries[*entry_idx];
                    let gl = &app.graph[*entry_idx];
                    let graph_node = gl.node.as_str();
                    let graph_cont = gl.cont.as_str();
                    let is_source = target_select_source
                        .is_some_and(|src| src == entry.commit.unique_prefix().as_str());
                    let is_selected = app.is_commit_selected(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    render_commit_item(
                        graph_node,
                        graph_cont,
                        &entry.commit,
                        app.is_commit_unfolded(*entry_idx)
                            .then(|| app.commit_stats(*entry_idx))
                            .flatten(),
                        is_source,
                        is_selected,
                        in_visual,
                        row_search.as_ref(),
                    )
                }
                DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                } => {
                    let graph_str = app.graph[*entry_idx]
                        .extra
                        .get(line_idx.raw())
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let mut spans = vec![search_gutter(
                        row_search
                            .as_ref()
                            .map(|s| s.row_state)
                            .unwrap_or(SearchRowState::None),
                    )];
                    // Continue visual + selection bars through graph links.
                    let is_selected = app.is_commit_selected(*entry_idx);
                    let in_visual = app.is_in_visual_commit_range(*entry_idx);
                    spans.push(if in_visual {
                        Span::styled("│", Style::default().fg(Color::Cyan))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(if is_selected {
                        Span::styled("▎", Style::default().fg(Color::Yellow))
                    } else {
                        Span::raw(" ")
                    });
                    spans.push(Span::styled(
                        graph_str.to_string(),
                        Style::default().fg(Color::DarkGray),
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
                    render_file_line(file, is_unfolded, sel_state, row_search.as_ref())
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
                    let is_selected = app.is_line_selected(*entry_idx, *file_idx, *line_idx);
                    let in_visual = app.is_in_visual_range(*entry_idx, *file_idx, *line_idx);
                    render_diff_line(
                        diff_line,
                        app.show_line_numbers,
                        is_selected,
                        in_visual,
                        row_search.as_ref(),
                    )
                }
            }
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE))
        .scroll_padding(5)
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(50, 50, 60))
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
    is_source: bool,
    is_selected: bool,
    in_visual: bool,
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
    let graph_color = if is_source {
        Color::Yellow
    } else if c.has_conflict {
        Color::Red
    } else {
        match c.glyph() {
            crate::dag::Glyph::WorkingCopy => Color::Green,
            crate::dag::Glyph::Conflict => Color::Red,
            crate::dag::Glyph::Immutable => Color::Cyan,
            crate::dag::Glyph::Normal => Color::Cyan,
        }
    };

    let graph_style = Style::default().fg(graph_color);
    let search_state = search.map(|s| s.row_state).unwrap_or(SearchRowState::None);

    // --- Line 1: graph  change_id author timestamp bookmarks commit_id ---
    let mut line1: Vec<Span<'static>> = Vec::new();
    line1.push(search_gutter(search_state));

    // Visual range column + selection column + spacer.
    line1.push(if in_visual {
        Span::styled("│", Style::default().fg(Color::Cyan))
    } else {
        Span::raw(" ")
    });
    if is_selected {
        line1.push(Span::styled("▎", Style::default().fg(Color::Yellow)));
    } else if is_source {
        line1.push(Span::styled("►", Style::default().fg(Color::Yellow)));
    } else {
        line1.push(Span::raw(" "));
    }

    // Graph prefix (properly padded by the renderer).
    // Split into glyph characters vs connector characters for coloring.
    for (is_glyph, group) in graph_node
        .char_indices()
        .chunk_by(|&(_, c)| crate::dag::Glyph::try_from(c).is_ok())
        .into_iter()
    {
        let mut iter = group.into_iter();
        let (start, c) = iter.next().unwrap();
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
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // Change ID (prefix bright, rest dimmed; red if divergent)
    let change_color = if c.is_divergent {
        Color::Red
    } else if c.is_hidden {
        Color::White
    } else {
        Color::Magenta
    };
    let change_id_text = if let Some(suffix) = c.change_id_suffix {
        format!("{}/{}", c.change_id.display, suffix)
    } else {
        c.change_id.display.clone()
    };
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::CHANGE_ID)
            && contains_query(&change_id_text, search.query, search.case_sensitive)
        {
            push_highlighted_short_id(
                &mut line1,
                &c.change_id,
                c.change_id_suffix.map(|s| format!("/{s}")),
                change_color,
                search.query,
                search.case_sensitive,
            );
        } else {
            push_short_id(&mut line1, &c.change_id, change_color);
            if let Some(suffix) = c.change_id_suffix {
                line1.push(Span::styled(
                    format!("/{suffix}"),
                    Style::default().fg(change_color),
                ));
            }
        }
    } else {
        push_short_id(&mut line1, &c.change_id, change_color);
        if let Some(suffix) = c.change_id_suffix {
            line1.push(Span::styled(
                format!("/{suffix}"),
                Style::default().fg(change_color),
            ));
        }
    }
    if c.is_divergent {
        line1.push(Span::styled(" (divergent)", Style::default().fg(Color::Red)));
    }
    line1.push(Span::raw(" "));

    // Author
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::AUTHOR)
            && contains_query(c.author.email.as_str(), search.query, search.case_sensitive)
        {
            push_highlighted(
                &mut line1,
                c.author.email.as_str(),
                search.query,
                Style::default().fg(Color::Yellow),
                search.case_sensitive,
            );
        } else {
            line1.push(Span::styled(
                c.author.email.clone(),
                Style::default().fg(Color::Yellow),
            ));
        }
    } else {
        line1.push(Span::styled(
            c.author.email.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }
    line1.push(Span::raw(" "));

    // Timestamp
    let formatted = c.author.timestamp.strftime("%Y-%m-%d %H:%M:%S").to_string();
    line1.push(Span::styled(
        formatted,
        Style::default().fg(Color::DarkGray),
    ));

    // Commit ID (at end, like jj log -- prefix bright, rest dimmed)
    line1.push(Span::raw(" "));
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::COMMIT_ID)
            && contains_query(
                c.commit_id.display.as_str(),
                search.query,
                search.case_sensitive,
            )
        {
            push_highlighted_short_id(
                &mut line1,
                &c.commit_id,
                None,
                Color::Blue,
                search.query,
                search.case_sensitive,
            );
        } else {
            push_short_id(&mut line1, &c.commit_id, Color::Blue);
        }
    } else {
        push_short_id(&mut line1, &c.commit_id, Color::Blue);
    }

    // Local bookmarks (with * suffix if dirty)
    for bm in &c.bookmarks {
        line1.push(Span::raw(" "));
        let display = if bm.is_dirty {
            format!("{}*", bm.name)
        } else {
            bm.name.clone()
        };
        let style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::BOOKMARK)
                && contains_query(&display, search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line1,
                    &display,
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                line1.push(Span::styled(display, style));
            }
        } else {
            line1.push(Span::styled(display, style));
        }
    }

    // Remote bookmarks (name@remote, shown when no local bookmark covers them)
    for rb in &c.remote_bookmarks {
        line1.push(Span::raw(" "));
        let text = format!("{}@{}", rb.name, rb.remote);
        let style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::BOOKMARK)
                && contains_query(&text, search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line1,
                    &text,
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                line1.push(Span::styled(text, style));
            }
        } else {
            line1.push(Span::styled(text, style));
        }
    }

    // Workspace annotations (non-current workspaces shown as "name@")
    for ws in &c.workspaces {
        if !ws.is_current {
            line1.push(Span::raw(" "));
            line1.push(Span::styled(
                format!("{}@", ws.name),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    if let Some(stats) = line_stats {
        line1.push(Span::raw(" "));
        line1.push(Span::styled(
            format!("+{}", stats.added),
            Style::default().fg(Color::Green),
        ));
        line1.push(Span::raw(" "));
        line1.push(Span::styled(
            format!("-{}", stats.removed),
            Style::default().fg(Color::Red),
        ));
    }

    // Hidden indicator
    if c.is_hidden {
        line1.push(Span::styled(" (hidden)", Style::default().fg(Color::White)));
    }

    // --- Line 2: graph_cont  description ---
    let mut line2: Vec<Span<'static>> = Vec::new();
    line2.push(search_gutter(search_state));
    // Continue visual + selection bars on line 2.
    line2.push(if in_visual {
        Span::styled("│", Style::default().fg(Color::Cyan))
    } else {
        Span::raw(" ")
    });
    line2.push(if is_selected {
        Span::styled("▎", Style::default().fg(Color::Yellow))
    } else {
        Span::raw(" ")
    });

    // Graph continuation prefix (properly padded by the renderer).
    line2.push(Span::styled(
        graph_cont.to_string(),
        Style::default().fg(Color::DarkGray),
    ));

    if let Some(desc) = &c.description {
        if c.has_conflict {
            line2.push(Span::styled("(conflict) ", Style::default().fg(Color::Red)));
        }
        if c.is_empty {
            line2.push(Span::styled(
                "(empty) ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        let desc_style = if c.is_empty {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::DESCRIPTION)
                && contains_query(desc.as_str(), search.query, search.case_sensitive)
            {
                push_highlighted(
                    &mut line2,
                    desc.as_str(),
                    search.query,
                    desc_style,
                    search.case_sensitive,
                );
            } else {
                line2.push(Span::styled(desc.clone(), desc_style));
            }
        } else {
            line2.push(Span::styled(desc.clone(), desc_style));
        }
    } else {
        if c.has_conflict {
            line2.push(Span::styled("(conflict) ", Style::default().fg(Color::Red)));
        }
        let placeholder = if c.is_empty {
            "(empty)"
        } else {
            "(no description set)"
        };
        line2.push(Span::styled(
            placeholder,
            Style::default().fg(Color::DarkGray),
        ));
    }

    ListItem::new(vec![Line::from(line1), Line::from(line2)])
}

fn render_file_line(
    file: &FileChange,
    is_unfolded: bool,
    sel_state: FileSelectionState,
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
    let (marker, color) = if file.has_conflict {
        ("C", Color::Red)
    } else {
        match file.status {
            FileStatus::Added => ("A", Color::Green),
            FileStatus::Modified => ("M", Color::Cyan),
            FileStatus::Deleted => ("D", Color::Red),
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
    )];
    spans.extend(vec![
        Span::styled(
            format!("  {select_char} "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(fold_char, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(
            marker,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let base = Style::default().fg(Color::White);
    if let Some(search) = search {
        if search.scopes.contains(SearchScopes::PATH)
            && contains_query(file.path.as_str(), search.query, search.case_sensitive)
        {
            push_highlighted(
                &mut spans,
                file.path.as_str(),
                search.query,
                base,
                search.case_sensitive,
            );
        } else {
            spans.push(Span::styled(file.path.clone(), base));
        }
    } else {
        spans.push(Span::styled(file.path.clone(), base));
    }
    ListItem::new(Line::from(spans))
}

fn render_diff_line(
    diff_line: &DiffLine,
    show_line_numbers: bool,
    is_selected: bool,
    in_visual: bool,
    search: Option<&SearchRender<'_>>,
) -> ListItem<'static> {
    let (marker, style) = match diff_line.kind {
        DiffLineKind::Header => (" ", Style::default().fg(Color::Magenta)),
        DiffLineKind::Context => (" ", Style::default().fg(Color::DarkGray)),
        DiffLineKind::Added => ("+", Style::default().fg(Color::Green)),
        DiffLineKind::Removed => ("-", Style::default().fg(Color::Red)),
    };

    let line_num_style = Style::default().fg(Color::DarkGray);

    let mut spans = vec![search_gutter(
        search.map(|s| s.row_state).unwrap_or(SearchRowState::None),
    )];
    // Left margin: visual range bar │ + selection indicator ▎.
    let is_selectable =
        diff_line.kind == DiffLineKind::Added || diff_line.kind == DiffLineKind::Removed;
    if is_selectable {
        let bar = if in_visual { "│" } else { " " };
        let sel = if is_selected { "▎" } else { " " };
        spans.push(Span::styled(bar, Style::default().fg(Color::Cyan)));
        spans.push(Span::styled(sel, Style::default().fg(Color::Yellow)));
    } else {
        spans.push(Span::raw("  "));
    }
    if show_line_numbers && diff_line.kind != DiffLineKind::Header {
        // "  {old:>4} {new:>4} {marker}{content}"
        let old = diff_line
            .old_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        let new = diff_line
            .new_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        spans.push(Span::styled(format!("  {old} {new} "), line_num_style));
        spans.push(Span::styled(marker, style));
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::LINE)
                && contains_query(
                    diff_line.content.as_str(),
                    search.query,
                    search.case_sensitive,
                )
            {
                push_highlighted(
                    &mut spans,
                    diff_line.content.as_str(),
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                spans.push(Span::styled(diff_line.content.clone(), style));
            }
        } else {
            spans.push(Span::styled(diff_line.content.clone(), style));
        }
    } else {
        // Original layout: fixed indent + marker + content
        let prefix = match diff_line.kind {
            DiffLineKind::Header => "      ",
            DiffLineKind::Context => "       ",
            DiffLineKind::Added => "      +",
            DiffLineKind::Removed => "      -",
        };
        spans.push(Span::styled(prefix, style));
        if let Some(search) = search {
            if search.scopes.contains(SearchScopes::LINE)
                && contains_query(
                    diff_line.content.as_str(),
                    search.query,
                    search.case_sensitive,
                )
            {
                push_highlighted(
                    &mut spans,
                    diff_line.content.as_str(),
                    search.query,
                    style,
                    search.case_sensitive,
                );
            } else {
                spans.push(Span::styled(diff_line.content.clone(), style));
            }
        } else {
            spans.push(Span::styled(diff_line.content.clone(), style));
        }
    }

    ListItem::new(Line::from(spans))
}
