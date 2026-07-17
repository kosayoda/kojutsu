use super::{AppAction, SelectionKindSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    Builtin(AppAction),
    Lua(u16),
}

/// What context an action needs to be available. `File` and `Conflict`
/// were previously two independent bools that were never both set — an
/// enum makes the exclusivity structural.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requires {
    /// Available anywhere.
    Nothing,
    /// Needs the cursor on a file (or its diff).
    File,
    /// Needs the cursor on a conflict hunk (or conflicted file).
    Conflict,
}

/// Static metadata for an action: which selection kinds it supports and
/// what context it requires.
#[derive(Debug, Clone, Copy)]
pub struct ActionMeta {
    pub selection_support: SelectionKindSet,
    pub requires: Requires,
}

impl AppAction {
    /// Static metadata for this action.
    pub fn meta(self) -> ActionMeta {
        use AppAction::*;
        let s = SelectionKindSet::ALL;
        let c = SelectionKindSet::COMMIT;
        let cf = SelectionKindSet::COMMIT.union(SelectionKindSet::FILE);
        let f = SelectionKindSet::FILE;
        let m = |selection_support, requires| ActionMeta {
            selection_support,
            requires,
        };
        match self {
            // Conflict hunk actions — need the cursor on a conflict.
            ResolveOurs | ResolveTheirs | ResolveMergeTool | ConflictPickOurs
            | ConflictPickTheirs | ConflictPickBase | ConflictUnpick | ConflictApplyPicks
            | ConflictEditFile | ConflictEditHunk => m(f, Requires::Conflict),

            // File actions — need the cursor on a file.
            FileUntrack | FileAnnotate => m(f, Requires::File),
            EditFileWorkingCopy | EditFileAtRevision | CheckoutAndEditFile => m(cf, Requires::File),

            // Commit + file selection, no context requirement.
            Absorb | Diffedit | Fix => m(cf, Requires::Nothing),

            // Whole-selection commands (operate on any selection kind).
            Commit | CommitWithMessage | Squash | SquashInto | SquashOnto | SquashAfter
            | SquashBefore | Restore | RestoreFrom | RestoreInto | Split | SplitOnto
            | SplitAfter | SplitBefore => m(s, Requires::Nothing),

            // Commit-scoped commands.
            Abandon
            | Describe
            | DescribeInEditor
            | Edit
            | New
            | NewInsertAfter
            | NewInsertBefore
            | RebaseRevision
            | RebaseSource
            | RebaseBranch
            | BookmarkCreate
            | BookmarkSet
            | BookmarkDelete
            | BookmarkForget
            | BookmarkMove
            | BookmarkRename
            | BookmarkAdvance
            | BookmarkTrack
            | BookmarkUntrack
            | Undo
            | Redo
            | GitFetch
            | GitFetchAllRemotes
            | GitPush
            | GitPushAll
            | GitPushChange
            | GitPushBookmark
            | GitExport
            | GitImport
            | Duplicate
            | DuplicateOnto
            | Parallelize
            | SimplifyParents
            | Revert
            | ExpandAncestors
            | Run
            | TagSet
            | TagDelete
            | Interdiff
            | EvoLogInterdiff
            | AnnotateGoToCommit
            | AnnotateTimeTravel
            | AnnotateForward
            | ToggleAnnotateSeparator => m(c, Requires::Nothing),

            // Navigation, toggles, view switches, and view-local actions —
            // no selection semantics, available anywhere.
            Quit
            | MoveDown
            | MoveUp
            | MoveDownSection
            | MoveUpSection
            | PageDown
            | PageUp
            | JumpToWorkingCopy
            | MoveToTop
            | MoveToBottom
            | MoveToScreenTop
            | MoveToScreenMiddle
            | MoveToScreenBottom
            | ScrollLeft
            | ScrollRight
            | ToggleFold
            | Refresh
            | ExpandDescendants
            | EditRevset
            | EditRevsetInEditor
            | ResetRevset
            | ToggleConflictedRevset
            | ShowHelp
            | ArrangeUp
            | ArrangeDown
            | ToggleIgnoreImmutable
            | ToggleIgnoreWorkingCopy
            | ToggleDebug
            | ToggleGitDiff
            | ToggleLineNumbers
            | ToggleDiffUnderline
            | WorkspaceAdd
            | WorkspaceForget
            | WorkspaceList
            | WorkspaceRename
            | ToggleSelect
            | EnterVisualMode
            | StartSearch
            | NextMatch
            | PrevMatch
            | NextConflict
            | PrevConflict
            | SelectPreset
            | SwitchPreset1
            | SwitchPreset2
            | SwitchPreset3
            | SwitchPreset4
            | SwitchPreset5
            | SwitchToDagView
            | SwitchToBookmarkView
            | SwitchToTagView
            | BookmarkViewDelete
            | BookmarkViewTrack
            | BookmarkViewUntrack
            | BookmarkViewPush
            | BookmarkViewJumpToCommit
            | BookmarkViewEdit
            | BookmarkViewRename
            | BookmarkViewMove
            | BookmarkViewForget
            | BookmarkViewSet
            | BookmarkViewFetchDefault
            | BookmarkViewFetchBookmark
            | BookmarkViewFetchAllRemotes
            | BookmarkViewInterdiff
            | TagViewDelete
            | TagViewSet
            | TagViewJumpToCommit
            | TagViewEdit
            | SwitchToOpLogView
            | SwitchToWorkspaceView
            | SwitchToEvoLogView
            | SwitchToCommandLogView
            | Jump
            | WorkspaceViewForget
            | WorkspaceViewJumpToCommit
            | EvoLogRestore
            | EvoLogEdit
            | EvoLogNew
            | OpLogRestore
            | OpLogRevert
            | OpLogAbandon
            | OpLogFilterWorkspace
            | CommandMode
            | FileList
            | RepeatLast => m(s, Requires::Nothing),
        }
    }
}

pub struct ActionRegistry {
    /// Metadata for Lua-registered actions, indexed by `ActionId::Lua`.
    /// Builtin metadata comes from [`AppAction::meta`].
    lua_meta: Vec<ActionMeta>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self {
            lua_meta: Vec::new(),
        }
    }

    fn meta(&self, id: ActionId) -> Option<ActionMeta> {
        match id {
            ActionId::Builtin(a) => Some(a.meta()),
            ActionId::Lua(idx) => self.lua_meta.get(idx as usize).copied(),
        }
    }

    /// Register a Lua action's selection support. Lua actions are never
    /// file- or conflict-gated.
    pub fn register_lua(&mut self, selection_support: SelectionKindSet) -> ActionId {
        let id = self.lua_meta.len() as u16;
        self.lua_meta.push(ActionMeta {
            selection_support,
            requires: Requires::Nothing,
        });
        ActionId::Lua(id)
    }

    pub fn selection_support(&self, id: ActionId) -> SelectionKindSet {
        self.meta(id)
            .map(|m| m.selection_support)
            .unwrap_or(SelectionKindSet::ALL)
    }

    pub fn requires_file(&self, id: ActionId) -> bool {
        self.meta(id).is_some_and(|m| m.requires == Requires::File)
    }

    pub fn requires_conflict(&self, id: ActionId) -> bool {
        self.meta(id)
            .is_some_and(|m| m.requires == Requires::Conflict)
    }

    pub fn find_by_name(&self, name: &str) -> Option<ActionId> {
        name.parse::<AppAction>().ok().map(ActionId::Builtin)
    }
}
