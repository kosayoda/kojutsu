//! Keeping the bookmark, tag and workspace lists on the same item across a
//! reload.
//!
//! Their rows point into lists that every load rebuilds, often in another
//! order: a fetch brings new remote bookmarks, a push changes how one sorts.
//! So a list view's cursor is also remembered by what it is on, by name,
//! before a load rebuilds its list and when the view is left, and put back
//! on that item once the list is rebuilt or the view is shown again.

use super::App;
use crate::types::{ActiveView, BookmarkName, DisplayRow, RemoteName, TagName, WorkspaceName};

/// What a list view's cursor is on, by name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ListAnchor {
    Bookmark {
        name: BookmarkName,
        remote: Option<RemoteName>,
    },
    Tag(TagName),
    Workspace(WorkspaceName),
}

impl App {
    /// Remember what the list view on screen has its cursor on, as a load
    /// starts replacing its list. An anchor already pending is kept: it is
    /// from a load this one interrupted, and still where the user was.
    pub(super) fn anchor_list_cursor(&mut self) {
        let view = self.active_view;
        if self.view_states[view.idx()].anchor.is_some() {
            return;
        }
        let anchor = self.cursor_row().and_then(|row| self.list_anchor_at(row));
        self.view_states[view.idx()].anchor = anchor.map(|anchor| (anchor, self.dag.loads));
    }

    /// A list view is back on screen with its saved row restored. If no load
    /// has rebuilt its list since the anchor was taken, that row is still
    /// right, and finer than the anchor (a bookmark's remote line, say).
    pub(super) fn on_list_shown(&mut self) {
        let view = self.active_view;
        if self.view_states[view.idx()]
            .anchor
            .as_ref()
            .is_some_and(|(_, load)| *load == self.dag.loads)
        {
            self.view_states[view.idx()].anchor = None;
        }
        self.settle_list_anchor(self.dag.stream.is_none());
    }

    /// Put the list view on screen back on its anchor, now that its rows
    /// are rebuilt. The anchor is let go once found, or once the load is
    /// `complete` without it, leaving the cursor at its old position, next
    /// to where the item was.
    pub(super) fn settle_list_anchor(&mut self, complete: bool) {
        let view = self.active_view;
        let Some((anchor, load)) = self.view_states[view.idx()].anchor.take() else {
            return;
        };
        match self.row_of_anchor(&anchor) {
            Some(row) => self.cursor = row,
            None if !complete => self.view_states[view.idx()].anchor = Some((anchor, load)),
            None => {}
        }
    }

    /// Drop the anchor of `view`, which the user has just moved away from.
    pub(super) fn drop_list_anchor(&mut self, view: ActiveView) {
        self.view_states[view.idx()].anchor = None;
    }

    fn list_anchor_at(&self, row: DisplayRow) -> Option<ListAnchor> {
        match row {
            DisplayRow::BookmarkItem { bookmark_idx }
            | DisplayRow::BookmarkConflictTarget { bookmark_idx, .. }
            | DisplayRow::BookmarkRemoteTarget { bookmark_idx, .. } => {
                let entry = self.views.bookmark_entries.get(bookmark_idx.raw())?;
                Some(ListAnchor::Bookmark {
                    name: entry.name.clone(),
                    remote: entry.kind.remote().cloned(),
                })
            }
            DisplayRow::TagItem { tag_idx } | DisplayRow::TagRemoteTarget { tag_idx, .. } => {
                let entry = self.views.tag_entries.get(tag_idx.raw())?;
                Some(ListAnchor::Tag(entry.name.clone()))
            }
            DisplayRow::WorkspaceItem { workspace_idx } => {
                let entry = self.views.workspace_entries.get(workspace_idx.raw())?;
                Some(ListAnchor::Workspace(entry.name.clone()))
            }
            _ => None,
        }
    }

    fn row_of_anchor(&self, anchor: &ListAnchor) -> Option<crate::idx::RowIdx> {
        use crate::idx::{BookmarkIdx, TagIdx, WorkspaceIdx};
        let row = match anchor {
            ListAnchor::Bookmark { name, remote } => {
                let idx = self
                    .views
                    .bookmark_entries
                    .iter()
                    .position(|e| e.name == *name && e.kind.remote() == remote.as_ref())?;
                DisplayRow::BookmarkItem {
                    bookmark_idx: BookmarkIdx::new(idx),
                }
            }
            ListAnchor::Tag(name) => {
                let idx = self
                    .views
                    .tag_entries
                    .iter()
                    .position(|e| e.name == *name)?;
                DisplayRow::TagItem {
                    tag_idx: TagIdx::new(idx),
                }
            }
            ListAnchor::Workspace(name) => {
                let idx = self
                    .views
                    .workspace_entries
                    .iter()
                    .position(|e| e.name == *name)?;
                DisplayRow::WorkspaceItem {
                    workspace_idx: WorkspaceIdx::new(idx),
                }
            }
        };
        self.position_of(row)
    }
}

#[cfg(test)]
mod tests {
    use super::super::App;
    use super::super::test_support::{entry, load};
    use crate::dag::{BookmarkInfo, DagEntry};
    use crate::types::{ActiveView, BookmarkName, DisplayRow, TagName};

    fn bookmarked(mut entry: DagEntry, name: &str) -> DagEntry {
        entry.commit.bookmarks.push(BookmarkInfo {
            name: BookmarkName::new(name),
            is_dirty: false,
            is_tracking: false,
            is_conflicted: false,
        });
        entry
    }

    fn tagged(mut entry: DagEntry, name: &str) -> DagEntry {
        entry.commit.tags.push(TagName::new(name));
        entry
    }

    fn bookmark_at_cursor(app: &App) -> &str {
        let Some(DisplayRow::BookmarkItem { bookmark_idx }) = app.cursor_row() else {
            panic!("cursor not on a bookmark");
        };
        app.views.bookmark_entries[bookmark_idx.raw()].name.as_str()
    }

    fn put_cursor_on_bookmark(app: &mut App, name: &str) {
        let idx = app
            .views
            .bookmark_entries
            .iter()
            .position(|e| e.name.as_str() == name)
            .unwrap();
        app.cursor = app
            .position_of(DisplayRow::BookmarkItem {
                bookmark_idx: crate::idx::BookmarkIdx::new(idx),
            })
            .unwrap();
    }

    fn two_bookmarks() -> Vec<DagEntry> {
        vec![
            bookmarked(entry('a', "a1"), "main"),
            bookmarked(entry('b', "b1"), "dev"),
        ]
    }

    /// With three bookmarks and a new one listed first.
    fn with_a_new_bookmark_first() -> Vec<DagEntry> {
        let mut entries = vec![bookmarked(entry('n', "n1"), "alpha")];
        entries.extend(two_bookmarks());
        entries
    }

    /// A reload that lists a new bookmark above the cursor's keeps the
    /// cursor on its bookmark rather than on whatever took its place.
    #[test]
    fn the_bookmark_view_keeps_its_bookmark_across_a_reload() {
        let mut app = App::for_test();
        load(&mut app, two_bookmarks(), true);
        app.switch_view(ActiveView::Bookmarks);
        put_cursor_on_bookmark(&mut app, "dev");

        load(&mut app, with_a_new_bookmark_first(), true);

        assert_eq!(bookmark_at_cursor(&app), "dev");
    }

    /// The same when the reload happens while another view is on screen.
    #[test]
    fn returning_to_the_bookmark_view_finds_its_bookmark() {
        let mut app = App::for_test();
        load(&mut app, two_bookmarks(), true);
        app.switch_view(ActiveView::Bookmarks);
        put_cursor_on_bookmark(&mut app, "dev");
        app.switch_view(ActiveView::Dag);

        load(&mut app, with_a_new_bookmark_first(), true);
        app.switch_view(ActiveView::Bookmarks);

        assert_eq!(bookmark_at_cursor(&app), "dev");
    }

    #[test]
    fn the_tag_view_keeps_its_tag_across_a_reload() {
        let mut app = App::for_test();
        load(
            &mut app,
            vec![
                tagged(entry('a', "a1"), "v1"),
                tagged(entry('b', "b1"), "v2"),
            ],
            true,
        );
        app.switch_view(ActiveView::Tags);
        app.cursor = app
            .position_of(DisplayRow::TagItem {
                tag_idx: crate::idx::TagIdx::new(1),
            })
            .unwrap();

        let entries = vec![
            tagged(entry('n', "n1"), "v0"),
            tagged(entry('a', "a1"), "v1"),
            tagged(entry('b', "b1"), "v2"),
        ];
        load(&mut app, entries, true);

        let Some(DisplayRow::TagItem { tag_idx }) = app.cursor_row() else {
            panic!("cursor not on a tag");
        };
        assert_eq!(app.views.tag_entries[tag_idx.raw()].name.as_str(), "v2");
    }

    /// Moving in the view lets the anchor go: a later rebuild mustn't pull
    /// the cursor back to where it was before the move.
    #[test]
    fn moving_in_the_view_lets_the_anchor_go() {
        let mut app = App::for_test();
        load(&mut app, two_bookmarks(), true);
        app.switch_view(ActiveView::Bookmarks);
        put_cursor_on_bookmark(&mut app, "dev");
        app.anchor_list_cursor();

        app.as_user_input(|app| put_cursor_on_bookmark(app, "main"));
        app.settle_list_anchor(true);

        assert_eq!(bookmark_at_cursor(&app), "main");
    }
}
