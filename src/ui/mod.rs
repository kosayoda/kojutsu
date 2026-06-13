mod layout;
mod list;
mod overlay;
mod search;
mod spans;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::app::{App, AppMode, TargetMode};
use crate::keymap::{self, Keymaps};
use crate::theme::Config;

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &mut App, keymaps: &Keymaps, config: &Config) {
    let theme = &config.theme;
    // Use a single-line header if both repo and revset fit on one line.
    let single_line_len = "repository: ".len()
        + app.repo_root.len()
        + layout::HEADER_SEP.len()
        + "revset: ".len()
        + app.revset.current.len();
    let single_line = single_line_len <= frame.area().width as usize;
    let base_height: u16 = if single_line { 1 } else { 2 };
    let show_ws_filter = app.active_view == crate::app::ActiveView::Operations
        && !app.op_log.workspace_filter.is_empty();
    let annotate_header_lines: u16 = if app.active_view == crate::app::ActiveView::Annotate {
        if app.annotate.history.is_empty() {
            1
        } else {
            2
        }
    } else {
        0
    };
    let header_height = base_height + if show_ws_filter { 1 } else { 0 } + annotate_header_lines;

    let [header_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    app.last_header_height = header_height;
    app.last_list_height = main_area.height;
    layout::draw_header(frame, header_area, app, theme, single_line);
    list::draw_list(frame, main_area, app, config);
    layout::draw_status_bar(frame, status_area, app, theme);

    // Overlays render on top of the main + status area (bottom-aligned).
    let overlay_base = Rect {
        x: main_area.x,
        y: main_area.y,
        width: main_area.width,
        height: main_area.height + status_area.height,
    };

    // Compute context flags before borrowing app.mode mutably.
    let has_file_context = crate::input::has_file_context(app);
    let has_conflict_context = crate::input::has_conflict_context(app);
    let submenu_suffix = app.selection.submenu_suffix();
    let selection_active = app.selection.is_active();
    let selection_kind = app.selection.kind();

    match &mut app.mode {
        AppMode::Normal | AppMode::Jump { .. } => {}
        AppMode::Submenu {
            key,
            label,
            children,
            flags,
        } => {
            // Calculate how many lines we need by simulating the flow layout.
            let inner_width = overlay_base.width.saturating_sub(2).max(1) as usize;
            let pair_widths: Vec<usize> = children
                .iter()
                .filter(|(_, child)| !matches!(child, keymap::TrieNode::Toggle { .. }))
                .map(|(key_node, child)| {
                    let key_str = keymap::display_key(key_node);
                    let desc = match child {
                        keymap::TrieNode::Action { description, .. } => description.as_str(),
                        keymap::TrieNode::Prefix { label, .. } => label.as_str(),
                        _ => "",
                    };
                    // "(key) desc"
                    key_str.len() + 2 + 1 + desc.len()
                })
                .collect();
            let mut content_lines: usize = 1;
            let mut line_width: usize = 0;
            for &pw in &pair_widths {
                let sep = if line_width == 0 { 0 } else { 2 };
                if line_width > 0 && line_width + sep + pw > inner_width {
                    content_lines += 1;
                    line_width = pw;
                } else {
                    line_width += sep + pw;
                }
            }
            let area = overlay_area(overlay_base, content_lines as u16 + 1);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_submenu(
                frame,
                area,
                key,
                label,
                children,
                *flags,
                &keymaps.registry,
                submenu_suffix,
                selection_active,
                selection_kind,
                has_file_context,
                has_conflict_context,
                theme,
            );
        }
        AppMode::CommandOutput {
            command,
            command_parts,
            output,
            success,
            ..
        } => {
            let output_lines = output.iter().filter(|&&b| b == b'\n').count().max(1);
            let height = (output_lines as u16 + 3)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_command_output(
                frame,
                area,
                command,
                command_parts.as_deref(),
                output,
                *success,
                theme,
            );
        }
        AppMode::Help { scroll } => {
            let groups = match &app.pre_overlay_mode {
                Some(AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. }) => {
                    keymap::select_mode_help_entries()
                }
                _ => keymap::help_entries(
                    keymaps.for_view(app.active_view),
                    &keymaps.registry,
                    app.revset.presets,
                ),
            };
            let (left, right) = overlay::balance_help_groups(&groups);
            let left_h: usize = left.iter().map(|(_, e)| e.len() + 1).sum();
            let right_h: usize = right
                .iter()
                .enumerate()
                .map(|(i, (_, e))| e.len() + 1 + if i > 0 { 1 } else { 0 })
                .sum();
            let max_col = left_h.max(right_h);
            let height = (max_col as u16 + 2)
                .min(overlay_base.height * 7 / 10)
                .max(4);
            // Clamp scroll to content that doesn't fit — write back so the
            // stored value never drifts past the end.
            // Block has Borders::TOP only (no bottom), so inner height = height - 1.
            let visible_rows = height.saturating_sub(1);
            let max_scroll = (max_col as u16).saturating_sub(visible_rows);
            *scroll = (*scroll).min(max_scroll);
            let scroll = *scroll;
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_help(frame, area, app, &left, &right, scroll, theme);
        }
        AppMode::TextInput { prompt, input, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_text_input(frame, area, prompt, input, theme);
        }
        AppMode::SearchInput => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_search_input(frame, area, app, theme);
        }
        AppMode::TargetSelect {
            prompt,
            source,
            target_mode,
            ..
        } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            let multi = matches!(target_mode, TargetMode::Multi { .. });
            overlay::draw_target_select(frame, area, prompt, source.as_str(), multi, theme);
        }
        AppMode::CommitSelect { pending, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_commit_select(frame, area, pending.prompt(), theme);
        }
        AppMode::FollowUp { prompt, options } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_follow_up(frame, area, prompt, options, theme);
        }
        AppMode::CommandRunning { command_parts, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_command_running(frame, area, command_parts, theme);
        }
        AppMode::SelectFromList(s) => {
            let height = (s.filtered_indices.len() as u16 + 2)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_select_list(
                frame,
                area,
                &s.title,
                &s.items,
                &s.filtered_indices,
                &s.match_positions,
                s.cursor,
                &mut s.scroll_offset,
                &s.marked,
                s.multi,
                &s.filter,
                s.filtering,
                theme,
            );
        }
    }
}

/// Compute an overlay area at the bottom of `area` with the given height.
fn overlay_area(area: Rect, height: u16) -> Rect {
    let h = height.min(area.height);
    Rect {
        x: area.x,
        y: area.y + area.height - h,
        width: area.width,
        height: h,
    }
}
