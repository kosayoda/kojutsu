use std::collections::HashMap;

use crate::dag::{DagEntry, FileChange};
use crate::graph::{self, GraphLines};
use crate::repo::JjRepo;

/// One visual row in the list.
pub enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: usize },
    /// A graph link/pad line between commits.
    GraphLink { entry_idx: usize, line_idx: usize },
    /// A file change line (shown when commit is unfolded).
    FileChange { entry_idx: usize, file_idx: usize },
}

/// Application state. Pure data -- no I/O, no rendering.
pub struct App {
    pub entries: Vec<DagEntry>,
    pub graph: Vec<GraphLines>,
    /// Flattened display rows (one per visual line).
    pub rows: Vec<DisplayRow>,
    /// Index into `rows` of the currently selected row.
    pub cursor: usize,
    pub revset: String,
    pub repo_root: String,
    /// Per-commit fold state: true = unfolded (showing files).
    pub unfolded: Vec<bool>,
    /// Lazily loaded file changes, keyed by entry index.
    pub file_cache: HashMap<usize, Vec<FileChange>>,
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
            revset,
            repo_root,
            unfolded,
            file_cache: HashMap::new(),
        };
        app.rebuild_rows();
        app
    }

    /// Rebuild the flattened row list from current fold state.
    pub fn rebuild_rows(&mut self) {
        let selected_entry = self.rows.get(self.cursor).map(|r| match r {
            DisplayRow::CommitNode { entry_idx }
            | DisplayRow::GraphLink { entry_idx, .. }
            | DisplayRow::FileChange { entry_idx, .. } => *entry_idx,
        });

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
                    }
                }
            }

            for line_idx in 1..gl.lines.len() {
                self.rows.push(DisplayRow::GraphLink {
                    entry_idx,
                    line_idx,
                });
            }
        }

        // Restore selection to the same commit if possible.
        self.cursor = selected_entry
            .and_then(|target| {
                self.rows.iter().position(
                    |r| matches!(r, DisplayRow::CommitNode { entry_idx } if *entry_idx == target),
                )
            })
            .unwrap_or(0);
    }

    /// Move selection to the previous commit node line.
    pub fn move_up(&mut self) {
        for j in (0..self.cursor).rev() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Move selection to the next commit node line.
    pub fn move_down(&mut self) {
        for j in (self.cursor + 1)..self.rows.len() {
            if matches!(self.rows[j], DisplayRow::CommitNode { .. }) {
                self.cursor = j;
                return;
            }
        }
    }

    /// Toggle fold on the currently selected commit, lazily loading file
    /// changes from `jj` if needed.
    pub fn toggle_fold(&mut self, jj: &JjRepo) {
        let entry_idx = match self.rows.get(self.cursor) {
            Some(DisplayRow::CommitNode { entry_idx }) => *entry_idx,
            Some(DisplayRow::FileChange { entry_idx, .. }) => *entry_idx,
            _ => return,
        };

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
}
