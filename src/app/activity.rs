//! What kojutsu is busy with in the background, for the activity indicator.
//!
//! Read off the loading states the app already keeps rather than counted
//! from requests sent and answered: a request whose answer is dropped (a
//! revset load superseded by a newer one, a diff for a commit since
//! rewritten) would leave a count stuck above zero, and the spinner with it.

use std::time::{Duration, Instant};

use super::{App, AppMode, FileTree, Loadable};

/// How long work runs before the indicator shows: most of it finishes in a
/// few milliseconds, and a spinner flashing up for each would be noise.
pub const SHOW_AFTER: Duration = Duration::from_millis(200);

/// How long each spinner frame shows.
pub const FRAME: Duration = Duration::from_millis(100);

/// Something the app is waiting on, most telling first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// A jj command, which has its own panel saying so.
    RunningCommand,
    /// jj working out what can complete the command prompt.
    Completing,
    /// A revset load, before its first commits arrive: the working-copy
    /// snapshot and the evaluation.
    LoadingCommits,
    /// A revset load streaming in, with how many commits have arrived.
    StreamingCommits(usize),
    LoadingOperations,
    LoadingOperation,
    LoadingEvolog,
    Annotating,
    LoadingFile,
    LoadingFiles,
    LoadingDiff,
    LoadingConflicts,
}

impl Activity {
    /// What the status line says about it, if it says anything.
    pub fn label(&self) -> Option<String> {
        let label = match self {
            Self::RunningCommand => return None,
            Self::Completing => "completing…".to_string(),
            Self::LoadingCommits => "loading commits…".to_string(),
            Self::StreamingCommits(count) => format!("loading commits… {count}"),
            Self::LoadingOperations => "loading operations…".to_string(),
            Self::LoadingOperation => "loading operation…".to_string(),
            Self::LoadingEvolog => "loading evolution log…".to_string(),
            Self::Annotating => "annotating…".to_string(),
            Self::LoadingFile => "loading file…".to_string(),
            Self::LoadingFiles => "loading changed files…".to_string(),
            Self::LoadingDiff => "loading diff…".to_string(),
            Self::LoadingConflicts => "loading conflicts…".to_string(),
        };
        Some(label)
    }
}

/// The indicator to draw: a spinner frame, and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indicator {
    pub glyph: Option<char>,
    pub label: Option<String>,
}

fn is_loading<T>(state: &Loadable<T>) -> bool {
    matches!(state, Loadable::Loading)
}

fn tree_activity(tree: &FileTree) -> Option<Activity> {
    if is_loading(tree.summary()) {
        return Some(Activity::LoadingFiles);
    }
    let files = tree.files().map_or(0, <[_]>::len);
    (0..files)
        .any(|i| {
            tree.diff(crate::idx::FileIdx::new(i))
                .is_some_and(is_loading)
        })
        .then_some(Activity::LoadingDiff)
}

impl App {
    /// What the app is waiting on now, if anything.
    pub fn activity(&self) -> Option<Activity> {
        if matches!(self.mode, AppMode::CommandRunning(_)) {
            return Some(Activity::RunningCommand);
        }
        if self.pending_completion.is_some() {
            return Some(Activity::Completing);
        }
        if self.dag.stream.is_some() {
            return Some(Activity::StreamingCommits(self.dag.nodes.len()));
        }
        if is_loading(&self.revset.load_state) {
            return Some(Activity::LoadingCommits);
        }
        if is_loading(&self.op_log.load_state) {
            return Some(Activity::LoadingOperations);
        }
        if self.op_log.details.values().any(is_loading) {
            return Some(Activity::LoadingOperation);
        }
        if is_loading(&self.evolog.load_state) {
            return Some(Activity::LoadingEvolog);
        }
        if is_loading(&self.annotate.lines) {
            return Some(Activity::Annotating);
        }
        if self.pending_file_view.is_some() {
            return Some(Activity::LoadingFile);
        }
        let trees = self
            .dag
            .nodes
            .iter()
            .map(|node| &node.files)
            .chain(self.evolog.files.values())
            .chain(self.interdiff.iter().map(|interdiff| &interdiff.files));
        let mut activity = None;
        for tree in trees {
            match tree_activity(tree) {
                // The file list comes before any diff in it.
                Some(Activity::LoadingFiles) => return Some(Activity::LoadingFiles),
                found @ Some(_) => activity = activity.or(found),
                None => {}
            }
        }
        if activity.is_some() {
            return activity;
        }
        self.dag
            .nodes
            .iter()
            .any(|node| node.conflict_hunks.iter().any(is_loading))
            .then_some(Activity::LoadingConflicts)
    }

    /// Note, as of `now`, whether anything is in flight: once per batch of
    /// events, so the indicator times the whole stretch of work rather than
    /// each request in it.
    pub fn track_activity(&mut self, now: Instant) {
        if self.activity().is_some() {
            self.activity_since.get_or_insert(now);
        } else {
            self.activity_since = None;
        }
    }

    /// The indicator to draw at `now`, once work has gone on long enough to
    /// notice.
    pub fn activity_indicator(&self, now: Instant) -> Option<Indicator> {
        let elapsed = now.saturating_duration_since(self.activity_since?);
        if elapsed < SHOW_AFTER {
            return None;
        }
        let frames = &self.config.spinner;
        let frame = (elapsed.as_millis() / FRAME.as_millis()) as usize;
        // A loading placeholder in the list already says what is loading.
        let placeholder_shown = self.rows.iter().any(|row| {
            matches!(row, crate::types::DisplayRow::Placeholder(slot)
                if matches!(self.slot_state(*slot), super::SlotState::Loading))
        });
        let activity = self.activity()?;
        Some(Indicator {
            glyph: (!frames.is_empty()).then(|| frames[frame % frames.len()]),
            label: activity.label().filter(|_| !placeholder_shown),
        })
    }

    /// Whether work has gone on long enough at `now` to show for it.
    pub fn activity_shown(&self, now: Instant) -> bool {
        self.activity_since
            .is_some_and(|since| now.saturating_duration_since(since) >= SHOW_AFTER)
    }

    /// How long until the screen changes on its own, with nothing else
    /// happening: the indicator appearing, or its next frame. `None` when
    /// idle, so an idle app sleeps until an event.
    pub fn next_activity_redraw(&self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.activity_since?);
        Some(match SHOW_AFTER.checked_sub(elapsed) {
            Some(until_shown) if !until_shown.is_zero() => until_shown,
            _ => FRAME,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loading_app() -> App {
        let mut app = App::for_test();
        app.revset.load_state = Loadable::Loading;
        app
    }

    #[test]
    fn idle_shows_nothing_and_never_wakes() {
        let mut app = App::for_test();
        app.revset.load_state = Loadable::Loaded(());
        let now = Instant::now();
        app.track_activity(now);
        assert_eq!(app.activity(), None);
        assert_eq!(app.activity_indicator(now + SHOW_AFTER * 10), None);
        assert_eq!(app.next_activity_redraw(now), None);
    }

    /// Quick work never flashes the indicator up.
    #[test]
    fn the_indicator_waits_before_showing() {
        let mut app = loading_app();
        let start = Instant::now();
        app.track_activity(start);
        assert_eq!(app.activity_indicator(start + SHOW_AFTER / 2), None);
        // Woken just as it is due.
        assert_eq!(app.next_activity_redraw(start), Some(SHOW_AFTER));
        let shown = app
            .activity_indicator(start + SHOW_AFTER)
            .expect("shown once due");
        assert_eq!(shown.label.as_deref(), Some("loading commits…"));
        assert_eq!(app.next_activity_redraw(start + SHOW_AFTER), Some(FRAME));
    }

    /// A stretch of work is timed from its start, not restarted by each
    /// batch of events within it.
    #[test]
    fn continued_work_keeps_its_start() {
        let mut app = loading_app();
        let start = Instant::now();
        app.track_activity(start);
        app.track_activity(start + SHOW_AFTER);
        assert!(app.activity_indicator(start + SHOW_AFTER).is_some());

        app.revset.load_state = Loadable::Loaded(());
        app.track_activity(start + SHOW_AFTER * 2);
        assert_eq!(app.activity_indicator(start + SHOW_AFTER * 2), None);
    }

    /// Each frame period shows the next glyph, and after the last the
    /// spinner starts over.
    #[test]
    fn the_spinner_steps_through_its_frames() {
        let mut app = loading_app();
        let config = crate::config::Config {
            spinner: vec!['a', 'b', 'c'],
            ..Default::default()
        };
        app.config = std::rc::Rc::new(config);
        let start = Instant::now();
        app.track_activity(start);
        let glyph = |frames_in: u32| {
            app.activity_indicator(start + SHOW_AFTER + FRAME * frames_in)
                .and_then(|indicator| indicator.glyph)
                .expect("a frame is shown")
        };
        let cycle = [glyph(0), glyph(1), glyph(2)];
        let mut seen = cycle.to_vec();
        seen.sort_unstable();
        assert_eq!(seen, ['a', 'b', 'c'], "one cycle shows every frame once");
        assert_eq!(glyph(3), cycle[0]);
        assert_eq!(glyph(4), cycle[1]);
    }

    /// With a placeholder in the list saying it, the status line doesn't
    /// repeat it; the spinner still turns.
    #[test]
    fn a_placeholder_row_takes_over_the_label() {
        let mut app = App::for_test();
        app.request_revset_load(None, crate::repo_service::RevsetLoadKind::Snapshot);
        let start = Instant::now();
        app.track_activity(start);
        let shown = app.activity_indicator(start + SHOW_AFTER).expect("shown");
        assert!(shown.glyph.is_some());
        assert_eq!(shown.label, None);
    }

    /// No frames configured: the label still says what is happening.
    #[test]
    fn no_frames_leaves_the_label() {
        let mut app = loading_app();
        let mut config = crate::config::Config::default();
        config.spinner.clear();
        app.config = std::rc::Rc::new(config);
        let start = Instant::now();
        app.track_activity(start);
        let shown = app.activity_indicator(start + SHOW_AFTER).expect("shown");
        assert_eq!(shown.glyph, None);
        assert!(shown.label.is_some());
    }

    /// Commits `a` and `b`, loaded, those `listed` with their one file, a
    /// conflicted one, listed.
    fn commits_with_files(listed: [bool; 2]) -> App {
        use super::super::test_support::{entry, load};
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], true);
        for (node, listed) in app.dag.nodes.iter_mut().zip(listed) {
            let mut file = crate::dag::FileChange::for_test("f");
            file.has_conflict = true;
            if listed {
                node.files.set_summary(Ok(crate::dag::DiffSummary {
                    files: vec![file],
                    stats: crate::dag::LineStats::default(),
                }));
            }
        }
        app
    }

    /// Of loads under way together, the status line names the one whose
    /// answer the others wait on, wherever it is in the log: a file list
    /// before a diff, a diff before conflicts.
    #[test]
    fn the_load_others_depend_on_is_the_one_named() {
        use crate::idx::{EntryIdx, FileIdx};
        let (a, b, f) = (EntryIdx::new(0), EntryIdx::new(1), FileIdx::new(0));

        let mut app = commits_with_files([true, false]);
        app.dag.nodes[a].files.request_diff(f);
        app.dag.nodes[b].files.request_summary();
        assert_eq!(app.activity(), Some(Activity::LoadingFiles));

        let mut app = commits_with_files([true, true]);
        app.dag.nodes[a].request_conflict_hunks(f);
        app.dag.nodes[b].files.request_diff(f);
        assert_eq!(app.activity(), Some(Activity::LoadingDiff));
    }

    /// While a load streams in, the count is of the commits arrived so
    /// far, chunk by chunk.
    #[test]
    fn a_streaming_load_counts_the_commits_arrived() {
        use super::super::test_support::{chunk, entry, load};
        let mut app = App::for_test();
        load(&mut app, vec![entry('a', "a1"), entry('b', "b1")], false);
        assert_eq!(app.activity(), Some(Activity::StreamingCommits(2)));
        chunk(&mut app, vec![entry('c', "c1")], false);
        assert_eq!(app.activity(), Some(Activity::StreamingCommits(3)));
        chunk(&mut app, Vec::new(), true);
        assert_eq!(app.activity(), None);
    }
}
