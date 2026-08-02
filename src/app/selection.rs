use std::collections::HashSet;

use super::App;
use crate::dag::DiffLineKind;
use crate::idx::{DiffLineIdx, EntryIdx, FileIdx};
use crate::types::{
    ChangeId, FileRef, FileSelectionState, RepoPath, RevisionArg, Selection, SelectionKind,
    SmallVec,
};

impl App {
    /// Resolve entry + file indices to (change_id, file_path).
    /// Returns `None` if the file list isn't loaded yet.
    fn resolve_file(&self, entry_idx: EntryIdx, file_idx: FileIdx) -> Option<(ChangeId, RepoPath)> {
        let change_id = self.nodes[entry_idx].commit.unique_change_id();
        let files = self.files_for_entry(entry_idx)?;
        Some((change_id, files[file_idx.raw()].path.clone()))
    }

    pub fn selection_active(&self) -> bool {
        self.selection.is_active()
    }

    pub fn selection_kind(&self) -> SelectionKind {
        self.selection.kind()
    }

    /// Every selection kind currently present — what action gating tests
    /// against, so a mixed selection needs an action supporting all of it.
    pub fn selection_kinds(&self) -> crate::keymap::SelectionKindSet {
        self.selection.kinds()
    }

    pub fn selection_summary(&self) -> &crate::types::SelectionSummary {
        self.selection.summary()
    }

    pub fn explicit_selection(&self) -> Option<&HashSet<Selection>> {
        self.selection.explicit()
    }

    /// Toggle file selection. If the file belongs to a different commit than
    /// existing selections, clears the old selections first (selections are
    /// scoped to one commit at a time).
    pub fn toggle_file_selection(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let Some((change_id, path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::File);

        // Clear any line-level selections for this file (File overrides Lines).
        self.selection
            .retain(|s| !matches!(s, Selection::Line { file_ref: f, .. } if f.path == path));

        let sel = Selection::File(FileRef { change_id, path });
        self.selection.toggle(sel);
    }

    /// Toggle commit selection. Behavior depends on fold state:
    /// - Folded: toggle commit-level selection (multi-commit).
    /// - Unfolded: toggle all files in commit (select all / deselect all).
    pub fn toggle_commit_selection(&mut self, entry_idx: EntryIdx) {
        if self.is_commit_unfolded(entry_idx) {
            self.toggle_commit_file_selection(entry_idx);
        } else {
            let change_id = self.nodes[entry_idx].commit.unique_change_id();
            self.selection.ensure_compatible(SelectionKind::Commit);
            self.selection.toggle(Selection::Commit(change_id));
        }
    }

    /// Check if a commit is in the explicit commit selection set.
    pub fn is_commit_selected(&self, entry_idx: EntryIdx) -> bool {
        let change_id = self.nodes[entry_idx].commit.unique_change_id();
        self.selection.contains(&Selection::Commit(change_id))
    }

    /// Get the change IDs of explicitly selected commits, or fall back to cursor.
    ///
    /// Selections are keyed by whole change IDs so they survive prefix-length
    /// updates, but what goes to `jj` is the shortest unique prefix — the same
    /// form the cursor path uses, and the same form shown on screen.
    pub fn selected_change_ids(&self) -> SmallVec<RevisionArg> {
        if self.selection_kind() == SelectionKind::Commit && self.selection_active() {
            self.selection
                .iter()
                .filter_map(|s| match s {
                    Selection::Commit(id) => Some(self.shorten_change_id(id)),
                    _ => None,
                })
                .collect()
        } else {
            self.selected_change_id().into_iter().collect()
        }
    }

    /// Narrow a stored change ID to the prefix `jj` needs. Falls back to the
    /// stored ID if its commit has since left the DAG — still resolvable, just
    /// longer than necessary.
    fn shorten_change_id(&self, change_id: &ChangeId) -> RevisionArg {
        self.nodes
            .iter()
            .find(|n| n.commit.unique_change_id() == *change_id)
            .map_or_else(
                || RevisionArg::new(change_id.as_str()),
                |n| n.commit.unique_prefix(),
            )
    }

    /// Toggle all files in an unfolded commit (select all / deselect all).
    fn toggle_commit_file_selection(&mut self, entry_idx: EntryIdx) {
        if self.files_for_entry(entry_idx).is_none() {
            return;
        }

        let change_id = self.nodes[entry_idx].commit.unique_change_id();
        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::File);

        // Collect file paths upfront to avoid borrowing loaded file state across mutations.
        let file_paths: Vec<RepoPath> = self
            .files_for_entry(entry_idx)
            .into_iter()
            .flatten()
            .map(|f| f.path.clone())
            .collect();

        // If all files are already selected (File-level), deselect all.
        let all_selected = file_paths.iter().all(|p| {
            self.selection.contains(&Selection::File(FileRef {
                change_id: change_id.clone(),
                path: p.clone(),
            }))
        });

        if all_selected {
            self.selection.clear();
        } else {
            self.selection.clear();
            for p in file_paths {
                self.selection.insert(Selection::File(FileRef {
                    change_id: change_id.clone(),
                    path: p,
                }));
            }
        }
    }

    /// Replace a whole-file selection with one entry per selectable line,
    /// leaving the same set of changes selected. A `Full` file already draws
    /// its lines as selected ([`Self::is_line_selected`]), so a line toggle
    /// inside one has to deselect that line — not discard the rest of the
    /// file — and it needs the lines to exist individually to do that.
    fn expand_file_selection_to_lines(&mut self, entry_idx: EntryIdx, file_idx: FileIdx) {
        let Some((change_id, path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };
        let file_ref = FileRef {
            change_id: change_id.clone(),
            path,
        };
        if !self.selection.contains(&Selection::File(file_ref.clone())) {
            return;
        }

        let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) else {
            return;
        };
        let lines: Vec<Selection> = diff_lines
            .iter()
            .filter(|dl| dl.is_selectable())
            .map(|dl| Selection::Line {
                file_ref: file_ref.clone(),
                old_line: dl.old_line,
                new_line: dl.new_line,
            })
            .collect();

        self.selection.remove(&Selection::File(file_ref));
        self.selection.extend(lines);
    }

    /// Toggle a single diff line selection (added/removed only).
    pub fn toggle_line_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        // Extract what we need from the diff line before mutating self.
        let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) else {
            return;
        };
        let dl = &diff_lines[line_idx.raw()];
        if !dl.is_selectable() {
            return;
        }
        let old_line = dl.old_line;
        let new_line = dl.new_line;

        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::Line);
        self.expand_file_selection_to_lines(entry_idx, file_idx);

        let sel = Selection::Line {
            file_ref: FileRef {
                change_id,
                path: file_path,
            },
            old_line,
            new_line,
        };
        self.selection.toggle(sel);
    }

    /// Toggle all added/removed lines in a hunk (triggered by space on a header line).
    pub fn toggle_hunk_selection(
        &mut self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        header_line_idx: DiffLineIdx,
    ) {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return;
        };

        // Collect hunk line data before mutating self.
        let mut hunk_lines = Vec::new();
        {
            let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) else {
                return;
            };
            for dl in diff_lines.iter().skip(header_line_idx.raw() + 1) {
                if dl.kind == DiffLineKind::Header {
                    break;
                }
                if dl.is_selectable() {
                    hunk_lines.push(Selection::Line {
                        file_ref: FileRef {
                            change_id: change_id.clone(),
                            path: file_path.clone(),
                        },
                        old_line: dl.old_line,
                        new_line: dl.new_line,
                    });
                }
            }
        }

        self.clear_other_commits(&change_id);
        self.selection.ensure_compatible(SelectionKind::Line);
        self.expand_file_selection_to_lines(entry_idx, file_idx);

        // If all hunk lines are already selected, deselect them. Otherwise select all.
        let all_selected = hunk_lines.iter().all(|s| self.selection.contains(s));
        if all_selected {
            for s in &hunk_lines {
                self.selection.remove(s);
            }
        } else {
            self.selection.extend(hunk_lines);
        }
    }

    /// Check if a specific diff line is selected.
    pub fn is_line_selected(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    ) -> bool {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return false;
        };
        let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) else {
            return false;
        };
        let diff_line = &diff_lines[line_idx.raw()];

        // If the whole file is selected, all lines are implicitly selected.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return diff_line.is_selectable();
        }

        self.selection.contains(&Selection::Line {
            file_ref: FileRef {
                change_id,
                path: file_path,
            },
            old_line: diff_line.old_line,
            new_line: diff_line.new_line,
        })
    }

    /// Get the selection state of a file for UI display.
    pub fn file_selection_state(
        &self,
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    ) -> FileSelectionState {
        let Some((change_id, file_path)) = self.resolve_file(entry_idx, file_idx) else {
            return FileSelectionState::None;
        };

        // Explicit file-level selection.
        if self.selection.contains(&Selection::File(FileRef {
            change_id: change_id.clone(),
            path: file_path.clone(),
        })) {
            return FileSelectionState::Full;
        }

        // Check if any lines for this file are selected by scanning the diff
        // and testing membership, rather than scanning all selections.
        let Some(diff_lines) = self.diff_lines(entry_idx, file_idx) else {
            return FileSelectionState::None;
        };
        let file_ref = FileRef {
            change_id,
            path: file_path,
        };
        let mut selected_count = 0usize;
        let mut selectable_count = 0usize;
        for dl in diff_lines {
            if !dl.is_selectable() {
                continue;
            }
            selectable_count += 1;
            if self.selection.contains(&Selection::Line {
                file_ref: file_ref.clone(),
                old_line: dl.old_line,
                new_line: dl.new_line,
            }) {
                selected_count += 1;
            }
        }

        if selected_count == 0 {
            FileSelectionState::None
        } else if selectable_count > 0 && selected_count >= selectable_count {
            FileSelectionState::Full
        } else {
            FileSelectionState::Partial
        }
    }

    /// Check if there are any line-level selections (vs only file-level).
    pub fn has_line_selections(&self) -> bool {
        self.selection.any(|s| matches!(s, Selection::Line { .. }))
    }

    /// Get the unique file paths from all selections.
    pub fn selected_file_paths(&self) -> Vec<crate::types::Str> {
        let mut paths: Vec<crate::types::Str> = self
            .selection
            .iter()
            .filter_map(|s| s.path().map(crate::types::Str::from))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    /// Number of currently selected items.
    pub fn selection_count(&self) -> usize {
        self.selection.len()
    }

    /// Clear all selections.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Clear selections from other commits if switching to a different one.
    pub(crate) fn clear_other_commits(&mut self, change_id: &ChangeId) {
        if self.selection.is_active() {
            let same_commit = self.selection.any(|s| s.change_id() == change_id);
            if !same_commit {
                self.clear_selection();
            }
        }
    }
}

#[cfg(test)]
mod change_id_key_tests {
    use super::super::{App, DagNode};
    use crate::dag::CommitInfo;
    use crate::graph::GraphLines;
    use crate::idx::EntryIdx;
    use crate::types::SmallVec;

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";
    const COMMIT_ID: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";

    fn app_with_one_commit() -> App {
        let mut app = App::for_test();
        app.nodes.push(DagNode::new(
            CommitInfo::for_test(CHANGE_ID, COMMIT_ID),
            GraphLines::default(),
            SmallVec::new(),
        ));
        app
    }

    #[test]
    fn a_selection_survives_the_background_prefix_update() {
        // Prefix lengths land asynchronously. If the selection key were derived
        // from the displayed prefix, a commit selected beforehand would silently
        // stop matching itself once a >8-char prefix arrived.
        let mut app = app_with_one_commit();
        let idx = EntryIdx::new(0);

        app.toggle_commit_selection(idx);
        assert!(app.is_commit_selected(idx));

        app.nodes[idx].commit.change_id.set_prefix_len(12);
        assert!(app.is_commit_selected(idx));
    }

    #[test]
    fn a_selected_commit_reaches_jj_as_its_short_prefix() {
        // Selections are keyed by the whole change ID; commands must still get
        // the same short prefix the cursor path and the UI use.
        let mut app = app_with_one_commit();
        let idx = EntryIdx::new(0);
        app.nodes[idx].commit.change_id.set_prefix_len(2);

        app.toggle_commit_selection(idx);

        let ids = app.selected_change_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].as_str(), "uu");
    }
}
