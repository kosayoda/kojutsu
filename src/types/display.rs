use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, CommandLogDetailIdx, CommandLogIdx, ConflictHunkIdx,
    ConflictLineIdx, ConflictSideIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx,
    FileIdx, GraphLineIdx, OpLogDetailIdx, OpLogIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
};

/// One visual row in the list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DisplayRow {
    /// A commit node line (graph glyph + commit info).
    CommitNode { entry_idx: EntryIdx },
    /// A continuation line of a multi-line commit description (shown when unfolded).
    DescriptionLine {
        entry_idx: EntryIdx,
        line_idx: DescriptionLineIdx,
    },
    /// A graph link/pad line between commits.
    GraphLink {
        entry_idx: EntryIdx,
        line_idx: GraphLineIdx,
    },
    /// A file change line (shown when commit is unfolded).
    FileChange {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
    },
    /// A diff hunk line (shown when a file is unfolded).
    DiffLine {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    },
    /// A bookmark row in the bookmark view.
    BookmarkItem { bookmark_idx: BookmarkIdx },
    /// A conflict target line under a bookmark.
    BookmarkConflictTarget {
        bookmark_idx: BookmarkIdx,
        target_idx: BookmarkDetailIdx,
    },
    /// A remote tracking line under a bookmark.
    BookmarkRemoteTarget {
        bookmark_idx: BookmarkIdx,
        target_idx: BookmarkDetailIdx,
    },
    /// A tag row in the tag view.
    TagItem { tag_idx: TagIdx },
    /// A remote tracking line under a tag.
    TagRemoteTarget {
        tag_idx: TagIdx,
        target_idx: TagDetailIdx,
    },
    /// An operation row in the operation log view.
    OpLogItem { op_log_idx: OpLogIdx },
    /// A detail line under an unfolded op log entry.
    OpLogDetailLine {
        op_log_idx: OpLogIdx,
        line_idx: OpLogDetailIdx,
    },
    /// A graph link/pad line between operations.
    OpLogGraphLink {
        op_log_idx: OpLogIdx,
        line_idx: GraphLineIdx,
    },
    /// "Load more..." sentinel at the bottom of the op log.
    OpLogLoadMore,
    /// An evolution log entry.
    EvoLogItem { evolog_idx: EvoLogIdx },
    /// A file change row within an unfolded evolog entry.
    EvoLogFileChange {
        evolog_idx: EvoLogIdx,
        file_idx: FileIdx,
    },
    /// A diff line within an unfolded evolog file.
    EvoLogFileDiffLine {
        evolog_idx: EvoLogIdx,
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    },
    /// A graph link/pad line between evolog entries.
    EvoLogGraphLink {
        evolog_idx: EvoLogIdx,
        line_idx: GraphLineIdx,
    },
    /// A workspace row in the workspace view.
    WorkspaceItem { workspace_idx: WorkspaceIdx },
    /// A command log entry row.
    CommandLogItem { log_idx: CommandLogIdx },
    /// A detail line under an unfolded command log entry.
    CommandLogDetail {
        log_idx: CommandLogIdx,
        line_idx: CommandLogDetailIdx,
    },
    /// Header for a conflict hunk (e.g. "── conflict 1 of 2 ──").
    ConflictHeader {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: ConflictHunkIdx,
    },
    /// A line from a conflict side (ours/theirs/base).
    ConflictSide {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: ConflictHunkIdx,
        side_idx: ConflictSideIdx,
        line_idx: ConflictLineIdx,
    },
    /// A resolved (context) line within a conflict view.
    ConflictContext {
        entry_idx: EntryIdx,
        file_idx: FileIdx,
        hunk_idx: ConflictHunkIdx,
        line_idx: ConflictLineIdx,
    },
    /// Header row in the interdiff view.
    InterdiffHeader,
    /// A file change row in the interdiff view.
    InterdiffFileChange { file_idx: FileIdx },
    /// A diff line within an unfolded interdiff file.
    InterdiffDiffLine {
        file_idx: FileIdx,
        line_idx: DiffLineIdx,
    },
}
