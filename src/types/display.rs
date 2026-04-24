use crate::idx::{
    BookmarkDetailIdx, BookmarkIdx, DescriptionLineIdx, DiffLineIdx, EntryIdx, FileIdx,
    GraphLineIdx, TagDetailIdx, TagIdx,
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
        }
    }
}
