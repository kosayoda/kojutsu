use jiff::Timestamp;

/// Commit metadata extracted from jj-lib, with no jj-lib types leaking out.
pub struct CommitInfo {
    /// Full commit ID hex, used as stable key for graph rendering.
    pub graph_id: String,
    /// Short unique change ID prefix (reverse hex).
    pub change_id: String,
    /// Short unique commit ID prefix (hex).
    pub commit_id: String,
    /// First line of description, or `None` if empty / "(no description set)".
    pub description: Option<String>,
    /// Author information.
    pub author: AuthorInfo,
    /// Whether this is the working copy commit (`@`).
    pub is_working_copy: bool,
    /// Whether this commit is empty (no diff from parent).
    pub is_empty: bool,
    /// Whether this commit has unresolved conflicts.
    pub has_conflict: bool,
    /// Bookmark names pointing at this commit.
    pub bookmarks: Vec<String>,
}

pub struct AuthorInfo {
    pub name: String,
    pub email: String,
    pub timestamp: Timestamp,
}

/// A single entry in the DAG: a commit plus its edges to parents.
pub struct DagEntry {
    pub commit: CommitInfo,
    pub edges: Vec<Edge>,
}

/// An edge from a commit to a parent in the DAG.
pub struct Edge {
    /// The change ID of the target commit.
    pub target: String,
    pub kind: EdgeKind,
}

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
}

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
