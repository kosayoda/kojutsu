use std::collections::HashMap;

use super::{AppAction, SelectionKindSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    Builtin(AppAction),
    Lua(u16),
}

struct ActionMeta {
    selection_support: SelectionKindSet,
    requires_file: bool,
    requires_conflict: bool,
}

pub struct ActionRegistry {
    builtins: HashMap<AppAction, ActionMeta>,
    lua_meta: Vec<ActionMeta>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            builtins: HashMap::new(),
            lua_meta: Vec::new(),
        };
        reg.register_builtins();
        reg
    }

    fn get(&self, id: ActionId) -> Option<&ActionMeta> {
        match id {
            ActionId::Builtin(a) => self.builtins.get(&a),
            ActionId::Lua(idx) => self.lua_meta.get(idx as usize),
        }
    }

    pub fn register_lua(
        &mut self,
        selection_support: SelectionKindSet,
        requires_file: bool,
        requires_conflict: bool,
    ) -> ActionId {
        let id = self.lua_meta.len() as u16;
        self.lua_meta.push(ActionMeta {
            selection_support,
            requires_file,
            requires_conflict,
        });
        ActionId::Lua(id)
    }

    pub fn selection_support(&self, id: ActionId) -> SelectionKindSet {
        self.get(id)
            .map(|m| m.selection_support)
            .unwrap_or(SelectionKindSet::ALL)
    }

    pub fn requires_file(&self, id: ActionId) -> bool {
        self.get(id).map(|m| m.requires_file).unwrap_or(false)
    }

    pub fn requires_conflict(&self, id: ActionId) -> bool {
        self.get(id).map(|m| m.requires_conflict).unwrap_or(false)
    }

    pub fn find_by_name(&self, name: &str) -> Option<ActionId> {
        name.parse::<AppAction>().ok().map(ActionId::Builtin)
    }

    fn register_builtins(&mut self) {
        use AppAction::*;

        let s = SelectionKindSet::ALL;
        let c = SelectionKindSet::COMMIT;
        let cf = SelectionKindSet::COMMIT.union(SelectionKindSet::FILE);
        let f = SelectionKindSet::FILE;

        let entries: &[(AppAction, SelectionKindSet, bool, bool)] = &[
            (Abandon, c, false, false),
            (Absorb, cf, false, false),
            (Commit, s, false, false),
            (CommitWithMessage, s, false, false),
            (Describe, c, false, false),
            (DescribeInEditor, c, false, false),
            (Diffedit, cf, false, false),
            (Edit, c, false, false),
            (New, c, false, false),
            (NewInsertAfter, c, false, false),
            (NewInsertBefore, c, false, false),
            (Squash, s, false, false),
            (SquashInto, s, false, false),
            (SquashOnto, s, false, false),
            (SquashAfter, s, false, false),
            (SquashBefore, s, false, false),
            (RebaseRevision, c, false, false),
            (RebaseSource, c, false, false),
            (RebaseBranch, c, false, false),
            (Restore, s, false, false),
            (RestoreFrom, s, false, false),
            (RestoreInto, s, false, false),
            (Split, s, false, false),
            (SplitOnto, s, false, false),
            (SplitAfter, s, false, false),
            (SplitBefore, s, false, false),
            (BookmarkCreate, c, false, false),
            (BookmarkSet, c, false, false),
            (BookmarkDelete, c, false, false),
            (BookmarkForget, c, false, false),
            (BookmarkMove, c, false, false),
            (BookmarkRename, c, false, false),
            (BookmarkAdvance, c, false, false),
            (BookmarkTrack, c, false, false),
            (BookmarkUntrack, c, false, false),
            (Undo, c, false, false),
            (Redo, c, false, false),
            (GitFetch, c, false, false),
            (GitFetchAllRemotes, c, false, false),
            (GitPush, c, false, false),
            (GitPushAll, c, false, false),
            (GitPushChange, c, false, false),
            (GitPushBookmark, c, false, false),
            (GitExport, c, false, false),
            (GitImport, c, false, false),
            (Duplicate, c, false, false),
            (DuplicateOnto, c, false, false),
            (Parallelize, c, false, false),
            (SimplifyParents, c, false, false),
            (Revert, c, false, false),
            (ExpandAncestors, c, false, false),
            (Fix, cf, false, false),
            (Run, c, false, false),
            (FileUntrack, f, true, false),
            (FileAnnotate, f, true, false),
            (ResolveOurs, f, false, true),
            (ResolveTheirs, f, false, true),
            (ResolveMergeTool, f, false, true),
            (ConflictPickOurs, f, false, true),
            (ConflictPickTheirs, f, false, true),
            (ConflictPickBase, f, false, true),
            (TagSet, c, false, false),
            (TagDelete, c, false, false),
            (Interdiff, c, false, false),
            (EvoLogInterdiff, c, false, false),
            (AnnotateGoToCommit, c, false, false),
            (AnnotateTimeTravel, c, false, false),
            (AnnotateForward, c, false, false),
            (ToggleAnnotateSeparator, c, false, false),
            (EditFileWorkingCopy, cf, true, false),
            (EditFileAtRevision, cf, true, false),
            (CheckoutAndEditFile, cf, true, false),
        ];

        for &(action, sel, req_file, req_conflict) in entries {
            self.builtins.insert(
                action,
                ActionMeta {
                    selection_support: sel,
                    requires_file: req_file,
                    requires_conflict: req_conflict,
                },
            );
        }
    }
}
