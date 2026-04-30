use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, ConflictHunkIdx, ConflictLineIdx, ConflictSideIdx,
    DescriptionLineIdx, DiffLineIdx, EntryIdx, EvoLogIdx, FileIdx, GraphLineIdx, OpLogDetailIdx,
    OpLogIdx, TagDetailIdx, TagIdx, WorkspaceIdx,
};

/// Identifies a display row for cursor restore after rebuild.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowKey {
    CommitNode(EntryIdx),
    GraphLink(EntryIdx, GraphLineIdx),
    FileChange(EntryIdx, FileIdx),
    DiffLine(EntryIdx, FileIdx, DiffLineIdx),
    DescriptionLine(EntryIdx, DescriptionLineIdx),
    BookmarkItem(BookmarkIdx),
    BookmarkConflictTarget(BookmarkIdx, BookmarkDetailIdx),
    BookmarkRemoteTarget(BookmarkIdx, BookmarkDetailIdx),
    TagItem(TagIdx),
    TagRemoteTarget(TagIdx, TagDetailIdx),
    OpLogItem(OpLogIdx),
    OpLogDetailLine(OpLogIdx, OpLogDetailIdx),
    OpLogGraphLink(OpLogIdx, GraphLineIdx),
    OpLogLoadMore,
    EvoLogItem(EvoLogIdx),
    EvoLogGraphLink(EvoLogIdx, GraphLineIdx),
    WorkspaceItem(WorkspaceIdx),
    ConflictHeader(EntryIdx, FileIdx, ConflictHunkIdx),
    ConflictSide(
        EntryIdx,
        FileIdx,
        ConflictHunkIdx,
        ConflictSideIdx,
        ConflictLineIdx,
    ),
    ConflictContext(EntryIdx, FileIdx, ConflictHunkIdx, ConflictLineIdx),
}

/// One visual row in the list.
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
    /// A graph link/pad line between evolog entries.
    EvoLogGraphLink {
        evolog_idx: EvoLogIdx,
        line_idx: GraphLineIdx,
    },
    /// A workspace row in the workspace view.
    WorkspaceItem { workspace_idx: WorkspaceIdx },
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
}

impl DisplayRow {
    pub fn key(&self) -> RowKey {
        match *self {
            DisplayRow::CommitNode { entry_idx } => RowKey::CommitNode(entry_idx),
            DisplayRow::DescriptionLine {
                entry_idx,
                line_idx,
            } => RowKey::DescriptionLine(entry_idx, line_idx),
            DisplayRow::GraphLink {
                entry_idx,
                line_idx,
            } => RowKey::GraphLink(entry_idx, line_idx),
            DisplayRow::FileChange {
                entry_idx,
                file_idx,
            } => RowKey::FileChange(entry_idx, file_idx),
            DisplayRow::DiffLine {
                entry_idx,
                file_idx,
                line_idx,
            } => RowKey::DiffLine(entry_idx, file_idx, line_idx),
            DisplayRow::BookmarkItem { bookmark_idx } => RowKey::BookmarkItem(bookmark_idx),
            DisplayRow::BookmarkConflictTarget {
                bookmark_idx,
                target_idx,
            } => RowKey::BookmarkConflictTarget(bookmark_idx, target_idx),
            DisplayRow::BookmarkRemoteTarget {
                bookmark_idx,
                target_idx,
            } => RowKey::BookmarkRemoteTarget(bookmark_idx, target_idx),
            DisplayRow::TagItem { tag_idx } => RowKey::TagItem(tag_idx),
            DisplayRow::TagRemoteTarget {
                tag_idx,
                target_idx,
            } => RowKey::TagRemoteTarget(tag_idx, target_idx),
            DisplayRow::OpLogItem { op_log_idx } => RowKey::OpLogItem(op_log_idx),
            DisplayRow::OpLogDetailLine {
                op_log_idx,
                line_idx,
            } => RowKey::OpLogDetailLine(op_log_idx, line_idx),
            DisplayRow::OpLogGraphLink {
                op_log_idx,
                line_idx,
            } => RowKey::OpLogGraphLink(op_log_idx, line_idx),
            DisplayRow::OpLogLoadMore => RowKey::OpLogLoadMore,
            DisplayRow::EvoLogItem { evolog_idx } => RowKey::EvoLogItem(evolog_idx),
            DisplayRow::EvoLogGraphLink {
                evolog_idx,
                line_idx,
            } => RowKey::EvoLogGraphLink(evolog_idx, line_idx),
            DisplayRow::WorkspaceItem { workspace_idx } => RowKey::WorkspaceItem(workspace_idx),
            DisplayRow::ConflictHeader {
                entry_idx,
                file_idx,
                hunk_idx,
            } => RowKey::ConflictHeader(entry_idx, file_idx, hunk_idx),
            DisplayRow::ConflictSide {
                entry_idx,
                file_idx,
                hunk_idx,
                side_idx,
                line_idx,
            } => RowKey::ConflictSide(entry_idx, file_idx, hunk_idx, side_idx, line_idx),
            DisplayRow::ConflictContext {
                entry_idx,
                file_idx,
                hunk_idx,
                line_idx,
            } => RowKey::ConflictContext(entry_idx, file_idx, hunk_idx, line_idx),
        }
    }
}
