mod layout;
mod list;
mod overlay;
mod search;
mod spans;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::app::{App, AppMode};
use crate::keymap::{self, Keymap};

/// Render the full UI into the frame.
pub fn draw(frame: &mut Frame, app: &mut App, keymap: &'static Keymap) {
    // Use a single-line header if both repo and revset fit on one line.
    let single_line_len = "repository: ".len()
        + app.repo_root.len()
        + layout::HEADER_SEP.len()
        + "revset: ".len()
        + app.revset.len();
    let header_height = if single_line_len <= frame.area().width as usize {
        1
    } else {
        2
    };

    let [header_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    app.last_header_height = header_height;
    layout::draw_header(frame, header_area, app);
    list::draw_list(frame, main_area, app);
    layout::draw_status_bar(frame, status_area, app);

    // Overlays render on top of the main + status area (bottom-aligned).
    let overlay_base = Rect {
        x: main_area.x,
        y: main_area.y,
        width: main_area.width,
        height: main_area.height + status_area.height,
    };

    match &mut app.mode {
        AppMode::Normal => {}
        AppMode::Submenu {
            key,
            label,
            children,
            flags,
            error,
        } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_submenu(
                frame,
                area,
                key,
                label,
                children,
                *flags,
                app.selection.submenu_suffix(),
                &app.selection,
                error.as_deref(),
            );
        }
        AppMode::CommandOutput {
            command,
            output,
            success,
        } => {
            let output_lines = output.iter().filter(|&&b| b == b'\n').count().max(1);
            let height = (output_lines as u16 + 3)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_command_output(frame, area, command, output, *success);
        }
        AppMode::Help => {
            let groups = match &app.pre_overlay_mode {
                Some(AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. }) => {
                    keymap::select_mode_help_entries()
                }
                _ => keymap::help_entries(keymap),
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
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_help(frame, area, app, &left, &right);
        }
        AppMode::TextInput { prompt, input, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_text_input(frame, area, prompt, input);
        }
        AppMode::SearchInput => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_search_input(frame, area, app);
        }
        AppMode::TargetSelect { prompt, source, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_target_select(frame, area, prompt, source.as_str());
        }
        AppMode::CommitSelect { pending, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_commit_select(frame, area, pending.prompt());
        }
        AppMode::FollowUp { prompt, options } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_follow_up(frame, area, prompt, options);
        }
        AppMode::SelectFromList {
            title,
            items,
            filtered_indices,
            cursor,
            scroll_offset,
            marked,
            multi,
            filter,
            filtering,
            ..
        } => {
            let height = (filtered_indices.len() as u16 + 2)
                .min(overlay_base.height / 2)
                .max(3);
            let area = overlay_area(overlay_base, height);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_select_list(
                frame,
                area,
                title,
                items,
                filtered_indices,
                *cursor,
                scroll_offset,
                marked,
                *multi,
                filter,
                *filtering,
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
