mod bindings;
mod help;
mod registry;
mod trie;

use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use bindings::{BindTarget, BindingSpec, Scope, default_bindings};
pub use help::{HelpEntry, HelpGroup, help_entries, select_mode_help_entries};
pub use registry::{ActionId, ActionRegistry, Availability};
pub use trie::{Keymap, Keymaps, LookupResult, TrieNode};

use crate::types::GLOBAL_TOGGLES;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SelectionKindSet: u8 {
        const COMMIT = 1 << 0;
        const FILE   = 1 << 1;
        const LINE   = 1 << 2;
    }
}

impl SelectionKindSet {
    pub const ALL: Self = Self::COMMIT.union(Self::FILE).union(Self::LINE);

    /// Whether this selection rules out an action supporting only `support`.
    /// `self` is what is selected — empty means nothing is, which rules
    /// nothing out.
    ///
    /// The one place this rule lives. Dispatch refuses on it; the help panel
    /// and submenu grey out on it plus the cursor-context terms
    /// ([`Availability::blocks`]).
    pub fn blocked_by(self, support: SelectionKindSet) -> bool {
        !self.is_empty() && !support.contains(self)
    }
}

pub const CONFLICT_PREFIX: &str = "conflict";
pub const FILE_PREFIX: &str = "file";

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CommandFlags: u16 {
        const IGNORE_IMMUTABLE    = 1 << 0;
        const IGNORE_WORKING_COPY = 1 << 1;
        const DEBUG               = 1 << 2;
        const NO_EDIT             = 1 << 3;
        const RETAIN_BOOKMARKS    = 1 << 4;
        const RESTORE_DESCENDANTS = 1 << 5;
        const INTERACTIVE         = 1 << 6;
        const KEEP_EMPTIED        = 1 << 7;
        const ALLOW_BACKWARDS     = 1 << 8;
        const DRY_RUN             = 1 << 9;
        const PARALLEL            = 1 << 10;
        const CLEAN               = 1 << 11;
    }
}

/// Every dispatchable action. Unit-only by design: the snake_case name each
/// variant serializes to is the stable identifier Lua plugins bind and hook
/// on, and `EnumIter` is what exposes the full set to the Lua API and the
/// generated type definitions — data-carrying variants would silently fall
/// out of both.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum::EnumIter,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[strum(serialize_all = "snake_case")]
pub enum AppAction {
    Quit,
    MoveDown,
    MoveUp,
    MoveDownSection,
    MoveUpSection,
    PageDown,
    PageUp,
    JumpToWorkingCopy,
    MoveToTop,
    MoveToBottom,
    MoveToScreenTop,
    MoveToScreenMiddle,
    MoveToScreenBottom,
    ScrollLeft,
    ScrollRight,
    ToggleFold,
    Refresh,
    ExpandAncestors,
    ExpandDescendants,
    Abandon,
    Absorb,
    Commit,
    CommitWithMessage,
    Describe,
    DescribeInEditor,
    Diffedit,
    Edit,
    New,
    NewInsertAfter,
    NewInsertBefore,
    Squash,
    SquashInto,
    SquashOnto,
    SquashAfter,
    SquashBefore,
    RebaseRevision,
    RebaseSource,
    RebaseBranch,
    Restore,
    RestoreFrom,
    RestoreInto,
    Split,
    SplitOnto,
    SplitAfter,
    SplitBefore,
    EditRevset,
    EditRevsetInEditor,
    ResetRevset,
    ToggleConflictedRevset,
    BookmarkCreate,
    BookmarkSet,
    BookmarkDelete,
    BookmarkForget,
    BookmarkMove,
    BookmarkRename,
    BookmarkAdvance,
    BookmarkTrack,
    BookmarkUntrack,
    ShowHelp,
    Undo,
    Redo,
    GitFetch,
    GitFetchAllRemotes,
    GitPush,
    GitPushAll,
    GitPushChange,
    GitExport,
    GitImport,
    Duplicate,
    DuplicateOnto,
    Parallelize,
    SimplifyParents,
    Revert,
    ArrangeUp,
    ArrangeDown,
    Fix,
    Run,
    FileUntrack,
    ResolveOurs,
    ResolveTheirs,
    ResolveMergeTool,
    ConflictPickOurs,
    ConflictPickTheirs,
    ConflictPickBase,
    ConflictUnpick,
    ConflictApplyPicks,
    ConflictEditFile,
    ConflictEditHunk,
    ToggleIgnoreImmutable,
    ToggleIgnoreWorkingCopy,
    ToggleDebug,
    ToggleGitDiff,
    ToggleLineNumbers,
    ToggleDiffUnderline,
    WorkspaceAdd,
    WorkspaceForget,
    WorkspaceList,
    WorkspaceRename,
    ToggleSelect,
    EnterVisualMode,
    StartSearch,
    NextMatch,
    PrevMatch,
    NextConflict,
    PrevConflict,
    GitPushBookmark,
    TagSet,
    TagDelete,
    SelectPreset,
    #[strum(serialize = "switch_preset_1")]
    SwitchPreset1,
    #[strum(serialize = "switch_preset_2")]
    SwitchPreset2,
    #[strum(serialize = "switch_preset_3")]
    SwitchPreset3,
    #[strum(serialize = "switch_preset_4")]
    SwitchPreset4,
    #[strum(serialize = "switch_preset_5")]
    SwitchPreset5,
    SwitchToDagView,
    SwitchToBookmarkView,
    SwitchToTagView,
    BookmarkViewDelete,
    BookmarkViewTrack,
    BookmarkViewUntrack,
    BookmarkViewPush,
    BookmarkViewJumpToCommit,
    BookmarkViewEdit,
    BookmarkViewRename,
    BookmarkViewMove,
    BookmarkViewForget,
    BookmarkViewSet,
    BookmarkViewFetchDefault,
    BookmarkViewFetchBookmark,
    BookmarkViewFetchAllRemotes,
    BookmarkViewInterdiff,
    TagViewDelete,
    TagViewSet,
    TagViewJumpToCommit,
    TagViewEdit,
    SwitchToOpLogView,
    SwitchToWorkspaceView,
    #[strum(serialize = "switch_to_evolog_view")]
    SwitchToEvoLogView,
    SwitchToCommandLogView,
    Jump,
    WorkspaceViewForget,
    WorkspaceViewJumpToCommit,
    #[strum(serialize = "evolog_restore")]
    EvoLogRestore,
    #[strum(serialize = "evolog_edit")]
    EvoLogEdit,
    #[strum(serialize = "evolog_new")]
    EvoLogNew,
    Interdiff,
    #[strum(serialize = "evolog_interdiff")]
    EvoLogInterdiff,
    FileAnnotate,
    AnnotateGoToCommit,
    AnnotateTimeTravel,
    AnnotateForward,
    ToggleAnnotateSeparator,
    EditFileWorkingCopy,
    EditFileAtRevision,
    CheckoutAndEditFile,
    OpLogRestore,
    OpLogRevert,
    OpLogAbandon,
    OpLogFilterWorkspace,
    CommandMode,
    FileList,
    RepeatLast,
}

impl AppAction {
    /// Zero-based preset slot for the switch-preset actions.
    pub fn preset_slot(self) -> Option<usize> {
        match self {
            AppAction::SwitchPreset1 => Some(0),
            AppAction::SwitchPreset2 => Some(1),
            AppAction::SwitchPreset3 => Some(2),
            AppAction::SwitchPreset4 => Some(3),
            AppAction::SwitchPreset5 => Some(4),
            _ => None,
        }
    }

    /// Whether the action only moves the cursor or viewport, leaving the repo
    /// and the selection untouched. These stay available inside target- and
    /// commit-select, where every other action would either mutate mid-pick or
    /// collide with the mode's own keys (space toggles a target, Enter
    /// confirms, Esc cancels).
    pub fn is_cursor_navigation(self) -> bool {
        match self {
            AppAction::MoveDown
            | AppAction::MoveUp
            | AppAction::MoveDownSection
            | AppAction::MoveUpSection
            | AppAction::PageDown
            | AppAction::PageUp
            | AppAction::JumpToWorkingCopy
            | AppAction::MoveToTop
            | AppAction::MoveToBottom
            | AppAction::MoveToScreenTop
            | AppAction::MoveToScreenMiddle
            | AppAction::MoveToScreenBottom
            | AppAction::ScrollLeft
            | AppAction::ScrollRight
            | AppAction::ToggleFold
            | AppAction::Jump
            | AppAction::StartSearch
            | AppAction::NextMatch
            | AppAction::PrevMatch
            | AppAction::ShowHelp => true,

            AppAction::Quit
            | AppAction::Refresh
            | AppAction::ExpandAncestors
            | AppAction::ExpandDescendants
            | AppAction::Abandon
            | AppAction::Absorb
            | AppAction::Commit
            | AppAction::CommitWithMessage
            | AppAction::Describe
            | AppAction::DescribeInEditor
            | AppAction::Diffedit
            | AppAction::Edit
            | AppAction::New
            | AppAction::NewInsertAfter
            | AppAction::NewInsertBefore
            | AppAction::Squash
            | AppAction::SquashInto
            | AppAction::SquashOnto
            | AppAction::SquashAfter
            | AppAction::SquashBefore
            | AppAction::RebaseRevision
            | AppAction::RebaseSource
            | AppAction::RebaseBranch
            | AppAction::Restore
            | AppAction::RestoreFrom
            | AppAction::RestoreInto
            | AppAction::Split
            | AppAction::SplitOnto
            | AppAction::SplitAfter
            | AppAction::SplitBefore
            | AppAction::EditRevset
            | AppAction::EditRevsetInEditor
            | AppAction::ResetRevset
            | AppAction::ToggleConflictedRevset
            | AppAction::BookmarkCreate
            | AppAction::BookmarkSet
            | AppAction::BookmarkDelete
            | AppAction::BookmarkForget
            | AppAction::BookmarkMove
            | AppAction::BookmarkRename
            | AppAction::BookmarkAdvance
            | AppAction::BookmarkTrack
            | AppAction::BookmarkUntrack
            | AppAction::Undo
            | AppAction::Redo
            | AppAction::GitFetch
            | AppAction::GitFetchAllRemotes
            | AppAction::GitPush
            | AppAction::GitPushAll
            | AppAction::GitPushChange
            | AppAction::GitExport
            | AppAction::GitImport
            | AppAction::Duplicate
            | AppAction::DuplicateOnto
            | AppAction::Parallelize
            | AppAction::SimplifyParents
            | AppAction::Revert
            | AppAction::ArrangeUp
            | AppAction::ArrangeDown
            | AppAction::Fix
            | AppAction::Run
            | AppAction::FileUntrack
            | AppAction::ResolveOurs
            | AppAction::ResolveTheirs
            | AppAction::ResolveMergeTool
            | AppAction::ConflictPickOurs
            | AppAction::ConflictPickTheirs
            | AppAction::ConflictPickBase
            | AppAction::ConflictUnpick
            | AppAction::ConflictApplyPicks
            | AppAction::ConflictEditFile
            | AppAction::ConflictEditHunk
            | AppAction::ToggleIgnoreImmutable
            | AppAction::ToggleIgnoreWorkingCopy
            | AppAction::ToggleDebug
            | AppAction::ToggleGitDiff
            | AppAction::ToggleLineNumbers
            | AppAction::ToggleDiffUnderline
            | AppAction::WorkspaceAdd
            | AppAction::WorkspaceForget
            | AppAction::WorkspaceList
            | AppAction::WorkspaceRename
            | AppAction::ToggleSelect
            | AppAction::EnterVisualMode
            | AppAction::NextConflict
            | AppAction::PrevConflict
            | AppAction::GitPushBookmark
            | AppAction::TagSet
            | AppAction::TagDelete
            | AppAction::SelectPreset
            | AppAction::SwitchPreset1
            | AppAction::SwitchPreset2
            | AppAction::SwitchPreset3
            | AppAction::SwitchPreset4
            | AppAction::SwitchPreset5
            | AppAction::SwitchToDagView
            | AppAction::SwitchToBookmarkView
            | AppAction::SwitchToTagView
            | AppAction::BookmarkViewDelete
            | AppAction::BookmarkViewTrack
            | AppAction::BookmarkViewUntrack
            | AppAction::BookmarkViewPush
            | AppAction::BookmarkViewJumpToCommit
            | AppAction::BookmarkViewEdit
            | AppAction::BookmarkViewRename
            | AppAction::BookmarkViewMove
            | AppAction::BookmarkViewForget
            | AppAction::BookmarkViewSet
            | AppAction::BookmarkViewFetchDefault
            | AppAction::BookmarkViewFetchBookmark
            | AppAction::BookmarkViewFetchAllRemotes
            | AppAction::BookmarkViewInterdiff
            | AppAction::TagViewDelete
            | AppAction::TagViewSet
            | AppAction::TagViewJumpToCommit
            | AppAction::TagViewEdit
            | AppAction::SwitchToOpLogView
            | AppAction::SwitchToWorkspaceView
            | AppAction::SwitchToEvoLogView
            | AppAction::SwitchToCommandLogView
            | AppAction::WorkspaceViewForget
            | AppAction::WorkspaceViewJumpToCommit
            | AppAction::EvoLogRestore
            | AppAction::EvoLogEdit
            | AppAction::EvoLogNew
            | AppAction::Interdiff
            | AppAction::EvoLogInterdiff
            | AppAction::FileAnnotate
            | AppAction::AnnotateGoToCommit
            | AppAction::AnnotateTimeTravel
            | AppAction::AnnotateForward
            | AppAction::ToggleAnnotateSeparator
            | AppAction::EditFileWorkingCopy
            | AppAction::EditFileAtRevision
            | AppAction::CheckoutAndEditFile
            | AppAction::OpLogRestore
            | AppAction::OpLogRevert
            | AppAction::OpLogAbandon
            | AppAction::OpLogFilterWorkspace
            | AppAction::CommandMode
            | AppAction::FileList
            | AppAction::RepeatLast => false,
        }
    }

    pub fn is_repeatable(self) -> bool {
        match self {
            AppAction::ArrangeUp
            | AppAction::ArrangeDown
            | AppAction::Abandon
            | AppAction::Fix
            | AppAction::Absorb
            | AppAction::Parallelize
            | AppAction::SimplifyParents
            | AppAction::Undo
            | AppAction::Redo
            | AppAction::Duplicate
            | AppAction::ConflictPickOurs
            | AppAction::ConflictPickTheirs
            | AppAction::ConflictPickBase
            | AppAction::ConflictUnpick
            | AppAction::ConflictApplyPicks
            | AppAction::ResolveOurs
            | AppAction::ResolveTheirs
            | AppAction::NextConflict
            | AppAction::PrevConflict => true,

            AppAction::Quit
            | AppAction::MoveDown
            | AppAction::MoveUp
            | AppAction::MoveDownSection
            | AppAction::MoveUpSection
            | AppAction::PageDown
            | AppAction::PageUp
            | AppAction::JumpToWorkingCopy
            | AppAction::ConflictEditFile
            | AppAction::ConflictEditHunk
            | AppAction::MoveToTop
            | AppAction::MoveToBottom
            | AppAction::MoveToScreenTop
            | AppAction::MoveToScreenMiddle
            | AppAction::MoveToScreenBottom
            | AppAction::ScrollLeft
            | AppAction::ScrollRight
            | AppAction::ToggleFold
            | AppAction::Refresh
            | AppAction::ExpandAncestors
            | AppAction::ExpandDescendants
            | AppAction::Commit
            | AppAction::CommitWithMessage
            | AppAction::Describe
            | AppAction::DescribeInEditor
            | AppAction::Diffedit
            | AppAction::Edit
            | AppAction::New
            | AppAction::NewInsertAfter
            | AppAction::NewInsertBefore
            | AppAction::Squash
            | AppAction::SquashInto
            | AppAction::SquashOnto
            | AppAction::SquashAfter
            | AppAction::SquashBefore
            | AppAction::RebaseRevision
            | AppAction::RebaseSource
            | AppAction::RebaseBranch
            | AppAction::Restore
            | AppAction::RestoreFrom
            | AppAction::RestoreInto
            | AppAction::Split
            | AppAction::SplitOnto
            | AppAction::SplitAfter
            | AppAction::SplitBefore
            | AppAction::EditRevset
            | AppAction::EditRevsetInEditor
            | AppAction::ResetRevset
            | AppAction::ToggleConflictedRevset
            | AppAction::BookmarkCreate
            | AppAction::BookmarkSet
            | AppAction::BookmarkDelete
            | AppAction::BookmarkForget
            | AppAction::BookmarkMove
            | AppAction::BookmarkRename
            | AppAction::BookmarkAdvance
            | AppAction::BookmarkTrack
            | AppAction::BookmarkUntrack
            | AppAction::ShowHelp
            | AppAction::GitFetch
            | AppAction::GitFetchAllRemotes
            | AppAction::GitPush
            | AppAction::GitPushAll
            | AppAction::GitPushChange
            | AppAction::GitExport
            | AppAction::GitImport
            | AppAction::DuplicateOnto
            | AppAction::Revert
            | AppAction::FileUntrack
            | AppAction::ResolveMergeTool
            | AppAction::ToggleIgnoreImmutable
            | AppAction::ToggleIgnoreWorkingCopy
            | AppAction::ToggleDebug
            | AppAction::ToggleGitDiff
            | AppAction::ToggleLineNumbers
            | AppAction::ToggleDiffUnderline
            | AppAction::WorkspaceAdd
            | AppAction::WorkspaceForget
            | AppAction::WorkspaceList
            | AppAction::WorkspaceRename
            | AppAction::ToggleSelect
            | AppAction::EnterVisualMode
            | AppAction::StartSearch
            | AppAction::NextMatch
            | AppAction::PrevMatch
            | AppAction::GitPushBookmark
            | AppAction::TagSet
            | AppAction::TagDelete
            | AppAction::SelectPreset
            | AppAction::SwitchPreset1
            | AppAction::SwitchPreset2
            | AppAction::SwitchPreset3
            | AppAction::SwitchPreset4
            | AppAction::SwitchPreset5
            | AppAction::SwitchToDagView
            | AppAction::SwitchToBookmarkView
            | AppAction::SwitchToTagView
            | AppAction::BookmarkViewDelete
            | AppAction::BookmarkViewTrack
            | AppAction::BookmarkViewUntrack
            | AppAction::BookmarkViewPush
            | AppAction::BookmarkViewJumpToCommit
            | AppAction::BookmarkViewEdit
            | AppAction::BookmarkViewRename
            | AppAction::BookmarkViewMove
            | AppAction::BookmarkViewForget
            | AppAction::BookmarkViewSet
            | AppAction::BookmarkViewFetchDefault
            | AppAction::BookmarkViewFetchBookmark
            | AppAction::BookmarkViewFetchAllRemotes
            | AppAction::BookmarkViewInterdiff
            | AppAction::TagViewDelete
            | AppAction::TagViewSet
            | AppAction::TagViewJumpToCommit
            | AppAction::TagViewEdit
            | AppAction::SwitchToOpLogView
            | AppAction::SwitchToWorkspaceView
            | AppAction::SwitchToEvoLogView
            | AppAction::SwitchToCommandLogView
            | AppAction::Jump
            | AppAction::WorkspaceViewForget
            | AppAction::WorkspaceViewJumpToCommit
            | AppAction::EvoLogRestore
            | AppAction::EvoLogEdit
            | AppAction::EvoLogNew
            | AppAction::Interdiff
            | AppAction::EvoLogInterdiff
            | AppAction::FileAnnotate
            | AppAction::AnnotateGoToCommit
            | AppAction::AnnotateTimeTravel
            | AppAction::AnnotateForward
            | AppAction::ToggleAnnotateSeparator
            | AppAction::EditFileWorkingCopy
            | AppAction::EditFileAtRevision
            | AppAction::CheckoutAndEditFile
            | AppAction::OpLogRestore
            | AppAction::OpLogRevert
            | AppAction::OpLogAbandon
            | AppAction::OpLogFilterWorkspace
            | AppAction::CommandMode
            | AppAction::FileList
            | AppAction::Run
            | AppAction::RepeatLast => false,
        }
    }

    pub fn is_mutation(self) -> bool {
        match self {
            AppAction::Abandon
            | AppAction::Absorb
            | AppAction::Commit
            | AppAction::CommitWithMessage
            | AppAction::Describe
            | AppAction::DescribeInEditor
            | AppAction::Diffedit
            | AppAction::Edit
            | AppAction::New
            | AppAction::NewInsertAfter
            | AppAction::NewInsertBefore
            | AppAction::Squash
            | AppAction::SquashInto
            | AppAction::SquashOnto
            | AppAction::SquashAfter
            | AppAction::SquashBefore
            | AppAction::RebaseRevision
            | AppAction::RebaseSource
            | AppAction::RebaseBranch
            | AppAction::Restore
            | AppAction::RestoreFrom
            | AppAction::RestoreInto
            | AppAction::Split
            | AppAction::SplitOnto
            | AppAction::SplitAfter
            | AppAction::SplitBefore
            | AppAction::BookmarkCreate
            | AppAction::BookmarkSet
            | AppAction::BookmarkDelete
            | AppAction::BookmarkForget
            | AppAction::BookmarkMove
            | AppAction::BookmarkRename
            | AppAction::BookmarkAdvance
            | AppAction::BookmarkTrack
            | AppAction::BookmarkUntrack
            | AppAction::Undo
            | AppAction::Redo
            | AppAction::GitFetch
            | AppAction::GitFetchAllRemotes
            | AppAction::GitPush
            | AppAction::GitPushAll
            | AppAction::GitPushChange
            | AppAction::GitPushBookmark
            | AppAction::GitExport
            | AppAction::GitImport
            | AppAction::Duplicate
            | AppAction::DuplicateOnto
            | AppAction::Parallelize
            | AppAction::SimplifyParents
            | AppAction::Revert
            | AppAction::ArrangeUp
            | AppAction::ArrangeDown
            | AppAction::Fix
            | AppAction::FileUntrack
            | AppAction::ResolveOurs
            | AppAction::ResolveTheirs
            | AppAction::ResolveMergeTool
            | AppAction::ConflictApplyPicks
            | AppAction::ConflictEditFile
            | AppAction::WorkspaceAdd
            | AppAction::WorkspaceForget
            | AppAction::WorkspaceRename
            | AppAction::TagSet
            | AppAction::TagDelete
            | AppAction::BookmarkViewDelete
            | AppAction::BookmarkViewTrack
            | AppAction::BookmarkViewUntrack
            | AppAction::BookmarkViewPush
            | AppAction::BookmarkViewEdit
            | AppAction::BookmarkViewRename
            | AppAction::BookmarkViewMove
            | AppAction::BookmarkViewForget
            | AppAction::BookmarkViewSet
            | AppAction::BookmarkViewFetchDefault
            | AppAction::BookmarkViewFetchBookmark
            | AppAction::BookmarkViewFetchAllRemotes
            | AppAction::BookmarkViewInterdiff
            | AppAction::TagViewDelete
            | AppAction::TagViewSet
            | AppAction::TagViewEdit
            | AppAction::EvoLogRestore
            | AppAction::EvoLogEdit
            | AppAction::EvoLogNew
            | AppAction::OpLogRestore
            | AppAction::OpLogRevert
            | AppAction::OpLogAbandon
            | AppAction::Run
            | AppAction::CommandMode => true,

            AppAction::Quit
            | AppAction::MoveDown
            | AppAction::MoveUp
            | AppAction::MoveDownSection
            | AppAction::MoveUpSection
            | AppAction::PageDown
            | AppAction::PageUp
            | AppAction::JumpToWorkingCopy
            | AppAction::MoveToTop
            | AppAction::MoveToBottom
            | AppAction::MoveToScreenTop
            | AppAction::MoveToScreenMiddle
            | AppAction::MoveToScreenBottom
            | AppAction::ScrollLeft
            | AppAction::ScrollRight
            | AppAction::ToggleFold
            | AppAction::Refresh
            | AppAction::ExpandAncestors
            | AppAction::ExpandDescendants
            | AppAction::ShowHelp
            | AppAction::Jump
            | AppAction::ToggleSelect
            | AppAction::EnterVisualMode
            | AppAction::StartSearch
            | AppAction::NextMatch
            | AppAction::NextConflict
            | AppAction::PrevConflict
            | AppAction::PrevMatch
            | AppAction::SelectPreset
            | AppAction::SwitchPreset1
            | AppAction::SwitchPreset2
            | AppAction::SwitchPreset3
            | AppAction::SwitchPreset4
            | AppAction::SwitchPreset5
            | AppAction::SwitchToDagView
            | AppAction::SwitchToBookmarkView
            | AppAction::SwitchToTagView
            | AppAction::SwitchToOpLogView
            | AppAction::SwitchToWorkspaceView
            | AppAction::SwitchToEvoLogView
            | AppAction::SwitchToCommandLogView
            | AppAction::ToggleIgnoreImmutable
            | AppAction::ToggleIgnoreWorkingCopy
            | AppAction::ToggleDebug
            | AppAction::ToggleGitDiff
            | AppAction::ToggleLineNumbers
            | AppAction::ToggleDiffUnderline
            | AppAction::ToggleAnnotateSeparator
            | AppAction::WorkspaceList
            | AppAction::WorkspaceViewForget
            | AppAction::WorkspaceViewJumpToCommit
            | AppAction::Interdiff
            | AppAction::EvoLogInterdiff
            | AppAction::FileAnnotate
            | AppAction::FileList
            | AppAction::AnnotateGoToCommit
            | AppAction::AnnotateTimeTravel
            | AppAction::AnnotateForward
            | AppAction::EditFileWorkingCopy
            | AppAction::EditFileAtRevision
            | AppAction::CheckoutAndEditFile
            | AppAction::OpLogFilterWorkspace
            | AppAction::EditRevset
            | AppAction::EditRevsetInEditor
            | AppAction::ResetRevset
            | AppAction::ToggleConflictedRevset
            | AppAction::ConflictPickOurs
            | AppAction::ConflictPickTheirs
            | AppAction::ConflictPickBase
            | AppAction::ConflictUnpick
            | AppAction::ConflictEditHunk
            | AppAction::TagViewJumpToCommit
            | AppAction::BookmarkViewJumpToCommit
            | AppAction::RepeatLast => false,
        }
    }
}

pub fn convert_modifiers(mods: &KeyModifiers) -> keymap_parser::Modifiers {
    const MODIFIER_MAP: [(KeyModifiers, Modifier); 4] = [
        (KeyModifiers::ALT, Modifier::Alt),
        (KeyModifiers::CONTROL, Modifier::Ctrl),
        (KeyModifiers::META, Modifier::Cmd),
        (KeyModifiers::SHIFT, Modifier::Shift),
    ];
    MODIFIER_MAP.iter().fold(0u8, |acc, (ct_mod, kp_mod)| {
        if mods.contains(*ct_mod) {
            acc | *kp_mod as u8
        } else {
            acc
        }
    })
}

pub fn key_event_to_node(key: &KeyEvent) -> Option<Node> {
    let k = match key.code {
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Char(' ') => Key::Space,
        KeyCode::Char(c)
            if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_uppercase() =>
        {
            Key::Char(c.to_ascii_lowercase())
        }
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Delete => Key::Delete,
        KeyCode::Down => Key::Down,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        KeyCode::Home => Key::Home,
        KeyCode::Insert => Key::Insert,
        KeyCode::Left => Key::Left,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::Right => Key::Right,
        KeyCode::Tab => Key::Tab,
        KeyCode::Up => Key::Up,
        _ => return None,
    };

    let modifiers = convert_modifiers(&key.modifiers);
    Some(Node::new(modifiers, k))
}

pub fn display_key(node: &Node) -> String {
    if node.modifiers == Modifier::Shift as u8
        && let Key::Char(c) = node.key
        && c.is_ascii_lowercase()
    {
        return c.to_ascii_uppercase().to_string();
    }
    format!("{node}")
}

/// Parse a key string, or `None` if it's invalid. Uppercase letters
/// normalize to shift+lowercase, matching how key events are converted
/// (the terminal reports `Z` as shift+z).
pub fn try_parse_key(key_str: &str) -> Option<Node> {
    let mut node = keymap_parser::parse(key_str).ok()?;
    if let Key::Char(c) = node.key
        && c.is_ascii_uppercase()
    {
        node.key = Key::Char(c.to_ascii_lowercase());
        node.modifiers |= Modifier::Shift as u8;
    }
    Some(node)
}

/// Parse a key string known to be valid (the built-in binding tables).
pub fn parse_key(key_str: &str) -> Node {
    try_parse_key(key_str).expect("valid key string")
}

pub fn action_label(action: AppAction) -> &'static str {
    match action {
        AppAction::Abandon => "abandon",
        AppAction::Absorb => "absorb",
        AppAction::Commit => "commit",
        AppAction::CommitWithMessage => "commit",
        AppAction::Describe => "describe",
        AppAction::DescribeInEditor | AppAction::Diffedit => "describe",
        AppAction::Edit => "edit",
        AppAction::New | AppAction::NewInsertAfter | AppAction::NewInsertBefore => "new",
        AppAction::Squash
        | AppAction::SquashInto
        | AppAction::SquashOnto
        | AppAction::SquashAfter
        | AppAction::SquashBefore => "squash",
        AppAction::RebaseRevision | AppAction::RebaseSource | AppAction::RebaseBranch => "rebase",
        AppAction::Restore | AppAction::RestoreFrom | AppAction::RestoreInto => "restore",
        AppAction::Split
        | AppAction::SplitOnto
        | AppAction::SplitAfter
        | AppAction::SplitBefore => "split",
        AppAction::BookmarkCreate
        | AppAction::BookmarkSet
        | AppAction::BookmarkDelete
        | AppAction::BookmarkForget
        | AppAction::BookmarkMove
        | AppAction::BookmarkRename
        | AppAction::BookmarkAdvance
        | AppAction::BookmarkTrack
        | AppAction::BookmarkUntrack => "bookmark",
        AppAction::Undo => "undo",
        AppAction::Redo => "redo",
        AppAction::GitFetch
        | AppAction::GitFetchAllRemotes
        | AppAction::GitPush
        | AppAction::GitPushAll
        | AppAction::GitPushChange
        | AppAction::GitPushBookmark
        | AppAction::GitExport
        | AppAction::GitImport => "git",
        AppAction::TagSet
        | AppAction::TagDelete
        | AppAction::TagViewDelete
        | AppAction::TagViewSet
        | AppAction::TagViewJumpToCommit
        | AppAction::TagViewEdit => "tag",
        AppAction::Duplicate | AppAction::DuplicateOnto => "duplicate",
        AppAction::Parallelize => "parallelize",
        AppAction::SimplifyParents => "simplify-parents",
        AppAction::Revert => "revert",
        AppAction::ArrangeUp | AppAction::ArrangeDown => "arrange",
        AppAction::ExpandAncestors | AppAction::ExpandDescendants => "expand",
        AppAction::Fix => "fix",
        AppAction::Run => "run",
        AppAction::FileUntrack => "untrack",
        AppAction::ResolveOurs | AppAction::ResolveTheirs | AppAction::ResolveMergeTool => {
            "resolve"
        }
        AppAction::ConflictPickOurs
        | AppAction::ConflictPickTheirs
        | AppAction::ConflictPickBase
        | AppAction::ConflictUnpick => "pick conflict side",
        AppAction::ConflictApplyPicks => "apply picks",
        AppAction::ConflictEditFile | AppAction::ConflictEditHunk => "edit conflict",
        AppAction::Interdiff | AppAction::EvoLogInterdiff => "interdiff",
        AppAction::FileAnnotate
        | AppAction::AnnotateGoToCommit
        | AppAction::AnnotateTimeTravel
        | AppAction::AnnotateForward
        | AppAction::ToggleAnnotateSeparator => "annotate",
        AppAction::EditFileWorkingCopy
        | AppAction::EditFileAtRevision
        | AppAction::CheckoutAndEditFile => "edit",
        AppAction::SelectPreset => "preset",
        AppAction::EditRevset | AppAction::EditRevsetInEditor => "revset",
        AppAction::RepeatLast => "repeat",
        // Anything without a friendlier name reads as its stable id, which
        // is always meaningful — a literal "action" is not, and left the
        // conflict-pick group announcing itself as "action does not support
        // 3 commits" until someone noticed.
        other => action_id_name(other),
    }
}

/// Stable snake_case identifier for an action — the name Lua plugins bind
/// and hook on (derived from the variant name via strum).
pub fn action_id_name(action: AppAction) -> &'static str {
    action.into()
}

pub fn toggle_hint(action: AppAction) -> Option<&'static str> {
    let flag = match action {
        AppAction::ToggleIgnoreImmutable => CommandFlags::IGNORE_IMMUTABLE,
        AppAction::ToggleIgnoreWorkingCopy => CommandFlags::IGNORE_WORKING_COPY,
        AppAction::ToggleDebug => CommandFlags::DEBUG,
        _ => return None,
    };
    GLOBAL_TOGGLES
        .iter()
        .find(|t| t.flag == flag)
        .map(|t| t.hint)
}

#[cfg(test)]
mod selection_kind_set_tests {
    use super::SelectionKindSet;

    #[test]
    fn nothing_selected_rules_nothing_out() {
        assert!(!SelectionKindSet::empty().blocked_by(SelectionKindSet::COMMIT));
        assert!(!SelectionKindSet::empty().blocked_by(SelectionKindSet::empty()));
    }

    #[test]
    fn every_selected_kind_has_to_be_supported() {
        let file_and_line = SelectionKindSet::FILE | SelectionKindSet::LINE;
        assert!(!file_and_line.blocked_by(SelectionKindSet::ALL));
        // Supporting half of a mixed selection is not enough.
        assert!(file_and_line.blocked_by(SelectionKindSet::FILE));
        assert!(SelectionKindSet::FILE.blocked_by(SelectionKindSet::COMMIT));
    }
}

#[cfg(test)]
mod action_label_tests {
    use super::{AppAction, SelectionKindSet, action_id_name, action_label};
    use strum::IntoEnumIterator as _;

    /// A refusal names the action. Anything that can be selection-blocked
    /// reaches this message, and a placeholder there tells the user nothing.
    #[test]
    fn every_blockable_action_names_itself() {
        for action in AppAction::iter() {
            if action.meta().selection_support == SelectionKindSet::ALL {
                continue;
            }
            let label = action_label(action);
            assert!(!label.is_empty(), "{action:?} has an empty label");
            assert_ne!(label, "action", "{action:?} falls back to a placeholder");
        }
    }

    /// The conflict picks were the group that reached the placeholder. They
    /// should read as something written for a person — asserted as "not the
    /// id" rather than by quoting the labels, which would only restate them.
    #[test]
    fn the_conflict_actions_carry_human_labels() {
        for action in [
            AppAction::ConflictPickOurs,
            AppAction::ConflictPickTheirs,
            AppAction::ConflictPickBase,
            AppAction::ConflictUnpick,
            AppAction::ConflictApplyPicks,
            AppAction::ConflictEditFile,
            AppAction::ConflictEditHunk,
        ] {
            assert_ne!(
                action_label(action),
                action_id_name(action),
                "{action:?} has no label of its own"
            );
        }
    }
}
