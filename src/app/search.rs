use tui_input::Input;

use super::{App, AppMode};
use crate::idx::RowIdx;
use crate::types::{DisplayRow, SearchFocus, SearchScopes, SearchState};

/// Match the description and change_id fields shared by bookmark, tag, and
/// workspace entries.
///
/// IDs are matched against their whole text rather than the few characters on
/// screen, so a pasted or copied ID finds its commit. Every match against the
/// displayed form is still a match against the full one, which is a prefix of
/// it: this only widens what is findable.
fn ref_entry_matches(
    description: Option<&str>,
    change_id: Option<&crate::dag::ShortId>,
    scopes: SearchScopes,
    contains: &dyn Fn(&str) -> bool,
) -> bool {
    (scopes.contains(SearchScopes::DESCRIPTION) && description.is_some_and(contains))
        || (scopes.contains(SearchScopes::CHANGE_ID)
            && change_id.is_some_and(|c| contains(c.full())))
}

/// A search query, ready to match: smart-case, so a query with a capital
/// letter in it matches case, and one without matches either. Folding is
/// Unicode-aware, and every match is a byte range of the text searched, so
/// highlighting can cut the original text there.
pub struct SearchQuery {
    /// Lowercased unless `case_sensitive`.
    text: String,
    case_sensitive: bool,
}

impl SearchQuery {
    pub fn new(query: &str) -> Self {
        let case_sensitive = query.chars().any(char::is_uppercase);
        let text = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        Self {
            text,
            case_sensitive,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether `haystack` contains the query. An empty query is in
    /// everything.
    pub fn matches(&self, haystack: &str) -> bool {
        self.find(haystack).is_some()
    }

    /// Where the query first occurs in `haystack`, as a byte range of
    /// `haystack` itself. An empty query matches an empty range at the start.
    pub fn find(&self, haystack: &str) -> Option<std::ops::Range<usize>> {
        let needle = self.text.as_str();
        if self.case_sensitive || needle.is_empty() {
            return haystack.find(needle).map(|at| at..at + needle.len());
        }
        if haystack.is_ascii() && needle.is_ascii() {
            return haystack
                .as_bytes()
                .windows(needle.len())
                .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
                .map(|at| at..at + needle.len());
        }
        haystack
            .char_indices()
            .find_map(|(start, _)| Some(start..self.folded_match_end(&haystack[start..])? + start))
    }

    /// Where a match starting at the beginning of `text` ends, comparing
    /// each character's lowercase form, which can be more than one character
    /// long, against the lowercased query. A match ends on a character
    /// boundary of `text`.
    fn folded_match_end(&self, text: &str) -> Option<usize> {
        let mut needle = self.text.chars().peekable();
        for (offset, c) in text.char_indices() {
            for lower in c.to_lowercase() {
                if needle.next() != Some(lower) {
                    return None;
                }
            }
            if needle.peek().is_none() {
                return Some(offset + c.len_utf8());
            }
        }
        None
    }
}

impl App {
    pub fn begin_search(&mut self) {
        let started_on = self.cursor_row();
        match &mut self.search {
            Some(search) => {
                search.started_on = started_on;
                search.focus = SearchFocus::Query;
            }
            None => {
                self.search = Some(SearchState::new(started_on));
            }
        }
        self.recompute_search_matches();
        self.enter_overlay(AppMode::SearchInput);
    }

    /// End the search and put the cursor back where it started.
    pub fn cancel_search(&mut self) {
        if let Some(search) = &self.search {
            self.return_to_row(search.started_on);
        }
        self.finish_search();
    }

    /// End the search, leaving the cursor on the match it reached: Enter in
    /// the query, or Esc once back in normal mode.
    pub fn finish_search(&mut self) {
        self.search = None;
        self.exit_overlay();
    }

    pub fn search_next(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let next = match search.current_match {
            Some(i) => (i + 1) % search.matches.len(),
            None => 0,
        };
        search.current_match = Some(next);
        self.cursor = search.matches[next];
    }

    pub fn search_prev(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let prev = match search.current_match {
            Some(0) | None => search.matches.len() - 1,
            Some(i) => i - 1,
        };
        search.current_match = Some(prev);
        self.cursor = search.matches[prev];
    }

    pub fn is_match(&self, row_idx: RowIdx) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.binary_search(&row_idx).is_ok())
    }

    pub fn toggle_search_scope(&mut self, flag: SearchScopes) {
        if self.search.is_some() {
            self.search_scopes_mut().toggle(flag);
            self.recompute_search_matches();
        }
    }

    pub fn toggle_search_focus(&mut self) {
        if let Some(search) = &mut self.search {
            search.focus = match search.focus {
                SearchFocus::Query => SearchFocus::Scopes,
                SearchFocus::Scopes => SearchFocus::Query,
            };
        }
    }

    pub fn update_search_input(&mut self, input: Input) {
        if let Some(search) = &mut self.search {
            search.input = input;
        }
        self.recompute_search_matches();
    }

    fn recompute_search_matches(&mut self) {
        // Matches are looked for from where the search began, or from the
        // cursor if that row is gone.
        let preferred = self
            .search
            .as_ref()
            .and_then(|s| s.started_on)
            .and_then(|row| self.position_of(row))
            .unwrap_or(self.cursor);
        self.recompute_search_matches_at(preferred, true);
    }

    /// Recompute search match indices without moving the cursor.
    /// Used after DAG refresh to keep highlights valid.
    pub(crate) fn refresh_search_matches(&mut self) {
        self.recompute_search_matches_at(self.cursor, false);
    }

    fn recompute_search_matches_at(&mut self, preferred: RowIdx, move_cursor: bool) {
        let Some(search) = &self.search else {
            return;
        };
        let query = SearchQuery::new(search.query());
        let scopes = self.search_scopes();

        let mut matches = Vec::new();
        if !query.is_empty() && !scopes.is_empty() {
            for row_idx in 0..self.rows.len() {
                if self.row_matches(row_idx, &query, scopes) {
                    matches.push(RowIdx::new(row_idx));
                }
            }
        }

        let current_match = if matches.is_empty() {
            None
        } else {
            Some(
                matches
                    .iter()
                    .position(|&row| row >= preferred)
                    .unwrap_or(0),
            )
        };

        if let Some(search) = &mut self.search {
            search.matches = matches;
            search.current_match = current_match;
            if move_cursor && let Some(idx) = current_match {
                self.cursor = search.matches[idx];
            }
        }
    }

    fn row_matches(&self, row_idx: usize, query: &SearchQuery, scopes: SearchScopes) -> bool {
        let contains = |haystack: &str| query.matches(haystack);

        match &self.rows[row_idx] {
            DisplayRow::CommitNode { entry_idx } => {
                let commit = &self.dag.nodes[*entry_idx].commit;
                (scopes.contains(SearchScopes::CHANGE_ID)
                    && (contains(commit.change_id.full())
                        || commit.change_id_suffix().is_some_and(|n| {
                            contains(&format!("{}/{n}", commit.change_id.full()))
                        })))
                    || (scopes.contains(SearchScopes::COMMIT_ID)
                        && contains(commit.commit_id.full()))
                    || (scopes.contains(SearchScopes::DESCRIPTION)
                        && commit.description.as_deref().is_some_and(contains))
                    || (scopes.contains(SearchScopes::AUTHOR)
                        && (contains(commit.author.name.as_str())
                            || contains(commit.author.email.as_str())))
                    || (scopes.contains(SearchScopes::BOOKMARK)
                        && (commit.bookmarks.iter().any(|b| contains(b.name.as_str()))
                            || commit
                                .remote_bookmarks
                                .iter()
                                .any(|b| contains(&format!("{}@{}", b.name, b.remote)))))
                    || (scopes.contains(SearchScopes::TAG)
                        && commit.tags.iter().any(|t| contains(t.as_str())))
            }
            DisplayRow::FileChange { owner, file_idx } => {
                scopes.contains(SearchScopes::PATH_COMMAND)
                    && self
                        .file(*owner, *file_idx)
                        .is_some_and(|file| contains(file.path.as_str()))
            }
            row @ DisplayRow::DiffLine {
                owner, file_idx, ..
            } => {
                (scopes.contains(SearchScopes::LINE)
                    && self
                        .row_diff_line(*row)
                        .is_some_and(|line| contains(line.content.as_str())))
                    || (scopes.contains(SearchScopes::PATH_COMMAND)
                        && self
                            .file(*owner, *file_idx)
                            .is_some_and(|file| contains(file.path.as_str())))
            }
            DisplayRow::GraphLink { .. } => false,
            DisplayRow::BookmarkItem { bookmark_idx } => {
                let Some(entry) = self.views.bookmark_entries.get(bookmark_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::BOOKMARK)
                    && (contains(entry.name.as_str())
                        || entry.kind.remote().is_some_and(|r| contains(r.as_str()))))
                    || ref_entry_matches(
                        entry.description.as_deref(),
                        entry.change_id.as_ref(),
                        scopes,
                        &contains,
                    )
            }
            DisplayRow::BookmarkSeparator
            | DisplayRow::BookmarkConflictTarget { .. }
            | DisplayRow::BookmarkRemoteTarget { .. } => false,
            DisplayRow::TagItem { tag_idx } => {
                let Some(entry) = self.views.tag_entries.get(tag_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::TAG) && contains(entry.name.as_str()))
                    || ref_entry_matches(
                        entry.description.as_deref(),
                        entry.change_id.as_ref(),
                        scopes,
                        &contains,
                    )
            }
            DisplayRow::OpLogItem { op_log_idx } => {
                let Some(entry) = self.op_log.entries.get(op_log_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::DESCRIPTION) && contains(&entry.description))
                    || (scopes.contains(SearchScopes::PATH_COMMAND)
                        && entry.args.as_deref().is_some_and(contains))
            }
            DisplayRow::DescriptionLine { .. }
            | DisplayRow::TagRemoteTarget { .. }
            | DisplayRow::OpLogDetailLine { .. }
            | DisplayRow::OpLogGraphLink { .. }
            | DisplayRow::OpLogLoadMore
            | DisplayRow::EvoLogGraphLink { .. }
            | DisplayRow::ConflictHeader { .. }
            | DisplayRow::ConflictTerm { .. }
            | DisplayRow::ConflictContext { .. }
            | DisplayRow::ConflictGap { .. }
            | DisplayRow::ConflictEdited { .. }
            | DisplayRow::Placeholder(_) => false,
            DisplayRow::EvoLogItem { evolog_idx } => {
                let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::CHANGE_ID) && contains(entry.change_id.full()))
                    || (scopes.contains(SearchScopes::DESCRIPTION)
                        && entry.description.as_deref().is_some_and(contains))
                    || (scopes.contains(SearchScopes::AUTHOR) && contains(entry.author.as_str()))
            }
            DisplayRow::WorkspaceItem { workspace_idx } => {
                let Some(entry) = self.views.workspace_entries.get(workspace_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::DESCRIPTION) && contains(entry.name.as_str()))
                    || ref_entry_matches(
                        entry.description.as_deref(),
                        entry.change_id.as_ref(),
                        scopes,
                        &contains,
                    )
            }
            DisplayRow::CommandLogItem { log_idx } => {
                let Some(entry) = self.command_log.entries.get(log_idx.raw()) else {
                    return false;
                };
                scopes.contains(SearchScopes::DESCRIPTION) && contains(&entry.summary)
            }
            DisplayRow::InterdiffHeader => {
                scopes.contains(SearchScopes::DESCRIPTION)
                    && self
                        .interdiff
                        .as_ref()
                        .is_some_and(|i| contains(&i.from_label) || contains(&i.to_label))
            }
            DisplayRow::CommandLogDetail { .. } => false,
            DisplayRow::AnnotateLine { line_idx } => {
                let line = self
                    .annotate
                    .lines
                    .loaded()
                    .and_then(|l| l.get(line_idx.raw()));
                if let Some(line) = line {
                    (scopes.contains(SearchScopes::CHANGE_ID) && contains(line.change_id.full()))
                        || (scopes.contains(SearchScopes::AUTHOR) && contains(&line.author))
                        || (scopes.contains(SearchScopes::LINE) && contains(&line.content))
                } else {
                    false
                }
            }
            DisplayRow::AnnotateDetail { line_idx, .. } => {
                let info = self
                    .annotate
                    .lines
                    .loaded()
                    .and_then(|l| l.get(line_idx.raw()))
                    .and_then(|line| self.annotate.commit_info.get(&line.commit_id));
                info.is_some_and(|info| {
                    (scopes.contains(SearchScopes::CHANGE_ID) && contains(info.change_id.full()))
                        || (scopes.contains(SearchScopes::DESCRIPTION)
                            && info.description_lines.iter().any(|l| contains(l)))
                        || (scopes.contains(SearchScopes::AUTHOR)
                            && (contains(&info.author_name) || contains(&info.author_email)))
                })
            }
        }
    }
}

#[cfg(test)]
mod id_match_tests {
    use super::*;
    use crate::dag::ShortId;

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";

    fn matches(query: &str) -> bool {
        let id = ShortId::new(CHANGE_ID);
        let contains = |haystack: &str| haystack.contains(query);
        ref_entry_matches(None, Some(&id), SearchScopes::CHANGE_ID, &contains)
    }

    #[test]
    fn a_query_longer_than_the_displayed_id_still_matches() {
        // The displayed form is 8 chars; a copied ID is 32.
        assert!(matches("uunnomkx"));
        assert!(matches("uunnomkxrqvlyp"));
        assert!(matches(CHANGE_ID));
    }

    #[test]
    fn a_short_query_matches_as_it_always_did() {
        assert!(matches("uu"));
        assert!(matches("nnom"));
    }

    #[test]
    fn an_unrelated_query_does_not_match() {
        assert!(!matches("zzzz"));
    }

    #[test]
    fn the_change_id_scope_still_gates_the_match() {
        let id = ShortId::new(CHANGE_ID);
        let contains = |haystack: &str| haystack.contains("uu");
        assert!(!ref_entry_matches(
            None,
            Some(&id),
            SearchScopes::DESCRIPTION,
            &contains
        ));
    }
}

#[cfg(test)]
mod shifting_rows_tests {
    use super::super::App;
    use super::super::test_support::{entry, load};
    use crate::dag::{DiffSummary, FileChange, LineStats};
    use crate::idx::EntryIdx;
    use crate::types::CommitId;

    /// Rows can open above the cursor while a query is typed (a file list
    /// arriving, say); cancelling returns to the row the search began on,
    /// not to the number it had.
    #[test]
    fn cancelling_returns_to_the_starting_row_after_rows_shift() {
        let mut app = App::for_test();
        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        app.dag.unfolded_commits.insert(CommitId::new("a1"));
        app.rebuild_rows();
        app.cursor = app.row_of_commit(EntryIdx::new(2)).unwrap();
        app.begin_search();

        app.dag.nodes[EntryIdx::new(0)]
            .files
            .set_summary(Ok(DiffSummary {
                files: vec![FileChange::for_test("f")],
                stats: LineStats::default(),
            }));
        app.rebuild_rows();
        app.cancel_search();

        assert_eq!(app.selected_entry_idx(), Some(EntryIdx::new(2)));
    }

    /// Rows can arrive while a query is open; its matches follow them, so
    /// `n` lands on the matching commit and not on whatever row now has its
    /// old number.
    #[test]
    fn matches_follow_their_rows_through_a_rebuild() {
        let mut app = App::for_test();
        let entries = vec![entry('a', "a1"), entry('b', "b1"), entry('c', "c1")];
        load(&mut app, entries, true);
        app.begin_search();
        app.update_search_input(tui_input::Input::new("cc".into()));

        app.dag.unfolded_commits.insert(CommitId::new("a1"));
        app.dag.nodes[EntryIdx::new(0)]
            .files
            .set_summary(Ok(DiffSummary {
                files: vec![FileChange::for_test("f"), FileChange::for_test("g")],
                stats: LineStats::default(),
            }));
        app.rebuild_rows();
        app.cursor = app.row_of_commit(EntryIdx::new(0)).unwrap();
        app.search_next();

        assert_eq!(app.selected_entry_idx(), Some(EntryIdx::new(2)));
    }
}

#[cfg(test)]
mod query_tests {
    use super::SearchQuery;

    fn find(query: &str, haystack: &str) -> Option<std::ops::Range<usize>> {
        SearchQuery::new(query).find(haystack)
    }

    #[test]
    fn lowercase_matches_any_case() {
        assert_eq!(find("abc", "xABCx"), Some(1..4));
        assert_eq!(find("über", "ÜBER alles"), Some(0..5));
        assert_eq!(find("é", "CAFÉ"), Some(3..5));
    }

    /// A capital, ASCII or not, makes the query match case.
    #[test]
    fn a_capital_makes_it_case_sensitive() {
        assert_eq!(find("Abc", "xabcx"), None);
        assert_eq!(find("É", "café"), None);
        assert_eq!(find("É", "CAFÉ"), Some(3..5));
    }

    /// Lowercasing can change a character's length in bytes (`İ` is two,
    /// its lowercase three); ranges are of the text searched, not of a
    /// lowercased copy.
    #[test]
    fn ranges_are_of_the_original_text() {
        assert_eq!(find("é", "İé"), Some(2..4));
        assert_eq!(find("x", "İstanbul x"), Some(10..11));
        assert_eq!(find("i\u{307}x", "İx"), Some(0..3));
    }

    #[test]
    fn an_empty_query_is_in_everything() {
        assert!(SearchQuery::new("").matches("anything"));
        assert_eq!(find("", "abc"), Some(0..0));
    }
}
