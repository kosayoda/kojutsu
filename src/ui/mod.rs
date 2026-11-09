mod layout;
mod list;
mod overlay;
mod search;
mod spans;
mod views;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, AppMode, FollowUpPrompt};
use crate::config::Config;
use crate::keymap::{self, Keymaps};

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
    let header = layout::header_lines(app, theme, frame.area().width);
    let header_height = header.len() as u16;

    let [header_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Fill(1),
        Constraint::Length(STATUS_AREA_HEIGHT),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header), header_area);
    app.last_header_height = header_height;
    app.last_list_height = main_area.height;
    let retry_key = keymaps
        .for_view(app.active_view)
        .key_for(keymap::AppAction::ToggleFold);
    list::draw_list(frame, main_area, app, config, retry_key);
    layout::draw_status_bar(frame, status_area, app, theme);

    // Overlays render on top of the main + status area (bottom-aligned).
    let overlay_base = Rect {
        x: main_area.x,
        y: main_area.y,
        width: main_area.width,
        height: main_area.height + status_area.height,
    };

    // Compute context flags before borrowing app.mode mutably.
    let availability = crate::input::availability(app);
    let submenu_suffix = app.selection.describe();

    match &mut app.mode {
        AppMode::Normal | AppMode::Jump(_) => {}
        AppMode::Submenu {
            key,
            label,
            children,
            flags,
        } => {
            // A top border over the entries, which are laid out as wide as
            // the panel.
            let lines = overlay::submenu_lines(
                children,
                &keymaps.registry,
                &app.config.revsets.presets,
                availability,
                overlay_base.width as usize,
                theme,
            );
            let area = overlay_area(overlay_base, lines.len() as u16 + 1);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_submenu(
                frame,
                area,
                key,
                label,
                children,
                lines,
                *flags,
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
            picks,
            operation,
            flags,
            origin,
            ..
        } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            let multi = operation.multi_target();
            let toggles = origin.as_ref().map_or(&[][..], |o| &o.toggles[..]);
            let label = operation.label();
            let title = |room: usize| {
                let fixed = format!(" {label} from  ").width();
                let sources = overlay::revisions_label(&picks.sources, room.saturating_sub(fixed));
                format!(" {label} from {sources} ")
            };
            let toggles = overlay::submenu_toggle_spans(toggles, *flags, availability, theme);
            overlay::draw_target_select(frame, area, title, multi, toggles, theme);
        }
        AppMode::CommitSelect { pending, .. } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            overlay::draw_commit_select(frame, area, pending.prompt(), theme);
        }
        AppMode::FollowUp {
            prompt, options, ..
        } => {
            let area = overlay_area(overlay_base, 2);
            frame.render_widget(ratatui::widgets::Clear, area);
            let prompt = |room: usize| match prompt {
                FollowUpPrompt::Text(text) => text.clone(),
                FollowUpPrompt::Picked { operation, picks } => {
                    overlay::picks_prompt(operation, picks, room)
                }
            };
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

#[cfg(test)]
mod narrow_terminal_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::App;
    use crate::input::handle_key;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;

    fn press(app: &mut App, keymaps: &Keymaps, code: KeyCode) {
        let lua = LuaEngine::for_test();
        handle_key(app, keymaps, &lua, KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// Draw the whole screen at every width from 1 to 120 columns, after
    /// `keys` put the app in the mode under test. Help splits into two
    /// columns, so its cuts land at about half the terminal's width.
    fn draw_at_every_width(keys: &[KeyCode]) {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        for width in 1..=120 {
            let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
            for &key in keys {
                press(&mut app, &keymaps, key);
            }
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).expect("terminal");
            terminal
                .draw(|frame| super::draw(frame, &mut app, &keymaps))
                .unwrap_or_else(|e| panic!("drawing at {width} columns: {e}"));
        }
    }

    /// Help descriptions end in a multi-byte `…`; cutting them by bytes
    /// would land inside it at some width.
    #[test]
    fn help_draws_at_any_width() {
        draw_at_every_width(&[KeyCode::Char('?')]);
    }

    #[test]
    fn a_submenu_draws_at_any_width() {
        draw_at_every_width(&[KeyCode::Char('!')]);
    }

    /// A prompt longer than the panel scrolls by display columns, with text
    /// whose characters are wider in bytes than in columns.
    #[test]
    fn a_scrolled_prompt_draws_at_any_width() {
        let mut keys = vec![KeyCode::Char(':')];
        keys.extend("éèê日本語".chars().cycle().take(40).map(KeyCode::Char));
        draw_at_every_width(&keys);
    }

    /// A file list that failed says why, and which key asks again.
    #[test]
    fn a_failed_load_reads_as_a_failure_with_its_retry_key() {
        use crate::dag::DiffTarget;
        use crate::idx::EntryIdx;
        use crate::repo_service::{RepoError, RepoErrorKind, RepoResult};
        use crate::types::CommitId;

        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.toggle_commit_fold(EntryIdx::new(0));
        app.handle_repo_result(RepoResult::DiffSummary {
            target: DiffTarget::Commit(CommitId::new("7bbaa2cb")),
            result: Err(RepoError::new(
                RepoErrorKind::Operation,
                "object not found\nmore detail",
            )),
        });

        let mut terminal = Terminal::new(TestBackend::new(100, 12)).expect("terminal");
        terminal
            .draw(|frame| super::draw(frame, &mut app, &keymaps))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let screen: String = (0..12)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let key = crate::keymap::display_key(&keymap_parser::parse("tab").expect("a key"));
        assert!(
            screen.contains(&format!(
                "failed to load changed files: object not found  ({key} to retry)"
            )),
            "{screen}"
        );
    }
}

#[cfg(test)]
mod remotes_view_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::App;
    use crate::dag::{GitRemote, RemoteEntry, RemoteRefCounts};
    use crate::input::handle_key;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::lua::LuaEngine;
    use crate::types::{ActiveView, RemoteName};

    fn remote(
        name: &str,
        fetch: &str,
        push: Option<&str>,
        bookmarks: (usize, usize),
    ) -> RemoteEntry {
        RemoteEntry {
            remote: GitRemote {
                name: RemoteName::new(name),
                fetch_url: Some(fetch.into()),
                push_url: push.map(Into::into),
            },
            bookmarks: RemoteRefCounts {
                total: bookmarks.0,
                tracked: bookmarks.1,
            },
            tags: RemoteRefCounts::default(),
        }
    }

    /// `8` shows each remote's fetch URL in one column with what it has,
    /// and a separate push URL on its own row beneath, in the same column.
    #[test]
    fn remotes_are_listed_with_their_urls_aligned() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.views.remotes = vec![
            remote(
                "origin",
                "git@example.com:a/b",
                Some("git@push.example.com:a/b"),
                (2, 1),
            ),
            remote("upstream-mirror", "https://example.com/u", None, (1, 0)),
        ];
        let lua = LuaEngine::for_test();
        handle_key(
            &mut app,
            &keymaps,
            &lua,
            KeyEvent::new(KeyCode::Char('8'), KeyModifiers::NONE),
        );
        assert_eq!(app.active_view, ActiveView::Remotes);

        let (width, height) = (100, 10);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::draw(frame, &mut app, &keymaps))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let screen: Vec<String> = (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let row = |text: &str| {
            screen
                .iter()
                .find(|line| line.contains(text))
                .unwrap_or_else(|| panic!("no row with {text:?} in\n{}", screen.join("\n")))
        };
        let column = |line: &str, text: &str| line.find(text).expect("text on its row");

        let origin = row("origin");
        assert!(origin.contains("git@example.com:a/b · 2 bookmarks, 1 tracked"));
        let push = row("push  git@push.example.com:a/b");
        let upstream = row("upstream-mirror");
        assert!(upstream.contains("https://example.com/u · 1 bookmark"));
        assert!(!upstream.contains("tracked"));
        let url_column = column(upstream, "https://");
        assert_eq!(column(origin, "git@example"), url_column);
        assert_eq!(column(push, "git@push"), url_column);
        assert!(screen.iter().any(|line| line.contains("Remotes (2)")));
    }

    /// A password in a remote's URL is never drawn.
    #[test]
    fn a_password_in_a_remote_url_is_redacted() {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.views.remotes = vec![remote(
            "origin",
            "https://me:s3cret@example.com/a/b",
            Some("https://me:s3cret@push.example.com/a/b"),
            (0, 0),
        )];
        app.switch_view(ActiveView::Remotes);

        let (width, height) = (100, 6);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::draw(frame, &mut app, &keymaps))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let screen: String = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .collect();
        assert!(!screen.contains("s3cret"));
        assert!(screen.contains("example.com/a/b"));
        assert!(screen.contains("push.example.com/a/b"));
    }
}

#[cfg(test)]
mod search_hint_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
    use crate::types::ActiveView;

    fn screen(app: &mut App) -> String {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let (width, height) = (100, 8);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| super::draw(frame, app, &keymaps))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .collect()
    }

    /// A view that searches remotes offers the `@name` form until
    /// something is typed; the others never mention it.
    #[test]
    fn the_exact_remote_form_is_offered_where_remotes_are_searched() {
        let hint = "@name for one remote";
        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.switch_view(ActiveView::Bookmarks);
        app.begin_search();
        assert!(screen(&mut app).contains(hint));
        app.update_search_input(tui_input::Input::new("ma".into()));
        assert!(!screen(&mut app).contains(hint));

        let mut app = App::with_test_commit("qpvuntsm", "7bbaa2cb");
        app.begin_search();
        assert!(!screen(&mut app).contains(hint));
    }
}
