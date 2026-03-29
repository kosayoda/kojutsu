use tui_input::Input;

use super::{App, AppMode};
use crate::types::{DisplayRow, SearchFocus, SearchScopes, SearchState};

impl App {
    pub fn begin_search(&mut self) {
        let restore_cursor = self.cursor;
        match &mut self.search {
            Some(search) => {
                search.restore_cursor = restore_cursor;
                search.focus = SearchFocus::Query;
            }
            None => {
                let mut s = SearchState::new(restore_cursor);
                s.scopes = self.search_scopes;
                self.search = Some(s);
            }
        }
        self.recompute_search_matches();
        let old_mode = std::mem::replace(&mut self.mode, AppMode::SearchInput);
        self.pre_overlay_mode = match old_mode {
            AppMode::TargetSelect { .. } | AppMode::CommitSelect { .. } => Some(old_mode),
            _ => None,
        };
    }

    fn restore_mode(&mut self) {
        self.mode = self.pre_overlay_mode.take().unwrap_or(AppMode::Normal);
    }

    fn save_search_scopes(&mut self) {
        if let Some(search) = &self.search {
            self.search_scopes = search.scopes;
        }
    }

    pub fn cancel_search(&mut self) {
        self.save_search_scopes();
        if let Some(search) = &self.search {
            self.cursor = search.restore_cursor;
        }
        self.search = None;
        self.restore_mode();
    }

    /// Clear the active search without restoring the cursor.
    ///
    /// Used from plain normal mode, where the current cursor position is the
    /// user's intentional location after navigating matches.
    pub fn clear_search(&mut self) {
        self.save_search_scopes();
        self.search = None;
        self.restore_mode();
    }

    pub fn confirm_search(&mut self) {
        self.save_search_scopes();
        let clear = self
            .search
            .as_ref()
            .is_some_and(|search| search.query().is_empty());
        if clear {
            self.search = None;
        }
        self.restore_mode();
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

    pub fn is_match(&self, row_idx: usize) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.contains(&row_idx))
    }

    pub fn toggle_search_scope(&mut self, flag: SearchScopes) {
        if let Some(search) = &mut self.search {
            search.scopes.toggle(flag);
            self.recompute_search_matches();
        }
    }

    pub fn reset_search_scopes(&mut self) {
        if let Some(search) = &mut self.search {
            search.scopes = SearchScopes::DEFAULT;
            self.recompute_search_matches();
        }
    }

    pub fn enable_all_search_scopes(&mut self) {
        if let Some(search) = &mut self.search {
            search.scopes = SearchScopes::all();
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
        let preferred = self
            .search
            .as_ref()
            .map(|s| s.restore_cursor.min(self.rows.len().saturating_sub(1)))
            .unwrap_or(0);
        self.recompute_search_matches_at(preferred, true);
    }

    /// Recompute search match indices without moving the cursor.
    /// Used after DAG refresh to keep highlights valid.
    pub(crate) fn refresh_search_matches(&mut self) {
        self.recompute_search_matches_at(self.cursor, false);
    }

    fn recompute_search_matches_at(&mut self, preferred: usize, move_cursor: bool) {
        let Some(search) = &self.search else {
            return;
        };
        let query = search.query().to_string();
        let scopes = search.scopes;

        let mut matches = Vec::new();
        if !query.is_empty() && !scopes.is_empty() {
            for row_idx in 0..self.rows.len() {
                if self.row_matches(row_idx, &query, scopes) {
                    matches.push(row_idx);
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
            if move_cursor {
                if let Some(idx) = current_match {
                    self.cursor = search.matches[idx];
                }
            }
        }
    }

    fn row_matches(&self, row_idx: usize, query: &str, scopes: SearchScopes) -> bool {
        let case_sensitive = query.chars().any(|c| c.is_ascii_uppercase());
        let contains = |haystack: &str| {
            if case_sensitive {
                haystack.contains(query)
            } else {
                haystack.to_lowercase().contains(&query.to_lowercase())
            }
        };

        match &self.rows[row_idx] {
            DisplayRow::CommitNode { entry_idx } => {
                let commit = &self.entries[*entry_idx].commit;
                (scopes.contains(SearchScopes::CHANGE_ID)
                    && (contains(commit.change_id.display.as_str())
                        || commit.change_id_suffix.is_some_and(|n| {
                            contains(&format!("{}/{n}", commit.change_id.display))
                        })))
                    || (scopes.contains(SearchScopes::COMMIT_ID)
                        && contains(commit.commit_id.display.as_str()))
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
            }
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => {
                scopes.contains(SearchScopes::PATH)
                    && self
                        .files_for_entry(*entry_idx)
                        .and_then(|files| files.get(file_idx.raw()))
                        .is_some_and(|file| contains(file.path.as_str()))
            }
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => {
                (scopes.contains(SearchScopes::LINE)
                    && self
                        .diff_lines(*entry_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|line| contains(line.content.as_str())))
                    || (scopes.contains(SearchScopes::PATH)
                        && self
                            .files_for_entry(*entry_idx)
                            .and_then(|files| files.get(file_idx.raw()))
                            .is_some_and(|file| contains(file.path.as_str())))
            }
            DisplayRow::GraphLink { .. } => false,
        }
    }
}
