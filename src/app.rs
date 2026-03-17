use std::collections::HashMap;

use crate::dag::{DagEntry, DiffLine, FileChange};
use crate::graph::{self, GraphLines};
use crate::keymap::KeymapNode;
use crate::repo::JjRepo;

/// The current interaction mode.
pub enum AppMode {
    /// Normal browsing.
    Normal,
    /// A prefix key was pressed; showing submenu options in the bottom bar.
    /// References point into the leaked `&'static Keymap`.
    Submenu {
        label: &'static str,
        children: &'static [(keymap_parser::Node, KeymapNode)],
    },
    /// Showing the result of a shell command. Dismissed on next keypress.
    CommandOutput {
        /// The command that was run, e.g. `"$ jj abandon xvzwolmw"`.
        command: String,
        /// Raw stdout+stderr bytes (may contain ANSI color codes).
        output: Vec<u8>,
        /// Whether the command succeeded.
        success: bool,
    },
}

/// Identifies a display row for cursor restore after rebuild.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKey {
    CommitNode(usize),
    GraphLink(usize, usize),
    FileChange(usize, usize),
    DiffLine(usize, usize, usize),
}

/// One visual row in the list.
pub enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: usize },
    /// A graph link/pad line between commits.
    GraphLink { entry_idx: usize, line_idx: usize },
    /// A file change line (shown when commit is unfolded).
    FileChange { entry_idx: usize, file_idx: usize },
    /// A diff hunk line (shown when a file is unfolded).
    DiffLine {
        entry_idx: usize,
        file_idx: usize,
        line_idx: usize,
    },
}

impl DisplayRow {
    pub fn key(&self) -> RowKey {
        match *self {
            DisplayRow::CommitNode { entry_idx } => RowKey::CommitNode(entry_idx),
            DisplayRow::GraphLink {
                entry_idx,
                line_idx,
            } => RowKey::GraphLink(entry_idx, line_idx),
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => RowKey::FileChange(entry_idx, file_idx),
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => RowKey::DiffLine(entry_idx, file_idx, line_idx),
        }
    }
}

/// Application state. Pure data -- no I/O, no rendering.
pub struct App {
    pub entries: Vec<DagEntry>,
    pub graph: Vec<GraphLines>,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: usize,
    /// Scroll offset of the list from the last render (set by ui::draw).
    pub last_scroll_offset: usize,
    pub revset: String,
    pub repo_root: String,
    /// Current interaction mode.
    pub mode: AppMode,
    /// Per-commit fold state: true = unfolded (showing files).
    pub unfolded: Vec<bool>,
    /// Per-file fold state: (entry_idx, file_idx) -> unfolded.
    pub file_unfolded: HashMap<(usize, usize), bool>,
    /// Lazily loaded file changes, keyed by entry index.
    pub file_cache: HashMap<usize, Vec<FileChange>>,
    /// Lazily loaded diff lines, keyed by (entry_idx, file_idx).
    pub diff_cache: HashMap<(usize, usize), Vec<DiffLine>>,
}

impl App {
    pub fn new(entries: Vec<DagEntry>, revset: String, repo_root: String) -> Self {
        let graph = graph::render(&entries);
        let unfolded = vec![false; entries.len()];

        let mut app = Self {
            entries,
            graph,
            rows: Vec::new(),
            cursor: 0,
            last_scroll_offset: 0,
            revset,
            repo_root,
            mode: AppMode::Normal,
            unfolded,
            file_unfolded: HashMap::new(),
            file_cache: HashMap::new(),
            diff_cache: HashMap::new(),
        };
        app.rebuild_rows();
        app
    }

    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        // Remember what the cursor was pointing at so we can restore it.
        let prev_cursor = self.rows.get(self.cursor).map(DisplayRow::key);

        self.rows.clear();
        for (entry_idx, gl) in self.graph.iter().enumerate() {
            self.rows.push(DisplayRow::CommitNode { entry_idx });

            if self.unfolded[entry_idx] {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    for file_idx in 0..files.len() {
                        self.rows.push(DisplayRow::FileChange {
                            entry_idx,
                            file_idx,
                        });

                        // If this file is unfolded, show diff lines.
                        if self
                            .file_unfolded
                            .get(&(entry_idx, file_idx))
                            .copied()
                            .unwrap_or(false)
                        {
                            if let Some(diff_lines) = self.diff_cache.get(&(entry_idx, file_idx)) {
                                for line_idx in 0..diff_lines.len() {
                                    self.rows.push(DisplayRow::DiffLine {
                                        entry_idx,
                                        file_idx,
                                        line_idx,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Graph lines 0 and 1 are consumed by the 2-line CommitNode
            // ListItem. Remaining lines are rendered as separate GraphLink rows.
            for line_idx in 2..gl.lines.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                });
            }
        }

        // Restore cursor to the exact same row, or fall back to the commit.
        self.cursor = prev_cursor
            .and_then(|key| self.rows.iter().position(|r| r.key() == key))
            .unwrap_or(0);
    }

    /// Get the change ID (unique prefix) of the commit the cursor is on.
    ///
    /// Works from any row type -- files and diff lines resolve to their
    /// parent commit.
    pub fn selected_change_id(&self) -> Option<&str> {
        let entry_idx = match self.rows.get(self.cursor)? {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. }
            | DisplayRow::DiffLine { entry_idx, .. } => *entry_idx,
        };
        let id = &self.entries[entry_idx].commit.change_id;
        Some(&id.display[..id.prefix_len.min(id.display.len())])
    }

    /// Get the scroll offset from the last render.
    pub fn scroll_offset(&self) -> usize {
        self.last_scroll_offset
    }

    /// Move selection to the previous selectable row (commit, file, or diff line).
    pub fn move_up(&mut self) {
        for j in (0..self.cursor).rev() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next selectable row (commit, file, or diff line).
    pub fn move_down(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the previous commit node (section jump).
    pub fn move_up_section(&mut self) {
        for j in (0..self.cursor).rev() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next commit node (section jump).
    pub fn move_down_section(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Toggle fold on the currently selected row.
    ///
    /// - On a commit row: toggle showing file changes.
    /// - On a file row: toggle showing diff hunks.
    pub fn toggle_fold(&mut self, jj: &JjRepo) {
        match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => {
                self.toggle_commit_fold(*entry_idx, jj);
            }
            Some(DisplayRow::FileChange {
                entry_idx,
                file_idx,
            }) => {
                self.toggle_file_fold(*entry_idx, *file_idx, jj);
            }
            _ => {}
        }
    }

    /// Jump to the working copy commit (`@`).
    pub fn jump_to_working_copy(&mut self) {
        if let Some(pos) = self.rows.iter().position(|r| {
            matches!(r, DisplayRow::CommitNode { entry_idx }
                if self.entries[*entry_idx].commit.is_working_copy)
        }) {
            self.cursor = pos;
        }
    }

    /// Move cursor up by `n` selectable rows (commits or files).
    pub fn page_up(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_up();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Move cursor down by `n` selectable rows (commits or files).
    pub fn page_down(&mut self, n: usize) {
        for _ in 0..n {
            let prev = self.cursor;
            self.move_down();
            if self.cursor == prev {
                break;
            }
        }
    }

    /// Select a specific row index (e.g. from mouse click), snapping to the
    /// nearest non-graph-link row at or after `row`.
    pub fn select_row(&mut self, row: usize) {
        let target = row.min(self.rows.len().saturating_sub(1));
        for j in target..self.rows.len() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
        for j in (0..target).rev() {
            if !matches!(self.rows[j], DisplayRow::GraphLink { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Reload DAG data from the repo.
    pub fn refresh(&mut self, jj: &JjRepo, revset: &str) {
        if let Ok(entries) = jj.evaluate_revset(revset) {
            self.graph = graph::render(&entries);
            self.unfolded = vec![false; entries.len()];
            self.file_unfolded.clear();
            self.file_cache.clear();
            self.diff_cache.clear();
            self.entries = entries;
            self.cursor = 0;
            self.rebuild_rows();
        }
    }

    fn toggle_commit_fold(&mut self, entry_idx: usize, jj: &JjRepo) {
        if self.unfolded[entry_idx] {
            self.unfolded[entry_idx] = false;
        } else {
            if !self.file_cache.contains_key(&entry_idx) {
                let graph_id = &self.entries[entry_idx].commit.graph_id;
                let files = jj.file_changes(graph_id).unwrap_or_default();
                self.file_cache.insert(entry_idx, files);
            }
            self.unfolded[entry_idx] = true;
        }
        self.rebuild_rows();
    }

    fn toggle_file_fold(&mut self, entry_idx: usize, file_idx: usize, jj: &JjRepo) {
        let key = (entry_idx, file_idx);
        let currently_unfolded = self.file_unfolded.get(&key).copied().unwrap_or(false);

        if currently_unfolded {
            self.file_unfolded.insert(key, false);
        } else {
            // Lazy load diff lines.
            if !self.diff_cache.contains_key(&key) {
                if let Some(files) = self.file_cache.get(&entry_idx) {
                    if let Some(file) = files.get(file_idx) {
                        let graph_id = &self.entries[entry_idx].commit.graph_id;
                        let diff_lines = jj.file_diff(graph_id, &file.path).unwrap_or_default();
                        self.diff_cache.insert(key, diff_lines);
                    }
                }
            }
            self.file_unfolded.insert(key, true);
        }
        self.rebuild_rows();
    }
}
