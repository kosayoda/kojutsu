use strum::EnumDiscriminants;

use super::id::{CommitId, CommitRef, FileRef};
use crate::idx::DiffLineIdx;
use crate::keymap::{CommandFlags, SelectionKindSet};
use crate::pluralize;

/// A persistent visual selection range within one file's diff.
#[derive(Clone)]
pub struct VisualRange {
    pub commit_id: CommitId,
    pub path: super::id::RepoPath,
    pub start_line: DiffLineIdx,
    pub end_line: DiffLineIdx,
}

/// A selected item in the DAG, tied to its commit by commit ID. A reload
/// carries it to the commit's rewrite, if there is one.
#[derive(Clone, PartialEq, Eq, Hash, EnumDiscriminants)]
#[strum_discriminants(name(SelectionKind))]
#[strum_discriminants(derive(strum::Display, strum::EnumString, strum::EnumIter))]
#[strum_discriminants(strum(serialize_all = "snake_case"))]
pub enum Selection {
    /// Whole commit selected.
    Commit(CommitRef),
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

    /// The same text without the trailing verb, for embedding in a phrase
    /// ("… does not support 2 files + 5 lines").
    pub fn describe(&self) -> Option<String> {
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

    /// The kind a command should be built from. File and Line coexist, so
    /// this is a precedence, not a description: a line selection is the more
    /// specific of the two, and the line-level encoding carries whole files
    /// as well. Use [`Self::kinds`] to ask what is actually selected.
    pub fn kind(&self) -> SelectionKind {
        if self.summary.line_count > 0 {
            SelectionKind::Line
        } else if self.summary.full_file_count > 0 {
            SelectionKind::File
        } else {
            SelectionKind::Commit
        }
    }

    /// Every kind present. Gating reads this rather than [`Self::kind`] so an
    /// action has to support all of what's selected, not just the part that
    /// happened to win the precedence.
    pub fn kinds(&self) -> SelectionKindSet {
        let mut kinds = SelectionKindSet::empty();
        if self.summary.commit_count > 0 {
            kinds |= SelectionKindSet::COMMIT;
        }
        if self.summary.full_file_count > 0 {
            kinds |= SelectionKindSet::FILE;
        }
        if self.summary.line_count > 0 {
            kinds |= SelectionKindSet::LINE;
        }
        kinds
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

    pub fn describe(&self) -> Option<String> {
        self.summary.describe()
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

    /// Drop whatever can't coexist with `kind`. Commit selections are whole
    /// revisions and File/Line selections are parts of one, so those two
    /// exclude each other; File and Line mix freely, with each file holding
    /// one or the other (enforced where the toggles are).
    pub fn ensure_compatible(&mut self, kind: SelectionKind) {
        let commit_selected = self.summary.commit_count > 0;
        let clash = match kind {
            SelectionKind::Commit => self.is_active() && !commit_selected,
            SelectionKind::File | SelectionKind::Line => commit_selected,
        };
        if clash {
            self.clear();
        }
    }

    pub fn insert(&mut self, selection: Selection) {
        self.ensure_compatible(SelectionKind::from(&selection));
        if self.explicit.insert(selection) {
            self.recompute_summary();
        }
    }

    /// Insert many at once. The summary is recomputed over the whole set on
    /// every change, so inserting a large batch one at a time is quadratic:
    /// expanding a big file into its lines does exactly that.
    pub fn extend(&mut self, selections: impl IntoIterator<Item = Selection>) {
        let mut changed = false;
        for selection in selections {
            self.ensure_compatible(SelectionKind::from(&selection));
            changed |= self.explicit.insert(selection);
        }
        if changed {
            self.recompute_summary();
        }
    }

    pub fn toggle(&mut self, selection: Selection) {
        if !self.remove(&selection) {
            self.insert(selection);
        }
    }

    fn recompute_summary(&mut self) {
        use std::collections::HashSet;
        let mut files = HashSet::new();
        let mut full_file_count = 0usize;
        let mut line_count = 0usize;
        let mut commit_count = 0usize;
        let mut has_full_files = false;

        for selection in &self.explicit {
            match selection {
                Selection::Commit(_) => commit_count += 1,
                Selection::File(file_ref) => {
                    files.insert(&file_ref.path);
                    has_full_files = true;
                    full_file_count += 1;
                }
                Selection::Line { file_ref, .. } => {
                    files.insert(&file_ref.path);
                    line_count += 1;
                }
            }
        }

        self.summary = SelectionSummary {
            commit_count,
            file_count: files.len(),
            full_file_count,
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

    /// The commit this selects in, or selects.
    pub fn commit_id(&self) -> &CommitId {
        match self {
            Selection::Commit(commit) => &commit.commit_id,
            Selection::File(file_ref) | Selection::Line { file_ref, .. } => &file_ref.commit_id,
        }
    }

    /// The same selection in another commit.
    pub fn moved_to(&self, commit_id: &CommitId) -> Self {
        let mut moved = self.clone();
        match &mut moved {
            Selection::Commit(commit) => commit.commit_id = commit_id.clone(),
            Selection::File(file_ref) | Selection::Line { file_ref, .. } => {
                file_ref.commit_id = commit_id.clone()
            }
        }
        moved
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

/// All global toggles. Single source of truth for status bar rendering,
/// help display, and CLI arg generation.
pub const GLOBAL_TOGGLES: &[GlobalToggle] = &[
    GlobalToggle {
        flag: CommandFlags::IGNORE_IMMUTABLE,
        hint: "I",
        label: "ignore-immutable",
        cli_flag: "--ignore-immutable",
    },
    GlobalToggle {
        flag: CommandFlags::IGNORE_WORKING_COPY,
        hint: "W",
        label: "ignore-working-copy",
        cli_flag: "--ignore-working-copy",
    },
    GlobalToggle {
        flag: CommandFlags::DEBUG,
        hint: "D",
        label: "debug",
        cli_flag: "--debug",
    },
];

/// Where to jump the cursor after the next DAG refresh.
pub enum JumpTarget {
    /// Jump to the working copy commit (@).
    WorkingCopy,
    /// Jump to the commit that has this local bookmark.
    Bookmark(super::BookmarkName),
    /// Jump to a commit by a change or commit ID prefix, with jj's
    /// `/offset` when it names one copy of a divergent change or a hidden
    /// commit.
    Revision(super::RevisionArg),
}

#[cfg(test)]
mod selection_context_tests {
    use super::*;
    use crate::types::id::{ChangeId, CommitId, CommitRef, RepoPath};

    fn file_ref(path: &str) -> FileRef {
        FileRef {
            commit_id: CommitId::new("7bbaa2cb"),
            path: RepoPath::new(path),
        }
    }

    fn line(path: &str, n: u32) -> Selection {
        Selection::Line {
            file_ref: file_ref(path),
            old_line: None,
            new_line: Some(n),
        }
    }

    #[test]
    fn files_and_lines_coexist() {
        let mut ctx = SelectionContext::new();
        ctx.insert(Selection::File(file_ref("a.rs")));
        ctx.insert(line("b.rs", 3));

        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx.summary().full_file_count, 1);
        assert_eq!(ctx.summary().line_count, 1);
        assert_eq!(ctx.describe().as_deref(), Some("1 file + 1 line"));
    }

    /// Commit selections are whole revisions; anything finer-grained clears
    /// them and vice versa.
    #[test]
    fn commits_stay_exclusive_in_both_directions() {
        let mut ctx = SelectionContext::new();
        ctx.insert(Selection::Commit(CommitRef {
            commit_id: CommitId::new("7bbaa2cb"),
            change_id: ChangeId::new("qpvuntsm"),
        }));
        ctx.insert(Selection::File(file_ref("a.rs")));
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx.kind(), SelectionKind::File);

        ctx.insert(Selection::Commit(CommitRef {
            commit_id: CommitId::new("7bbaa2cb"),
            change_id: ChangeId::new("qpvuntsm"),
        }));
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx.kind(), SelectionKind::Commit);
    }

    /// Precedence, not description: the line encoding carries whole files, so
    /// a mixed selection has to build its command the line way.
    #[test]
    fn kind_prefers_line_but_kinds_reports_both() {
        let mut ctx = SelectionContext::new();
        ctx.insert(Selection::File(file_ref("a.rs")));
        assert_eq!(ctx.kind(), SelectionKind::File);
        assert_eq!(ctx.kinds(), SelectionKindSet::FILE);

        ctx.insert(line("b.rs", 3));
        assert_eq!(ctx.kind(), SelectionKind::Line);
        assert_eq!(ctx.kinds(), SelectionKindSet::FILE | SelectionKindSet::LINE);
    }

    #[test]
    fn an_empty_selection_has_no_kinds() {
        let ctx = SelectionContext::new();
        assert_eq!(ctx.kinds(), SelectionKindSet::empty());
        assert!(!ctx.is_active());
    }
}
