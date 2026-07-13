mod bindings;
mod help;
mod registry;
mod trie;

use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use bindings::{BindTarget, BindingSpec, Scope, default_bindings};
pub use help::{HelpEntry, HelpGroup, help_entries, select_mode_help_entries};
pub use registry::{ActionId, ActionRegistry};
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
            | AppAction::ResolveOurs
            | AppAction::ResolveTheirs => true,

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
            | AppAction::ConflictPickOurs
            | AppAction::ConflictPickTheirs
            | AppAction::ConflictPickBase
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
        _ => "action",
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
