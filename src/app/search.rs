use tui_input::Input;

use super::{App, AppMode};
use crate::idx::RowIdx;
use crate::types::{DisplayRow, SearchFocus, SearchScopes, SearchState};

/// Match the description and change_id fields shared by bookmark, tag, and
/// workspace entries.
fn ref_entry_matches(
    description: Option<&str>,
    change_id: Option<&crate::dag::ShortId>,
    scopes: SearchScopes,
    contains: &dyn Fn(&str) -> bool,
) -> bool {
    (scopes.contains(SearchScopes::DESCRIPTION) && description.is_some_and(contains))
        || (scopes.contains(SearchScopes::CHANGE_ID)
            && change_id.is_some_and(|c| contains(&c.display)))
}

impl App {
    pub fn begin_search(&mut self) {
        let restore_cursor = self.cursor;
        match &mut self.search {
            Some(search) => {
                search.restore_cursor = restore_cursor;
                search.focus = SearchFocus::Query;
            }
            None => {
                self.search = Some(SearchState::new(restore_cursor));
            }
        }
        self.recompute_search_matches();
        self.enter_overlay(AppMode::SearchInput);
    }

    fn restore_mode(&mut self) {
        self.exit_overlay();
    }

    pub fn cancel_search(&mut self) {
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
        self.search = None;
        self.restore_mode();
    }

    pub fn confirm_search(&mut self) {
        self.search = None;
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

    pub fn is_match(&self, row_idx: RowIdx) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.matches.contains(&row_idx))
    }

    pub fn toggle_search_scope(&mut self, flag: SearchScopes) {
        if self.search.is_some() {
            self.search_scopes_mut().toggle(flag);
            self.recompute_search_matches();
        }
    }

    pub fn reset_search_scopes(&mut self) {
        if self.search.is_some() {
            *self.search_scopes_mut() = self.default_search_scopes;
            self.recompute_search_matches();
        }
    }

    pub fn enable_all_search_scopes(&mut self) {
        if self.search.is_some() {
            *self.search_scopes_mut() = SearchScopes::all();
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
            .map(|s| {
                RowIdx::new(
                    s.restore_cursor
                        .raw()
                        .min(self.rows.len().saturating_sub(1)),
                )
            })
            .unwrap_or(RowIdx::new(0));
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
        let query = search.query().to_string();
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
            if move_cursor {
                if let Some(idx) = current_match {
                    self.cursor = search.matches[idx];
                }
            }
        }
    }

    fn row_matches(&self, row_idx: usize, query: &str, scopes: SearchScopes) -> bool {
        let case_sensitive = query.chars().any(|c| c.is_ascii_uppercase());
        let query_lower;
        let needle = if case_sensitive {
            query
        } else {
            query_lower = query.to_lowercase();
            &query_lower
        };
        let contains = |haystack: &str| {
            if case_sensitive {
                haystack.contains(needle)
            } else {
                haystack.to_lowercase().contains(needle)
            }
        };

        match &self.rows[row_idx] {
            DisplayRow::CommitNode { entry_idx } => {
                let commit = &self.nodes[*entry_idx].commit;
                (scopes.contains(SearchScopes::CHANGE_ID)
                    && (contains(commit.change_id.display.as_str())
                        || commit.change_id_suffix().is_some_and(|n| {
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
                    || (scopes.contains(SearchScopes::TAG)
                        && commit.tags.iter().any(|t| contains(t.as_str())))
            }
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => {
                scopes.contains(SearchScopes::PATH_COMMAND)
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
                    || (scopes.contains(SearchScopes::PATH_COMMAND)
                        && self
                            .files_for_entry(*entry_idx)
                            .and_then(|files| files.get(file_idx.raw()))
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
            DisplayRow::BookmarkConflictTarget { .. } | DisplayRow::BookmarkRemoteTarget { .. } => {
                false
            }
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
            | DisplayRow::ConflictSide { .. }
            | DisplayRow::ConflictContext { .. } => false,
            DisplayRow::EvoLogItem { evolog_idx } => {
                let Some(entry) = self.evolog.entries.get(evolog_idx.raw()) else {
                    return false;
                };
                (scopes.contains(SearchScopes::CHANGE_ID) && contains(&entry.change_id.display))
                    || (scopes.contains(SearchScopes::DESCRIPTION)
                        && entry.description.as_deref().is_some_and(contains))
                    || (scopes.contains(SearchScopes::AUTHOR) && contains(entry.author.as_str()))
            }
            DisplayRow::EvoLogFileChange {
                evolog_idx,
                file_idx,
            } => {
                scopes.contains(SearchScopes::PATH_COMMAND)
                    && self
                        .evolog
                        .entries
                        .get(evolog_idx.raw())
                        .and_then(|e| self.evolog.files.get(&e.commit_id))
                        .and_then(|l| l.loaded())
                        .and_then(|files| files.get(file_idx.raw()))
                        .is_some_and(|file| contains(file.path.as_str()))
            }
            DisplayRow::EvoLogFileDiffLine {
                evolog_idx,
                file_idx,
                line_idx,
            } => {
                scopes.contains(SearchScopes::LINE)
                    && self
                        .evolog_diff_lines(*evolog_idx, *file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|line| contains(line.content.as_str()))
            }
            DisplayRow::InterdiffDiffLine { file_idx, line_idx } => {
                scopes.contains(SearchScopes::LINE)
                    && self
                        .interdiff_diff_lines(*file_idx)
                        .and_then(|lines| lines.get(line_idx.raw()))
                        .is_some_and(|line| contains(line.content.as_str()))
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
                        .target
                        .as_ref()
                        .is_some_and(|t| contains(&t.from_label) || contains(&t.to_label))
            }
            DisplayRow::InterdiffFileChange { file_idx } => {
                scopes.contains(SearchScopes::PATH_COMMAND)
                    && self
                        .interdiff
                        .files
                        .loaded()
                        .and_then(|files| files.get(file_idx.raw()))
                        .is_some_and(|file| contains(file.path.as_str()))
            }
            DisplayRow::CommandLogDetail { .. } => false,
            DisplayRow::AnnotateLine { line_idx } => {
                let line = self
                    .annotate
                    .lines
                    .loaded()
                    .and_then(|l| l.get(line_idx.raw()));
                if let Some(line) = line {
                    (scopes.contains(SearchScopes::CHANGE_ID) && contains(&line.change_id.display))
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
                    (scopes.contains(SearchScopes::CHANGE_ID) && contains(&info.change_id.display))
                        || (scopes.contains(SearchScopes::DESCRIPTION)
                            && info.description_lines.iter().any(|l| contains(l)))
                        || (scopes.contains(SearchScopes::AUTHOR)
                            && (contains(&info.author_name) || contains(&info.author_email)))
                })
            }
        }
    }
}
