mod layout;
mod list;
mod overlay;
mod search;
mod spans;
mod views;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, AppMode, TargetMode};
use crate::keymap::{self, Keymaps};
use crate::theme::Config;

/// Height of the status bar area at the bottom of the screen. Overlays
/// render over the main list plus this area, so input handlers that need
/// overlay geometry derive it from `app.last_list_height` plus this.
pub const STATUS_AREA_HEIGHT: u16 = 2;

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &mut App, keymaps: &Keymaps) {
    // Cloning the handle (not the config) releases `app` for the `&mut`
    // borrows the render path needs.
    let config: std::rc::Rc<Config> = app.config.clone();
    let config = &*config;
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
        Constraint::Length(STATUS_AREA_HEIGHT),
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
    let availability = crate::keymap::Availability {
        selection: app.selection.kinds(),
        on_file: crate::input::has_file_context(app),
        on_conflict: crate::input::has_conflict_context(app),
    };
    let submenu_suffix = app.selection.describe();

    match &mut app.mode {
        AppMode::Normal | AppMode::Jump(_) => {}
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
                availability,
                theme,
            );
        }
        AppMode::CommandOutput(state) => {
            let height = state.overlay_height(overlay_base.height);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_command_output(frame, area, state, theme);
        }
        AppMode::Help { scroll } => {
            let groups = match &app.pre_overlay_mode {
                Some(AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. }) => {
                    keymap::select_mode_help_entries(keymaps.for_view(app.active_view))
                }
                _ => keymap::help_entries(
                    keymaps.for_view(app.active_view),
                    &keymaps.registry,
                    &app.config.revsets.presets,
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
            // Clamp scroll to content that doesn't fit: write back so the
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
            flags,
            target_mode,
            toggles,
            ..
        } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            let multi = matches!(target_mode, TargetMode::Multi { .. });
            let title = format!(" {prompt} from {source} ");
            overlay::draw_target_select(frame, area, &title, multi, toggles, *flags, theme);
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
        AppMode::CommandRunning(state) => {
            let has_partial_line = state.parsed_upto < state.output.len();
            let output_lines = state.parsed_lines.len() + has_partial_line as usize;
            // Top border consumes one row: 3 rows fit the hint + command
            // lines, output grows the overlay beyond that.
            let height = (output_lines as u16 + 3)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_command_running(frame, area, state, theme);
        }
        AppMode::SelectFromList(s) => {
            let height = (s.filtered_indices.len() as u16 + 2)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_select_list(frame, area, s, theme);
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
