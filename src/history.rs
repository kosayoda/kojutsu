//! The repo's history as the log views show it: the operation log and a
//! change's evolution. Plain data read from jj; drawing it is the app's job.

use crate::dag::{DiffKind, ShortId};
use crate::types::{CommitId, OperationId, Str, WorkspaceName};

/// One operation in the operation log.
pub struct OpLogEntry {
    /// Full hex operation ID.
    pub id: OperationId,
    /// The operations this one follows, newest-first like jj lists them.
    pub parent_ids: Vec<OperationId>,
    /// Human-readable operation description.
    pub description: Str,
    /// Relative time string (e.g. "5 hours ago").
    pub relative_time: Str,
    /// Workspace name that ran this operation.
    pub workspace: Option<WorkspaceName>,
    /// "user@host" who performed the operation.
    pub user: Str,
    /// The CLI args that produced this operation (from metadata tags).
    pub args: Option<Str>,
    /// Whether this is a pure working-copy snapshot.
    pub is_snapshot: bool,
    /// Whether this is the repo's current operation.
    pub is_current: bool,
}

/// One version of a change in its evolution log.
pub struct EvoLogEntry {
    /// Full hex commit ID.
    pub commit_id: CommitId,
    /// Short change ID with unique prefix length.
    pub change_id: ShortId,
    /// First line of commit description.
    pub description: Option<String>,
    /// Author name/email.
    pub author: Str,
    /// Relative time string (e.g. "5 hours ago").
    pub relative_time: Str,
    /// Description of the operation that produced this version.
    pub op_description: Option<Str>,
    /// Whether this is the newest (current) version.
    pub is_current: bool,
    /// Predecessor commit IDs (the version(s) this was rewritten from).
    pub predecessor_ids: Vec<CommitId>,
}

#[cfg(test)]
impl EvoLogEntry {
    /// A step carrying only its identity and ancestry.
    pub fn for_test(commit_id: CommitId, predecessor_ids: Vec<CommitId>) -> Self {
        Self {
            commit_id,
            change_id: ShortId::new("zzzzzzzz"),
            description: None,
            author: Str::default(),
            relative_time: Str::default(),
            op_description: None,
            is_current: false,
            predecessor_ids,
        }
    }
}

/// A commit an operation added or removed.
pub struct OpDiffCommit {
    pub change_id: ShortId,
    pub commit_id: ShortId,
    pub description: Option<String>,
    pub kind: DiffKind,
}

/// A workspace whose working-copy commit an operation moved.
pub struct OpDiffWorkingCopy {
    pub workspace: WorkspaceName,
    pub new_commit: Option<ShortId>,
    pub old_commit: Option<ShortId>,
}

/// A bookmark an operation moved.
pub struct OpDiffBookmark {
    pub name: Str,
    pub new_target: Option<ShortId>,
    pub old_target: Option<ShortId>,
}

/// One line of what an operation changed, as `jj op show` groups it.
pub enum OpDetailLine {
    SectionHeader(Str),
    Commit(OpDiffCommit),
    WorkingCopy(OpDiffWorkingCopy),
    Bookmark(OpDiffBookmark),
}
