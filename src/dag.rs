use std::collections::HashMap;

use compact_str::format_compact;
use jiff::Timestamp;

use crate::types::{
    BookmarkName, ChangeId, CommitId, RemoteName, RepoPath, TagName, WorkspaceName,
};

/// A short display ID with a unique prefix highlighted.
///
/// Shows at least 8 chars, extended if needed for uniqueness. For example,
/// if the shortest unique prefix is 4 chars: `display = "xvzwolmw"` (8 chars),
/// `prefix_len = 4`. If the prefix is 10 chars: `display = "xvzwolmwrq"` (10 chars),
/// `prefix_len = 10`. The UI renders the prefix bright and the rest dimmed.
#[derive(Clone, Debug)]
pub struct ShortId {
    /// Display string (at least 8 chars, longer if needed for uniqueness).
    pub display: String,
    /// Number of characters in `display` that form the unique prefix.
    pub prefix_len: usize,
}

impl ShortId {
    pub fn change_id(&self) -> ChangeId {
        ChangeId::new(&self.display)
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

/// Shortest unique prefix lengths for a commit's change and commit IDs.
pub struct PrefixLengthUpdate {
    pub change_display: String,
    pub change_prefix_len: usize,
    pub commit_display: String,
    pub commit_prefix_len: usize,
}

impl PrefixLengthUpdate {
    /// Apply this update to a `CommitSummary`.
    pub fn apply(&self, summary: &mut CommitSummary) {
        summary.change_id.display.clone_from(&self.change_display);
        summary.change_id.prefix_len = self.change_prefix_len;
        summary
            .short_commit_id
            .display
            .clone_from(&self.commit_display);
        summary.short_commit_id.prefix_len = self.commit_prefix_len;
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
    /// Whether the commit is hidden (superseded).
    pub is_hidden: bool,
    /// Divergence suffix (e.g., `Some(2)` → `/2`).
    pub change_id_suffix: Option<usize>,
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
    /// Divergence suffix (e.g., `Some(2)` → `/2`).
    pub change_id_suffix: Option<usize>,
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

    /// A ChangeId that's unique even among divergent commits (includes suffix).
    /// Uses the full display string as the base.
    pub fn unique_change_id(&self) -> ChangeId {
        match self.change_id_suffix() {
            Some(suffix) => ChangeId::new(format_compact!("{}/{suffix}", self.change_id.display)),
            None => self.change_id.change_id(),
        }
    }

    /// Short unique prefix with suffix if divergent. Used for jj CLI arguments.
    pub fn unique_prefix(&self) -> ChangeId {
        let prefix =
            &self.change_id.display[..self.change_id.prefix_len.min(self.change_id.display.len())];
        match self.change_id_suffix() {
            Some(suffix) => ChangeId::new(format_compact!("{prefix}/{suffix}")),
            None => ChangeId::new(prefix),
        }
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
    /// Per-file line stats (added/removed counts).
    pub stats: LineStats,
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

/// Identifies one term of a conflict hunk. jj represents a conflict as
/// alternating positive terms ("sides", the contents to merge) and negative
/// terms ("bases", the common ancestors diffed away); ordinals are 0-based.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictTermKind {
    Side(usize),
    Base(usize),
}

impl ConflictTermKind {
    /// Human-readable label. Two-sided conflicts use the familiar
    /// ours/theirs/base; n-way conflicts fall back to numbered terms.
    pub fn label(self, num_sides: usize) -> String {
        if num_sides <= 2 {
            match self {
                Self::Side(0) => "ours".to_string(),
                Self::Side(_) => "theirs".to_string(),
                Self::Base(_) => "base".to_string(),
            }
        } else {
            match self {
                Self::Side(n) => format!("side {}", n + 1),
                Self::Base(n) => format!("base {}", n + 1),
            }
        }
    }
}

/// One term of a conflict hunk: a side's or base's content lines.
#[derive(Clone)]
pub struct ConflictTerm {
    pub kind: ConflictTermKind,
    pub lines: Vec<String>,
}

#[derive(Clone)]
pub enum ConflictHunkKind {
    /// Auto-resolved section — just context lines.
    Resolved { lines: Vec<String> },
    /// Conflicted section with multiple terms to choose from.
    Conflict {
        /// Terms in jj's materialized order: side 1, base 1, side 2, ...
        terms: Vec<ConflictTerm>,
        /// Which term the user picked (None = unresolved).
        selected: Option<ConflictTermKind>,
    },
}

impl ConflictHunkKind {
    /// Number of positive terms (sides) in a conflict hunk; 0 for resolved.
    pub fn num_sides(&self) -> usize {
        match self {
            Self::Resolved { .. } => 0,
            Self::Conflict { terms, .. } => terms.len().div_ceil(2),
        }
    }
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
pub struct AnnotateResult {
    pub lines: Vec<AnnotateLineData>,
    pub commit_info: HashMap<CommitId, AnnotateCommitInfo>,
}

/// A syntax-highlighted token within a line.
pub struct SyntaxToken {
    pub text: String,
    /// ANSI color index (0-15 for terminal palette colors).
    pub color_idx: u8,
}

/// A single line from file annotation (blame).
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
    /// Whether this line kind can be individually selected (added or removed).
    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Added | Self::Removed)
    }
}
