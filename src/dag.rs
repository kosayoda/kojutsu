use compact_str::format_compact;
use jiff::Timestamp;

use crate::types::{BookmarkName, ChangeId, CommitId, RemoteName, RepoPath, TagName};

/// A short display ID with a unique prefix highlighted.
///
/// Shows at least 8 chars, extended if needed for uniqueness. For example,
/// if the shortest unique prefix is 4 chars: `display = "xvzwolmw"` (8 chars),
/// `prefix_len = 4`. If the prefix is 10 chars: `display = "xvzwolmwrq"` (10 chars),
/// `prefix_len = 10`. The UI renders the prefix bright and the rest dimmed.
#[derive(Clone)]
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
    /// Whether this commit is divergent (multiple visible commits share the same change ID).
    pub is_divergent: bool,
    /// Whether this commit is hidden (superseded by a newer version with the same change ID).
    pub is_hidden: bool,
    /// Disambiguation suffix for the change ID (e.g., `5` in `ztmnmkvk/5`).
    /// `Some(n)` when multiple commits share the same change ID prefix.
    pub change_id_suffix: Option<usize>,
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
    /// Apply this update to a (change_id, short_commit_id) pair.
    pub fn apply(&self, change_id: &mut ShortId, short_commit_id: &mut ShortId) {
        change_id.display.clone_from(&self.change_display);
        change_id.prefix_len = self.change_prefix_len;
        short_commit_id.display.clone_from(&self.commit_display);
        short_commit_id.prefix_len = self.commit_prefix_len;
    }
}

/// Divergence and hidden status for a commit.
pub struct DivergenceUpdate {
    pub is_divergent: bool,
    pub is_hidden: bool,
    pub change_id_suffix: Option<usize>,
}

/// A local bookmark with its tracking status.
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
#[derive(Clone)]
pub struct WorkspaceAnnotation {
    /// Workspace name (e.g., "default", "feature").
    pub name: String,
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
#[derive(Clone)]
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

// ---------------------------------------------------------------------------
// Rich bookmark detail types (for bookmark view child rows)
// ---------------------------------------------------------------------------

/// Whether a conflict target was added or removed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictTargetKind {
    Added,
    Removed,
}

/// A single conflict target for a conflicted bookmark.
pub struct BookmarkConflictTarget {
    pub kind: ConflictTargetKind,
    /// Full commit ID hex.
    pub commit_id: CommitId,
    /// Short change ID with unique prefix length.
    pub change_id: ShortId,
    /// Short commit ID with unique prefix length.
    pub short_commit_id: ShortId,
    /// First line of description.
    pub description: Option<String>,
    /// Whether the commit is hidden (superseded).
    pub is_hidden: bool,
    /// Divergence suffix (e.g., `Some(2)` → `/2`).
    pub change_id_suffix: Option<usize>,
}

/// Remote tracking info for a bookmark at a specific remote.
pub struct BookmarkRemoteTarget {
    /// Remote name (e.g., "origin", "git").
    pub remote: RemoteName,
    /// Full commit ID hex of the remote's target.
    pub commit_id: CommitId,
    /// Short change ID of the remote's target commit.
    pub change_id: ShortId,
    /// Short commit ID of the remote's target commit.
    pub short_commit_id: ShortId,
    /// First line of description.
    pub description: Option<String>,
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
    /// Full commit ID hex (for comparison with local target).
    pub commit_id: CommitId,
    /// Short change ID of the remote's target commit.
    pub change_id: ShortId,
    /// Short commit ID of the remote's target commit.
    pub short_commit_id: ShortId,
    /// First line of description.
    pub description: Option<String>,
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
    pub commit_id: CommitId,
    pub change_id: ShortId,
    pub short_commit_id: ShortId,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum Glyph {
    WorkingCopy,
    Conflict,
    Immutable,
    Merge,
    Normal,
}

impl std::fmt::Display for Glyph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", char::from(*self))
    }
}

impl From<Glyph> for char {
    fn from(value: Glyph) -> Self {
        match value {
            Glyph::WorkingCopy => '@',
            Glyph::Conflict => '×',
            Glyph::Immutable => '◆',
            Glyph::Merge => '⊕',
            Glyph::Normal => '○',
        }
    }
}

impl TryFrom<char> for Glyph {
    type Error = ();

    fn try_from(value: char) -> Result<Self, Self::Error> {
        match value {
            '@' => Ok(Self::WorkingCopy),
            '×' => Ok(Self::Conflict),
            '◆' => Ok(Self::Immutable),
            '⊕' => Ok(Self::Merge),
            '○' => Ok(Self::Normal),
            _ => Err(()),
        }
    }
}

impl CommitInfo {
    pub fn is_working_copy(&self) -> bool {
        self.workspaces.iter().any(|ws| ws.is_current)
    }

    pub fn glyph(&self) -> Glyph {
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

    /// A ChangeId that's unique even among divergent commits (includes suffix).
    /// Uses the full display string as the base.
    pub fn unique_change_id(&self) -> ChangeId {
        match self.change_id_suffix {
            Some(suffix) => ChangeId::new(format_compact!("{}/{suffix}", self.change_id.display)),
            None => self.change_id.change_id(),
        }
    }

    /// Short unique prefix with suffix if divergent. Used for jj CLI arguments.
    pub fn unique_prefix(&self) -> ChangeId {
        let prefix =
            &self.change_id.display[..self.change_id.prefix_len.min(self.change_id.display.len())];
        match self.change_id_suffix {
            Some(suffix) => ChangeId::new(format_compact!("{prefix}/{suffix}")),
            None => ChangeId::new(prefix),
        }
    }
}

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
}

/// A hunk in a conflicted file — either auto-resolved or needing user choice.
#[derive(Clone)]
pub struct ConflictHunk {
    pub kind: ConflictHunkKind,
}

#[derive(Clone)]
pub enum ConflictHunkKind {
    /// Auto-resolved section — just context lines.
    Resolved { lines: Vec<String> },
    /// Conflicted section with multiple sides to choose from.
    Conflict {
        /// Each side's content lines.
        sides: Vec<Vec<String>>,
        /// Which side the user picked (None = unresolved).
        selected: Option<usize>,
    },
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

impl DiffLineKind {
    /// Whether this line kind can be individually selected (added or removed).
    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Added | Self::Removed)
    }
}
