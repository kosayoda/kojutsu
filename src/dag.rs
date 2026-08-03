use std::collections::HashMap;

use compact_str::format_compact;
use jiff::Timestamp;

use crate::types::{
    BookmarkName, ChangeId, CommitId, RemoteName, RepoPath, RevisionArg, Str, TagName,
    WorkspaceName,
};

/// Number of characters of an ID always shown, even when fewer would be unique.
pub const DISPLAY_ID_LEN: usize = 8;

/// A whole change or commit ID, plus the length of its shortest unique prefix.
///
/// The ID is kept intact and every view of it is derived. [`Self::display`]
/// shows the prefix padded to at least [`DISPLAY_ID_LEN`], which the UI renders
/// bright up to `prefix_len` and dim beyond; [`Self::prefix`] is the shortest
/// form `jj` can still resolve; [`Self::full`] is the whole thing.
///
/// `prefix_len` starts at `DISPLAY_ID_LEN` as a placeholder — computing the
/// real value means evaluating a revset, so it lands later from a background
/// pass. Holding the full ID keeps that update to a single integer, and keeps
/// anything keyed by an ID keyed to the same string before and after it.
#[derive(Clone, Debug)]
pub struct ShortId {
    full: Str,
    prefix_len: usize,
}

impl ShortId {
    /// A whole ID whose shortest unique prefix is not yet known.
    pub fn new(full: impl Into<Str>) -> Self {
        Self {
            full: full.into(),
            prefix_len: DISPLAY_ID_LEN,
        }
    }

    /// The whole ID. Use this as a key — it never changes.
    pub fn full(&self) -> &str {
        &self.full
    }

    /// The shortest prefix that still resolves to this commit, which is what
    /// `jj` should be handed as a revision argument.
    pub fn prefix(&self) -> &str {
        &self.full[..self.prefix_len.min(self.full.len())]
    }

    /// What the UI shows: the unique prefix, padded out to [`DISPLAY_ID_LEN`]
    /// so IDs line up in a column.
    pub fn display(&self) -> &str {
        let len = self.prefix_len.max(DISPLAY_ID_LEN).min(self.full.len());
        &self.full[..len]
    }

    /// [`Self::display`] split into its unique prefix and the padding after
    /// it, for rendering the two halves differently.
    pub fn split(&self) -> (&str, &str) {
        self.display()
            .split_at(self.prefix_len.min(self.full.len()))
    }

    pub fn set_prefix_len(&mut self, prefix_len: usize) {
        self.prefix_len = prefix_len;
    }

    pub fn change_id(&self) -> ChangeId {
        ChangeId::new(self.full())
    }
}

/// Commit metadata extracted from jj-lib, with no jj-lib types leaking out.
#[derive(Debug)]
pub struct CommitInfo {
    /// Full commit ID hex, used as stable key for graph rendering.
    pub graph_id: CommitId,
    /// Short change ID (reverse hex) with unique prefix length.
    pub change_id: ShortId,
    /// Short commit ID (hex) with unique prefix length.
    pub commit_id: ShortId,
    /// First line of description, or `None` if empty / "(no description set)".
    pub description: Option<String>,
    /// Full description text (only set when multi-line).
    pub full_description: Option<String>,
    /// Author information.
    pub author: AuthorInfo,
    /// Workspaces that have this commit as their working copy.
    pub workspaces: Vec<WorkspaceAnnotation>,
    /// Whether this commit is empty (no diff from parent).
    pub is_empty: bool,
    /// Whether this commit has multiple parents (merge commit).
    pub is_merge: bool,
    /// Whether this commit has unresolved conflicts.
    pub has_conflict: bool,
    /// Whether this commit is immutable (ancestor of immutable_heads).
    pub is_immutable: bool,
    /// Divergence/hidden state. `None` for normal commits.
    pub divergence: Option<DivergenceInfo>,
    /// Local bookmarks pointing at this commit.
    pub bookmarks: Vec<BookmarkInfo>,
    /// Remote bookmarks pointing at this commit (excluding those already
    /// represented by a local bookmark with the same name).
    pub remote_bookmarks: Vec<RemoteBookmarkInfo>,
    /// Tags pointing at this commit.
    pub tags: Vec<TagName>,
}

#[cfg(test)]
impl CommitInfo {
    /// A commit carrying only the identity fields, for tests that care about
    /// how IDs are keyed and shortened.
    pub fn for_test(change_id: &str, commit_id: &str) -> Self {
        Self {
            graph_id: CommitId::new(commit_id),
            change_id: ShortId::new(change_id),
            commit_id: ShortId::new(commit_id),
            description: None,
            full_description: None,
            author: AuthorInfo {
                name: String::new(),
                email: String::new(),
                timestamp: Timestamp::UNIX_EPOCH,
                tz_offset_seconds: 0,
            },
            workspaces: Vec::new(),
            is_empty: false,
            is_merge: false,
            has_conflict: false,
            is_immutable: false,
            divergence: None,
            bookmarks: Vec::new(),
            remote_bookmarks: Vec::new(),
            tags: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct LineStats {
    pub added: u32,
    pub removed: u32,
}

/// Result of computing file-level changes for a commit.
pub struct CommitDetails {
    pub files: Vec<FileChange>,
    pub stats: LineStats,
    pub is_empty: bool,
}

/// Shortest unique prefix lengths for a commit's change and commit IDs,
/// computed in the background and applied to IDs that already hold their
/// full text.
#[derive(Clone, Copy)]
pub struct PrefixLengthUpdate {
    pub change_prefix_len: usize,
    pub commit_prefix_len: usize,
}

impl PrefixLengthUpdate {
    /// Apply this update to a `CommitSummary`.
    pub fn apply(&self, summary: &mut CommitSummary) {
        summary.change_id.set_prefix_len(self.change_prefix_len);
        summary
            .short_commit_id
            .set_prefix_len(self.commit_prefix_len);
    }
}

/// Divergence and hidden status for a commit. Present only when the commit
/// is divergent (multiple visible commits share the same change ID) or hidden
/// (superseded by a newer version).
#[derive(Clone, Debug)]
pub struct DivergenceInfo {
    pub is_divergent: bool,
    pub is_hidden: bool,
    pub suffix: Option<usize>,
}

/// The revision to hand `jj` for a commit: the shortest unique change ID
/// prefix, plus the `/<offset>` jj needs when the change is divergent or the
/// commit is hidden and a bare change ID would resolve elsewhere.
///
/// Shared so that every view produces the same reference for a commit — a
/// bookmark row and a DAG row naming the same commit must run the same thing.
fn revision_of(change_id: &ShortId, divergence: Option<&DivergenceInfo>) -> RevisionArg {
    let prefix = change_id.prefix();
    match divergence.and_then(|d| d.suffix) {
        Some(suffix) => RevisionArg::new(format_compact!("{prefix}/{suffix}")),
        None => RevisionArg::new(prefix),
    }
}

/// A local bookmark with its tracking status.
#[derive(Debug)]
pub struct BookmarkInfo {
    /// Bookmark name.
    pub name: BookmarkName,
    /// Whether the local bookmark differs from its tracked remote counterpart.
    pub is_dirty: bool,
    /// Whether the local bookmark tracks a remote (e.g., `main` tracks `main@origin`).
    pub is_tracking: bool,
    /// Whether the bookmark has conflicting targets (divergent operations).
    pub is_conflicted: bool,
}

/// A workspace that has a commit as its working copy.
#[derive(Clone, Debug)]
pub struct WorkspaceAnnotation {
    /// Workspace name (e.g., "default", "feature").
    pub name: WorkspaceName,
    /// Whether this is the workspace kojutsu is running in.
    pub is_current: bool,
}

/// A bookmark name + remote pair (e.g., for track/untrack operations).
#[derive(Debug, Clone)]
pub struct BookmarkRef {
    pub name: BookmarkName,
    pub remote: RemoteName,
}

/// A remote bookmark (e.g., `main@origin`).
#[derive(Clone, Debug)]
pub struct RemoteBookmarkInfo {
    /// Bookmark name (e.g., "main").
    pub name: BookmarkName,
    /// Remote name (e.g., "origin").
    pub remote: RemoteName,
    /// Whether the remote target matches the local target.
    pub synced: bool,
    /// Whether the remote ref is tracked locally.
    pub is_tracked: bool,
}

/// A remote bookmark reference with full metadata (for off-DAG bookmarks).
#[derive(Clone)]
pub struct RemoteBookmarkRef {
    pub name: BookmarkName,
    pub remote: RemoteName,
    pub commit_id: Option<crate::types::CommitId>,
    pub is_tracked: bool,
}

/// Core commit fields shared across bookmark conflict targets, remote targets,
/// and tag targets.
pub struct CommitSummary {
    /// Full commit ID hex.
    pub commit_id: CommitId,
    /// Short change ID with unique prefix length.
    pub change_id: ShortId,
    /// Short commit ID with unique prefix length.
    pub short_commit_id: ShortId,
    /// First line of description.
    pub description: Option<String>,
    /// Divergence/hidden state. `None` for normal commits.
    pub divergence: Option<DivergenceInfo>,
}

impl CommitSummary {
    /// The revision to hand `jj` for this commit.
    pub fn revision(&self) -> RevisionArg {
        revision_of(&self.change_id, self.divergence.as_ref())
    }

    /// Whether this commit has been superseded.
    pub fn is_hidden(&self) -> bool {
        self.divergence.as_ref().is_some_and(|d| d.is_hidden)
    }

    /// Divergence suffix, for rendering the `/<n>` alongside the ID.
    pub fn change_id_suffix(&self) -> Option<usize> {
        self.divergence.as_ref().and_then(|d| d.suffix)
    }
}

/// Whether something was added or removed (used for op diffs, conflict targets, etc.).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Removed,
}

/// A single conflict target for a conflicted bookmark.
pub struct BookmarkConflictTarget {
    pub kind: DiffKind,
    /// Commit metadata.
    pub summary: CommitSummary,
}

/// Remote tracking info for a bookmark at a specific remote.
pub struct BookmarkRemoteTarget {
    /// Remote name (e.g., "origin", "git").
    pub remote: RemoteName,
    /// Commit metadata.
    pub summary: CommitSummary,
    /// Whether the remote ref is tracked locally.
    pub is_tracked: bool,
    /// Commits the local is behind the remote (None = unknown/conflicted).
    pub behind_count: Option<usize>,
    /// Commits the local is ahead of the remote (None = unknown/conflicted).
    pub ahead_count: Option<usize>,
}

/// Expanded detail data for a bookmark (conflict targets + remote tracking).
pub struct BookmarkDetails {
    pub conflict_targets: Vec<BookmarkConflictTarget>,
    pub remote_targets: Vec<BookmarkRemoteTarget>,
}

/// Remote tracking info for a tag at a specific remote.
pub struct TagRemoteTarget {
    /// Remote name (e.g., "origin", "git").
    pub remote: RemoteName,
    /// Commit metadata.
    pub summary: CommitSummary,
}

/// Rich data for a single tag (local target + remote tracking).
pub struct TagDetails {
    /// Whether the local tag has been deleted (remote-only).
    pub is_deleted: bool,
    /// Commit info for the local target (if present).
    pub local_target: Option<TagLocalTarget>,
    /// Remote tracking info.
    pub remote_targets: Vec<TagRemoteTarget>,
}

/// Local target info for a tag.
pub struct TagLocalTarget {
    /// Commit metadata.
    pub summary: CommitSummary,
}

impl CommitInfo {
    pub fn is_working_copy(&self) -> bool {
        self.workspaces.iter().any(|ws| ws.is_current)
    }

    pub fn glyph(&self) -> crate::theme::Glyph {
        use crate::theme::Glyph;
        if self.is_working_copy() {
            Glyph::WorkingCopy
        } else if self.has_conflict {
            Glyph::Conflict
        } else if self.is_immutable {
            Glyph::Immutable
        } else if self.is_merge {
            Glyph::Merge
        } else {
            Glyph::Normal
        }
    }

    pub fn is_divergent(&self) -> bool {
        self.divergence.as_ref().is_some_and(|d| d.is_divergent)
    }

    pub fn is_hidden(&self) -> bool {
        self.divergence.as_ref().is_some_and(|d| d.is_hidden)
    }

    pub fn change_id_suffix(&self) -> Option<usize> {
        self.divergence.as_ref().and_then(|d| d.suffix)
    }

    /// Stable identity for this commit, unique even among divergent ones.
    ///
    /// Built from the whole change ID rather than the displayed prefix, so
    /// that anything keyed by it — selections, fold state, cursor restore —
    /// keeps matching when the background pass revises `prefix_len`.
    pub fn unique_change_id(&self) -> ChangeId {
        match self.change_id_suffix() {
            Some(suffix) => ChangeId::new(format_compact!("{}/{suffix}", self.change_id.full())),
            None => self.change_id.change_id(),
        }
    }

    /// The revision to hand `jj` for this commit: the shortest unique change
    /// ID prefix, plus the `/<offset>` jj needs when the change is divergent
    /// or the commit is hidden and a bare change ID would resolve elsewhere.
    pub fn unique_prefix(&self) -> RevisionArg {
        revision_of(&self.change_id, self.divergence.as_ref())
    }
}

#[derive(Debug)]
pub struct AuthorInfo {
    pub name: String,
    pub email: String,
    pub timestamp: Timestamp,
    /// Timezone offset from UTC in seconds (e.g. -18000 for UTC-5).
    pub tz_offset_seconds: i32,
}

/// A single entry in the DAG: a commit plus its edges to parents.
pub struct DagEntry {
    pub commit: CommitInfo,
    pub edges: Vec<Edge>,
}

/// Result of evaluating a revset: entries plus any non-fatal warnings.
pub struct RevsetResult {
    pub entries: Vec<DagEntry>,
    pub warnings: Vec<String>,
}

/// An edge from a commit to a parent in the DAG.
pub struct Edge {
    /// The commit ID hex of the target (parent) commit.
    pub target: CommitId,
    pub kind: EdgeKind,
}

#[derive(Debug)]
pub enum EdgeKind {
    /// Immediate parent.
    Direct,
    /// Ancestor, but not a direct parent (transitive edge).
    Indirect,
    /// Parent is outside the revset or missing.
    Missing,
}

/// A file changed in a commit.
#[derive(Clone)]
pub struct FileChange {
    pub path: RepoPath,
    /// Source path for renames/copies (the old location).
    pub old_path: Option<RepoPath>,
    pub status: FileStatus,
    pub has_conflict: bool,
    /// The diff baseline (e.g. the auto-merged parents of a merge commit)
    /// was conflicted, so the diff is shown against materialized conflict
    /// markers rather than plain file content.
    pub baseline_conflicted: bool,
    /// Either side of the change is a Git submodule pointer rather than
    /// file content. See [`Self::line_selection_blocker`].
    pub is_submodule: bool,
    /// Per-file line stats (added/removed counts).
    pub stats: LineStats,
}

impl FileChange {
    /// A resolved conflict: the baseline was conflicted and this commit
    /// resolves it (jj diff labels these "Resolved conflict in …").
    pub fn is_conflict_resolution(&self) -> bool {
        self.baseline_conflicted && !self.has_conflict
    }

    /// What stops a line-level selection from applying to this path, if
    /// anything — worded to drop into a sentence after the path.
    ///
    /// Line selection works by re-invoking kojutsu as jj's diff editor and
    /// rewriting the files jj laid out in its "after" directory. A submodule
    /// is not laid out as a file there — jj creates an empty directory and
    /// drops submodule entries again when it snapshots the result — so the
    /// pointer is carried through whole no matter what the editor writes.
    /// Nothing can narrow it, which makes any per-line answer a lie.
    pub fn line_selection_blocker(&self) -> Option<&'static str> {
        self.is_submodule.then_some("a Git submodule")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    /// Diff materialization failed for this file.
    Error,
}

/// A token within a diff line (for word-level highlighting).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffTokenKind {
    /// Unchanged text.
    Unchanged,
    /// Removed text (shown in error/red color).
    Removed,
    /// Added text (shown in added/green color).
    Added,
}

#[derive(Clone)]
pub struct DiffToken {
    pub text: String,
    pub kind: DiffTokenKind,
}

/// A single line of a unified diff.
#[derive(Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    /// Token spans for word-level highlighting within the line.
    pub tokens: Vec<DiffToken>,
    /// Line number in the old (removed) file. `None` for added lines and headers.
    pub old_line: Option<u32>,
    /// Line number in the new (added) file. `None` for removed lines and headers.
    pub new_line: Option<u32>,
    /// The line lies in a materialized conflict region (markers plus the
    /// term content between them) on either side of the diff.
    pub conflict_region: bool,
}

impl DiffLine {
    /// Whether this line can be individually selected. Added/removed
    /// lines only, and never inside a conflict region — moving partial
    /// conflict encodings between commits produces malformed conflicts.
    pub fn is_selectable(&self) -> bool {
        self.kind.is_selectable() && !self.conflict_region
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Unchanged context line.
    Context,
    /// Added line.
    Added,
    /// Removed line.
    Removed,
    /// Hunk header (e.g. `@@ -1,5 +1,7 @@`).
    Header,
}

/// Both diff formats for a single file, computed at load time.
#[derive(Clone)]
pub struct DiffResult {
    /// Traditional unified diff with `+`/`-` lines.
    pub git: Vec<DiffLine>,
    /// Color-words format with inline removed/added tokens.
    pub color_words: Vec<DiffLine>,
}

impl DiffResult {
    pub fn lines(&self, format: crate::app::DiffFormat) -> &Vec<DiffLine> {
        match format {
            crate::app::DiffFormat::Git => &self.git,
            crate::app::DiffFormat::ColorWords => &self.color_words,
        }
    }
}

/// Per-commit metadata for the annotate detail expansion.
#[derive(Clone)]
pub struct AnnotateCommitInfo {
    pub commit_id: ShortId,
    pub change_id: ShortId,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    /// Pre-split description lines (trimmed). Empty vec → "(no description set)".
    pub description_lines: Vec<String>,
}

impl AnnotateCommitInfo {
    /// Number of detail rows this commit info expands to.
    pub fn detail_row_count(&self) -> usize {
        4 + self.description_lines.len().max(1)
    }
}

/// Result from file annotation: lines + per-commit metadata.
#[derive(Clone)]
pub struct AnnotateResult {
    pub lines: Vec<AnnotateLineData>,
    pub commit_info: HashMap<CommitId, AnnotateCommitInfo>,
}

/// A syntax-highlighted token within a line.
#[derive(Clone)]
pub struct SyntaxToken {
    pub text: String,
    /// ANSI color index (0-15 for terminal palette colors).
    pub color_idx: u8,
}

/// A single line from file annotation (blame).
#[derive(Clone)]
pub struct AnnotateLineData {
    /// Full hex commit ID (for jump-to-commit).
    pub commit_id: CommitId,
    /// Short change ID for display (prefix-highlighted).
    pub change_id: ShortId,
    /// Author name.
    pub author: String,
    /// Relative time string (e.g. "3 days ago").
    pub relative_time: crate::types::Str,
    /// 1-based line number in the current file.
    pub line_number: usize,
    /// The line content (plain text, used for search).
    pub content: String,
    /// Syntax-highlighted tokens for rendering. Empty if highlighting unavailable.
    pub syntax_tokens: Vec<SyntaxToken>,
    /// Whether this line's origin was outside the annotation domain.
    pub outside_domain: bool,
}

impl DiffLineKind {
    /// Whether this line kind can be individually selected (added or
    /// removed). Callers outside this module want `DiffLine::is_selectable`,
    /// which also excludes conflict-region lines.
    fn is_selectable(self) -> bool {
        matches!(self, Self::Added | Self::Removed)
    }
}

#[cfg(test)]
mod short_id_tests {
    use super::*;

    /// 32-char reverse-hex change ID, the real shape.
    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";

    #[test]
    fn a_short_prefix_is_still_displayed_at_the_default_width() {
        let mut id = ShortId::new(CHANGE_ID);
        id.set_prefix_len(2);

        assert_eq!(id.prefix(), "uu");
        assert_eq!(id.display(), "uunnomkx");
        assert_eq!(id.split(), ("uu", "nnomkx"));
    }

    #[test]
    fn a_long_prefix_widens_the_display_to_fit() {
        let mut id = ShortId::new(CHANGE_ID);
        id.set_prefix_len(10);

        assert_eq!(id.prefix(), "uunnomkxrq");
        assert_eq!(id.display(), "uunnomkxrq");
        assert_eq!(id.split(), ("uunnomkxrq", ""));
    }

    #[test]
    fn the_full_id_never_changes_as_the_prefix_length_does() {
        // Selections, fold state and cursor restore are keyed by the full ID,
        // so the background prefix pass must not move that key.
        let mut id = ShortId::new(CHANGE_ID);
        let before = id.full().to_string();
        for len in [1, 8, 12, 40] {
            id.set_prefix_len(len);
            assert_eq!(id.full(), before);
        }
    }

    #[test]
    fn an_over_long_prefix_length_does_not_panic() {
        // jj returns `len + 1` when one ID is an exact prefix of another.
        let mut id = ShortId::new("abcd");
        id.set_prefix_len(5);

        assert_eq!(id.prefix(), "abcd");
        assert_eq!(id.display(), "abcd");
        assert_eq!(id.split(), ("abcd", ""));
    }

    #[test]
    fn an_id_shorter_than_the_display_width_is_shown_whole() {
        let id = ShortId::new("abc");

        assert_eq!(id.display(), "abc");
        assert_eq!(id.split(), ("abc", ""));
    }

    #[test]
    fn an_empty_id_renders_as_nothing() {
        // Op-log rows can reference an entry with no commit behind it.
        let id = ShortId::new("");

        assert_eq!(id.prefix(), "");
        assert_eq!(id.display(), "");
        assert_eq!(id.split(), ("", ""));
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    const CHANGE_ID: &str = "uunnomkxrqvlypszwlwkvvqnstvzoxrs";
    const COMMIT_ID: &str = "7bbaa2cb1f0e4d3a9c8b7a6e5d4c3b2a19087654";

    fn commit(divergence: Option<DivergenceInfo>) -> CommitInfo {
        let mut c = CommitInfo::for_test(CHANGE_ID, COMMIT_ID);
        c.change_id.set_prefix_len(2);
        c.divergence = divergence;
        c
    }

    #[test]
    fn an_ordinary_commit_is_referred_to_by_its_change_prefix() {
        assert_eq!(commit(None).unique_prefix().as_str(), "uu");
    }

    #[test]
    fn a_divergent_or_hidden_commit_carries_its_change_offset() {
        // A bare change ID is ambiguous when divergent, and resolves to the
        // superseding commit when hidden. jj disambiguates both the same way.
        for (is_divergent, is_hidden) in [(true, false), (false, true)] {
            let c = commit(Some(DivergenceInfo {
                is_divergent,
                is_hidden,
                suffix: Some(1),
            }));
            assert_eq!(c.unique_prefix().as_str(), "uu/1");
        }
    }

    #[test]
    fn identity_is_the_whole_change_id_not_the_revision() {
        // The revision shortens as prefixes are computed; the key must not.
        let c = commit(None);
        assert_eq!(c.unique_change_id().as_str(), CHANGE_ID);
        assert_ne!(c.unique_change_id().as_str(), c.unique_prefix().as_str());
    }

    fn summary(divergence: Option<DivergenceInfo>) -> CommitSummary {
        let mut change_id = ShortId::new(CHANGE_ID);
        change_id.set_prefix_len(2);
        CommitSummary {
            commit_id: CommitId::new(COMMIT_ID),
            change_id,
            short_commit_id: ShortId::new(COMMIT_ID),
            description: None,
            divergence,
        }
    }

    #[test]
    fn a_summary_names_a_commit_the_same_way_a_dag_row_does() {
        // A bookmark row and a DAG row pointing at one commit have to run the
        // same thing, so both derive their revision the same way.
        for divergence in [
            None,
            Some(DivergenceInfo {
                is_divergent: true,
                is_hidden: false,
                suffix: Some(1),
            }),
            Some(DivergenceInfo {
                is_divergent: false,
                is_hidden: true,
                suffix: Some(3),
            }),
        ] {
            let expected = commit(divergence.clone()).unique_prefix();
            assert_eq!(summary(divergence).revision(), expected);
        }
    }

    #[test]
    fn a_summary_without_divergence_needs_no_offset() {
        assert_eq!(summary(None).revision().as_str(), "uu");
        assert!(!summary(None).is_hidden());
        assert_eq!(summary(None).change_id_suffix(), None);
    }

    #[test]
    fn a_hidden_summary_reports_itself_hidden_and_carries_the_offset() {
        // A hidden bookmark target's bare change ID resolves to whatever
        // superseded it, so the offset has to survive to the command.
        let s = summary(Some(DivergenceInfo {
            is_divergent: false,
            is_hidden: true,
            suffix: Some(2),
        }));
        assert_eq!(s.revision().as_str(), "uu/2");
        assert!(s.is_hidden());
        assert_eq!(s.change_id_suffix(), Some(2));
    }

    #[test]
    fn a_divergent_identity_keeps_the_offset_too() {
        let c = commit(Some(DivergenceInfo {
            is_divergent: true,
            is_hidden: false,
            suffix: Some(2),
        }));
        assert_eq!(c.unique_change_id().as_str(), format!("{CHANGE_ID}/2"));
    }
}
