mod bindings;
mod help;
mod registry;
mod trie;

use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use bindings::{default_bindings, BindTarget, BindingSpec, Scope};
pub use help::{help_entries, select_mode_help_entries, HelpEntry, HelpGroup};
pub use registry::{ActionId, ActionRegistry};
pub use trie::{Keymap, Keymaps, LookupResult, TrieNode};

use crate::app::GLOBAL_TOGGLES;
use crate::types::SquashKind;

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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    Abandon,
    Absorb,
    Commit,
    CommitWithMessage,
    Describe,
    DescribeInEditor,
    Edit,
    New,
    NewInsertAfter,
    NewInsertBefore,
    Squash,
    SquashSelect(SquashKind),
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
    Fix,
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
    SwitchPreset(usize),
    SwitchToDagView,
    SwitchToBookmarkView,
    SwitchToTagView,
    BmViewDelete,
    BmViewTrack,
    BmViewUntrack,
    BmViewPush,
    BmViewJumpToCommit,
    BmViewEdit,
    BmViewRename,
    BmViewMove,
    BmViewForget,
    BmViewSet,
    BmViewFetch,
    TgViewDelete,
    TgViewSet,
    TgViewJumpToCommit,
    SwitchToOpLogView,
    SwitchToWorkspaceView,
    SwitchToEvoLogView,
    SwitchToCommandLogView,
    Jump,
    WsViewForget,
    WsViewJumpToCommit,
    EvoLogRestore,
    EvoLogEdit,
    EvoLogNew,
    Interdiff,
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
    if node.modifiers == Modifier::Shift as u8 {
        if let Key::Char(c) = node.key {
            if c.is_ascii_lowercase() {
                return c.to_ascii_uppercase().to_string();
            }
        }
    }
    format!("{node}")
}

pub fn parse_key(key_str: &str) -> Node {
    keymap_parser::parse(key_str).expect("valid key string")
}

pub fn action_label(action: AppAction) -> &'static str {
    match action {
        AppAction::Abandon => "abandon",
        AppAction::Absorb => "absorb",
        AppAction::Commit => "commit",
        AppAction::CommitWithMessage => "commit",
        AppAction::Describe => "describe",
        AppAction::DescribeInEditor => "describe",
        AppAction::Edit => "edit",
        AppAction::New | AppAction::NewInsertAfter | AppAction::NewInsertBefore => "new",
        AppAction::Squash | AppAction::SquashSelect(_) => "squash",
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
        | AppAction::TgViewDelete
        | AppAction::TgViewSet
        | AppAction::TgViewJumpToCommit => "tag",
        AppAction::Duplicate | AppAction::DuplicateOnto => "duplicate",
        AppAction::Parallelize => "parallelize",
        AppAction::SimplifyParents => "simplify-parents",
        AppAction::Revert => "revert",
        AppAction::ExpandAncestors => "expand",
        AppAction::Fix => "fix",
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
        _ => "action",
    }
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
