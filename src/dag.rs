use jiff::Timestamp;

/// A short display ID with a unique prefix highlighted.
///
/// For example, if the full hex is `xvzwolmwrq...` and the shortest unique
/// prefix is 4 chars, we store `display = "xvzwolmw"` (8 chars) and
/// `prefix_len = 4`.  The UI renders the prefix bright and the rest dimmed.
pub struct ShortId {
    /// Fixed-length display string (e.g. first 8 chars of hex).
    pub display: String,
    /// Number of characters in `display` that form the unique prefix.
    pub prefix_len: usize,
}

/// Commit metadata extracted from jj-lib, with no jj-lib types leaking out.
pub struct CommitInfo {
    /// Full commit ID hex, used as stable key for graph rendering.
    pub graph_id: String,
    /// Short change ID (reverse hex) with unique prefix length.
    pub change_id: ShortId,
    /// Short commit ID (hex) with unique prefix length.
    pub commit_id: ShortId,
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

#[derive(Debug, Clone, Copy)]
pub enum Glyph {
    WorkingCopy,
    Conflict,
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
            '○' => Ok(Self::Normal),
            _ => Err(()),
        }
    }
}

impl CommitInfo {
    pub fn glyph(&self) -> Glyph {
        if self.is_working_copy {
            Glyph::WorkingCopy
        } else if self.has_conflict {
            Glyph::Conflict
        } else {
            Glyph::Normal
        }
    }
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
