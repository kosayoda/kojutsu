use strum::EnumDiscriminants;

use super::id::{ChangeId, FileRef};
use crate::idx::DiffLineIdx;
use crate::keymap::{CommandFlags, SelectionKindSet};
use crate::pluralize;

/// A persistent visual selection range within one file's diff.
#[derive(Clone)]
pub struct VisualRange {
    pub change_id: ChangeId,
    pub path: super::id::RepoPath,
    pub start_line: DiffLineIdx,
    pub end_line: DiffLineIdx,
}

/// A selected item in the DAG. Tied to commit identity (change ID) and file
/// path, so selections survive DAG refreshes.
#[derive(Clone, PartialEq, Eq, Hash, EnumDiscriminants)]
#[strum_discriminants(name(SelectionKind))]
pub enum Selection {
    /// Commit selected (used implicitly from cursor, not currently in explicit sets).
    Commit(ChangeId),
    /// Entire file selected.
    File(FileRef),
    /// Individual diff line selected (added or removed).
    Line {
        file_ref: FileRef,
        /// Line number in the old file (`Some` for removed/context lines).
        old_line: Option<u32>,
        /// Line number in the new file (`Some` for added/context lines).
        new_line: Option<u32>,
    },
}

pub struct SelectionSummary {
    pub commit_count: usize,
    pub file_count: usize,
    pub full_file_count: usize,
    pub line_count: usize,
    pub has_full_files: bool,
}

impl SelectionSummary {
    pub fn empty() -> Self {
        Self {
            commit_count: 0,
            file_count: 0,
            full_file_count: 0,
            line_count: 0,
            has_full_files: false,
        }
    }

    pub fn display_text(&self) -> Option<String> {
        self.format_summary(" selected")
    }

    pub fn submenu_suffix(&self) -> Option<String> {
        self.format_summary("")
    }

    fn format_summary(&self, suffix: &str) -> Option<String> {
        if self.commit_count > 0 {
            let noun = pluralize!(self.commit_count, "commit", "commits");
            return Some(format!("{} {}{suffix}", self.commit_count, noun));
        }

        let file_noun = pluralize!(self.full_file_count, "file", "files");
        let line_noun = pluralize!(self.line_count, "line", "lines");

        if self.file_count == 0 && self.line_count == 0 {
            return None;
        }

        if self.line_count == 0 {
            return Some(format!("{} {}{suffix}", self.full_file_count, file_noun));
        }

        if !self.has_full_files && self.file_count == 1 {
            return Some(format!("{} {}{suffix}", self.line_count, line_noun));
        }

        if self.has_full_files && self.line_count > 0 {
            return Some(format!(
                "{} {} + {} {}{suffix}",
                self.full_file_count, file_noun, self.line_count, line_noun
            ));
        }

        Some(format!(
            "{} {} in {} {}{suffix}",
            self.line_count, line_noun, self.file_count, file_noun
        ))
    }
}

impl SelectionKind {
    pub fn as_bitset(&self) -> SelectionKindSet {
        match self {
            SelectionKind::Commit => SelectionKindSet::COMMIT,
            SelectionKind::File => SelectionKindSet::FILE,
            SelectionKind::Line => SelectionKindSet::LINE,
        }
    }
}

pub struct SelectionContext {
    explicit: std::collections::HashSet<Selection>,
    summary: SelectionSummary,
}

impl SelectionContext {
    pub fn new() -> Self {
        Self {
            explicit: std::collections::HashSet::new(),
            summary: SelectionSummary::empty(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.explicit.is_empty()
    }

    pub fn kind(&self) -> SelectionKind {
        self.explicit
            .iter()
            .next()
            .map(SelectionKind::from)
            .unwrap_or(SelectionKind::Commit)
    }

    pub fn summary(&self) -> &SelectionSummary {
        &self.summary
    }

    pub fn explicit(&self) -> Option<&std::collections::HashSet<Selection>> {
        if self.explicit.is_empty() {
            None
        } else {
            Some(&self.explicit)
        }
    }

    pub fn display_text(&self) -> Option<String> {
        self.summary.display_text()
    }

    pub fn submenu_suffix(&self) -> Option<String> {
        self.summary.submenu_suffix()
    }

    pub fn clear(&mut self) {
        self.explicit.clear();
        self.summary = SelectionSummary::empty();
    }

    pub fn len(&self) -> usize {
        self.explicit.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = &Selection> + '_> {
        Box::new(self.explicit.iter())
    }

    pub fn contains(&self, selection: &Selection) -> bool {
        self.explicit.contains(selection)
    }

    pub fn remove(&mut self, selection: &Selection) -> bool {
        let removed = self.explicit.remove(selection);
        if removed {
            self.recompute_summary();
        }
        removed
    }

    pub fn retain(&mut self, f: impl FnMut(&Selection) -> bool) {
        self.explicit.retain(f);
        self.recompute_summary();
    }

    pub fn any(&self, f: impl FnMut(&Selection) -> bool) -> bool {
        self.iter().any(f)
    }

    pub fn ensure_kind(&mut self, kind: SelectionKind) {
        let reset = self.is_active() && self.kind() != kind;
        if reset {
            self.clear();
        }
    }

    pub fn insert(&mut self, kind: SelectionKind, selection: Selection) {
        self.ensure_kind(kind);
        if self.explicit.insert(selection) {
            self.recompute_summary();
        }
    }

    pub fn toggle(&mut self, kind: SelectionKind, selection: Selection) {
        if !self.remove(&selection) {
            self.insert(kind, selection);
        }
    }

    fn recompute_summary(&mut self) {
        let mut files = std::collections::HashSet::new();
        let mut full_files = std::collections::HashSet::new();
        let mut line_count = 0usize;
        let mut commit_count = 0usize;
        let mut has_full_files = false;

        for selection in &self.explicit {
            match selection {
                Selection::Commit(_) => commit_count += 1,
                Selection::File(file_ref) => {
                    files.insert(file_ref.path.clone());
                    has_full_files = true;
                    full_files.insert(file_ref.path.clone());
                }
                Selection::Line { file_ref, .. } => {
                    files.insert(file_ref.path.clone());
                    line_count += 1;
                }
            }
        }

        self.summary = SelectionSummary {
            commit_count,
            file_count: files.len(),
            full_file_count: full_files.len(),
            line_count,
            has_full_files,
        };
    }
}

impl Default for SelectionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Selection {
    /// Get the file reference from any selection variant.
    /// Returns `None` for `Selection::Commit`.
    pub fn file_ref(&self) -> Option<&FileRef> {
        match self {
            Selection::Commit(_) => None,
            Selection::File(file_ref) => Some(file_ref),
            Selection::Line {
                file_ref,
                old_line: _,
                new_line: _,
            } => Some(file_ref),
        }
    }

    /// Get the change ID from any selection variant.
    pub fn change_id(&self) -> &ChangeId {
        match self {
            Selection::Commit(change_id) => change_id,
            Selection::File(file_ref) | Selection::Line { file_ref, .. } => &file_ref.change_id,
        }
    }

    /// Get the file path from any selection variant.
    /// Returns `None` for `Selection::Commit`.
    pub fn path(&self) -> Option<&str> {
        self.file_ref().map(|f| f.path.as_str())
    }
}

/// Selection state of a file (for UI display).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileSelectionState {
    /// No selections for this file.
    None,
    /// Some lines selected (but not all, and no File-level selection).
    Partial,
    /// Entire file selected (Selection::File entry exists).
    Full,
}

/// Metadata for a global toggle that persists across commands.
pub struct GlobalToggle {
    /// The `CommandFlags` bit this toggle controls.
    pub flag: CommandFlags,
    /// Short hint character shown in the status bar (e.g., "I").
    pub hint: &'static str,
    /// Human-readable label (e.g., "ignore-immutable").
    pub label: &'static str,
    /// CLI flag appended to jj commands (e.g., "--ignore-immutable").
    pub cli_flag: &'static str,
}
