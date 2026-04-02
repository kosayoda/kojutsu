use compact_str::format_compact;
use jiff::Timestamp;

use crate::types::{ChangeId, CommitId, Str};

/// A short display ID with a unique prefix highlighted.
///
/// Shows at least 8 chars, extended if needed for uniqueness. For example,
/// if the shortest unique prefix is 4 chars: `display = "xvzwolmw"` (8 chars),
/// `prefix_len = 4`. If the prefix is 10 chars: `display = "xvzwolmwrq"` (10 chars),
/// `prefix_len = 10`. The UI renders the prefix bright and the rest dimmed.
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
    pub tags: Vec<Str>,
}

#[derive(Clone, Copy, Default)]
pub struct LineStats {
    pub added: u32,
    pub removed: u32,
}

/// A local bookmark with its tracking status.
pub struct BookmarkInfo {
    /// Bookmark name.
    pub name: String,
    /// Whether the local bookmark differs from its tracked remote counterpart.
    pub is_dirty: bool,
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
    pub name: String,
    pub remote: String,
}

/// A remote bookmark (e.g., `main@origin`).
#[derive(Clone)]
pub struct RemoteBookmarkInfo {
    /// Bookmark name (e.g., "main").
    pub name: String,
    /// Remote name (e.g., "origin").
    pub remote: String,
    /// Whether the remote target matches the local target.
    pub synced: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Glyph {
    WorkingCopy,
    Conflict,
    Immutable,
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
        } else {
            Glyph::Normal
        }
    }

    /// A ChangeId that's unique even among divergent commits (includes suffix).
    /// Uses the full display string as the base.
    pub fn unique_change_id(&self) -> ChangeId {
        match self.change_id_suffix {
            Some(suffix) => {
                ChangeId::new(format_compact!("{}/{suffix}", self.change_id.display))
            }
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

/// An edge from a commit to a parent in the DAG.
pub struct Edge {
    /// The change ID of the target commit.
    pub target: ChangeId,
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
pub struct FileChange {
    pub path: String,
    pub status: FileStatus,
    pub has_conflict: bool,
}

pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

/// A single line of a unified diff.
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
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
