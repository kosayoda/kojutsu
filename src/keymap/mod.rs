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

use crate::types::{SquashKind, GLOBAL_TOGGLES};

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
    ExpandDescendants,
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
    Arrange(crate::types::ArrangeDirection),
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
    SwitchPreset(usize),
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
    TagViewDelete,
    TagViewSet,
    TagViewJumpToCommit,
    SwitchToOpLogView,
    SwitchToWorkspaceView,
    SwitchToEvoLogView,
    SwitchToCommandLogView,
    Jump,
    WorkspaceViewForget,
    WorkspaceViewJumpToCommit,
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
    RepeatLast,
}

impl AppAction {
    pub fn is_repeatable(self) -> bool {
        match self {
            AppAction::Arrange(_)
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
            | AppAction::Edit
            | AppAction::New
            | AppAction::NewInsertAfter
            | AppAction::NewInsertBefore
            | AppAction::Squash
            | AppAction::SquashSelect(_)
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
            | AppAction::SwitchPreset(_)
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
            | AppAction::TagViewDelete
            | AppAction::TagViewSet
            | AppAction::TagViewJumpToCommit
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
            | AppAction::Edit
            | AppAction::New
            | AppAction::NewInsertAfter
            | AppAction::NewInsertBefore
            | AppAction::Squash
            | AppAction::SquashSelect(_)
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
            | AppAction::Arrange(_)
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
            | AppAction::TagViewDelete
            | AppAction::TagViewSet
            | AppAction::EvoLogRestore
            | AppAction::EvoLogEdit
            | AppAction::EvoLogNew
            | AppAction::OpLogRestore
            | AppAction::OpLogRevert
            | AppAction::OpLogAbandon
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
            | AppAction::SwitchPreset(_)
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
        | AppAction::TagViewDelete
        | AppAction::TagViewSet
        | AppAction::TagViewJumpToCommit => "tag",
        AppAction::Duplicate | AppAction::DuplicateOnto => "duplicate",
        AppAction::Parallelize => "parallelize",
        AppAction::SimplifyParents => "simplify-parents",
        AppAction::Revert => "revert",
        AppAction::Arrange(_) => "arrange",
        AppAction::ExpandAncestors | AppAction::ExpandDescendants => "expand",
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
        AppAction::RepeatLast => "repeat",
        _ => "action",
    }
}

pub fn action_id_name(action: AppAction) -> &'static str {
    match action {
        AppAction::Quit => "quit",
        AppAction::MoveDown => "move_down",
        AppAction::MoveUp => "move_up",
        AppAction::MoveDownSection => "move_down_section",
        AppAction::MoveUpSection => "move_up_section",
        AppAction::PageDown => "page_down",
        AppAction::PageUp => "page_up",
        AppAction::JumpToWorkingCopy => "jump_to_working_copy",
        AppAction::MoveToTop => "move_to_top",
        AppAction::MoveToBottom => "move_to_bottom",
        AppAction::MoveToScreenTop => "move_to_screen_top",
        AppAction::MoveToScreenMiddle => "move_to_screen_middle",
        AppAction::MoveToScreenBottom => "move_to_screen_bottom",
        AppAction::ScrollLeft => "scroll_left",
        AppAction::ScrollRight => "scroll_right",
        AppAction::ToggleFold => "toggle_fold",
        AppAction::Refresh => "refresh",
        AppAction::ExpandAncestors => "expand_ancestors",
        AppAction::ExpandDescendants => "expand_descendants",
        AppAction::Abandon => "abandon",
        AppAction::Absorb => "absorb",
        AppAction::Commit => "commit",
        AppAction::CommitWithMessage => "commit_with_message",
        AppAction::Describe => "describe",
        AppAction::DescribeInEditor => "describe_in_editor",
        AppAction::Edit => "edit",
        AppAction::New => "new",
        AppAction::NewInsertAfter => "new_insert_after",
        AppAction::NewInsertBefore => "new_insert_before",
        AppAction::Squash => "squash",
        AppAction::SquashSelect(SquashKind::Into) => "squash_into",
        AppAction::SquashSelect(SquashKind::Onto) => "squash_onto",
        AppAction::SquashSelect(SquashKind::After) => "squash_after",
        AppAction::SquashSelect(SquashKind::Before) => "squash_before",
        AppAction::RebaseRevision => "rebase_revision",
        AppAction::RebaseSource => "rebase_source",
        AppAction::RebaseBranch => "rebase_branch",
        AppAction::Restore => "restore",
        AppAction::RestoreFrom => "restore_from",
        AppAction::RestoreInto => "restore_into",
        AppAction::Split => "split",
        AppAction::SplitOnto => "split_onto",
        AppAction::SplitAfter => "split_after",
        AppAction::SplitBefore => "split_before",
        AppAction::EditRevset => "edit_revset",
        AppAction::EditRevsetInEditor => "edit_revset_in_editor",
        AppAction::ResetRevset => "reset_revset",
        AppAction::BookmarkCreate => "bookmark_create",
        AppAction::BookmarkSet => "bookmark_set",
        AppAction::BookmarkDelete => "bookmark_delete",
        AppAction::BookmarkForget => "bookmark_forget",
        AppAction::BookmarkMove => "bookmark_move",
        AppAction::BookmarkRename => "bookmark_rename",
        AppAction::BookmarkAdvance => "bookmark_advance",
        AppAction::BookmarkTrack => "bookmark_track",
        AppAction::BookmarkUntrack => "bookmark_untrack",
        AppAction::ShowHelp => "show_help",
        AppAction::Undo => "undo",
        AppAction::Redo => "redo",
        AppAction::GitFetch => "git_fetch",
        AppAction::GitFetchAllRemotes => "git_fetch_all_remotes",
        AppAction::GitPush => "git_push",
        AppAction::GitPushAll => "git_push_all",
        AppAction::GitPushChange => "git_push_change",
        AppAction::GitPushBookmark => "git_push_bookmark",
        AppAction::GitExport => "git_export",
        AppAction::GitImport => "git_import",
        AppAction::Duplicate => "duplicate",
        AppAction::DuplicateOnto => "duplicate_onto",
        AppAction::Parallelize => "parallelize",
        AppAction::SimplifyParents => "simplify_parents",
        AppAction::Revert => "revert",
        AppAction::Arrange(crate::types::ArrangeDirection::Up) => "arrange_up",
        AppAction::Arrange(crate::types::ArrangeDirection::Down) => "arrange_down",
        AppAction::Fix => "fix",
        AppAction::FileUntrack => "file_untrack",
        AppAction::ResolveOurs => "resolve_ours",
        AppAction::ResolveTheirs => "resolve_theirs",
        AppAction::ResolveMergeTool => "resolve_merge_tool",
        AppAction::ConflictPickOurs => "conflict_pick_ours",
        AppAction::ConflictPickTheirs => "conflict_pick_theirs",
        AppAction::ConflictPickBase => "conflict_pick_base",
        AppAction::ToggleIgnoreImmutable => "toggle_ignore_immutable",
        AppAction::ToggleIgnoreWorkingCopy => "toggle_ignore_working_copy",
        AppAction::ToggleDebug => "toggle_debug",
        AppAction::ToggleGitDiff => "toggle_git_diff",
        AppAction::ToggleLineNumbers => "toggle_line_numbers",
        AppAction::ToggleDiffUnderline => "toggle_diff_underline",
        AppAction::WorkspaceAdd => "workspace_add",
        AppAction::WorkspaceForget => "workspace_forget",
        AppAction::WorkspaceList => "workspace_list",
        AppAction::WorkspaceRename => "workspace_rename",
        AppAction::ToggleSelect => "toggle_select",
        AppAction::EnterVisualMode => "enter_visual_mode",
        AppAction::StartSearch => "start_search",
        AppAction::NextMatch => "next_match",
        AppAction::PrevMatch => "prev_match",
        AppAction::TagSet => "tag_set",
        AppAction::TagDelete => "tag_delete",
        AppAction::SelectPreset => "select_preset",
        AppAction::SwitchPreset(n) => match n {
            0 => "switch_preset_1",
            1 => "switch_preset_2",
            2 => "switch_preset_3",
            3 => "switch_preset_4",
            _ => "switch_preset",
        },
        AppAction::SwitchToDagView => "switch_to_dag_view",
        AppAction::SwitchToBookmarkView => "switch_to_bookmark_view",
        AppAction::SwitchToTagView => "switch_to_tag_view",
        AppAction::SwitchToOpLogView => "switch_to_op_log_view",
        AppAction::SwitchToWorkspaceView => "switch_to_workspace_view",
        AppAction::SwitchToEvoLogView => "switch_to_evolog_view",
        AppAction::SwitchToCommandLogView => "switch_to_command_log_view",
        AppAction::BookmarkViewDelete => "bookmark_view_delete",
        AppAction::BookmarkViewTrack => "bookmark_view_track",
        AppAction::BookmarkViewUntrack => "bookmark_view_untrack",
        AppAction::BookmarkViewPush => "bookmark_view_push",
        AppAction::BookmarkViewJumpToCommit => "bookmark_view_jump_to_commit",
        AppAction::BookmarkViewEdit => "bookmark_view_edit",
        AppAction::BookmarkViewRename => "bookmark_view_rename",
        AppAction::BookmarkViewMove => "bookmark_view_move",
        AppAction::BookmarkViewForget => "bookmark_view_forget",
        AppAction::BookmarkViewSet => "bookmark_view_set",
        AppAction::BookmarkViewFetchDefault => "bookmark_view_fetch_default",
        AppAction::BookmarkViewFetchBookmark => "bookmark_view_fetch_bookmark",
        AppAction::BookmarkViewFetchAllRemotes => "bookmark_view_fetch_all_remotes",
        AppAction::TagViewDelete => "tag_view_delete",
        AppAction::TagViewSet => "tag_view_set",
        AppAction::TagViewJumpToCommit => "tag_view_jump_to_commit",
        AppAction::Jump => "jump",
        AppAction::WorkspaceViewForget => "workspace_view_forget",
        AppAction::WorkspaceViewJumpToCommit => "workspace_view_jump_to_commit",
        AppAction::EvoLogRestore => "evolog_restore",
        AppAction::EvoLogEdit => "evolog_edit",
        AppAction::EvoLogNew => "evolog_new",
        AppAction::Interdiff => "interdiff",
        AppAction::EvoLogInterdiff => "evolog_interdiff",
        AppAction::FileAnnotate => "file_annotate",
        AppAction::AnnotateGoToCommit => "annotate_go_to_commit",
        AppAction::AnnotateTimeTravel => "annotate_time_travel",
        AppAction::AnnotateForward => "annotate_forward",
        AppAction::ToggleAnnotateSeparator => "toggle_annotate_separator",
        AppAction::EditFileWorkingCopy => "edit_file_working_copy",
        AppAction::EditFileAtRevision => "edit_file_at_revision",
        AppAction::CheckoutAndEditFile => "checkout_and_edit_file",
        AppAction::OpLogRestore => "op_log_restore",
        AppAction::OpLogRevert => "op_log_revert",
        AppAction::OpLogAbandon => "op_log_abandon",
        AppAction::OpLogFilterWorkspace => "op_log_filter_workspace",
        AppAction::CommandMode => "command_mode",
        AppAction::FileList => "file_list",
        AppAction::RepeatLast => "repeat_last",
    }
}

pub const ALL_ACTIONS: &[AppAction] = &[
    AppAction::Quit,
    AppAction::MoveDown,
    AppAction::MoveUp,
    AppAction::MoveDownSection,
    AppAction::MoveUpSection,
    AppAction::PageDown,
    AppAction::PageUp,
    AppAction::JumpToWorkingCopy,
    AppAction::MoveToTop,
    AppAction::MoveToBottom,
    AppAction::MoveToScreenTop,
    AppAction::MoveToScreenMiddle,
    AppAction::MoveToScreenBottom,
    AppAction::ScrollLeft,
    AppAction::ScrollRight,
    AppAction::ToggleFold,
    AppAction::Refresh,
    AppAction::ExpandAncestors,
    AppAction::ExpandDescendants,
    AppAction::Abandon,
    AppAction::Absorb,
    AppAction::Commit,
    AppAction::CommitWithMessage,
    AppAction::Describe,
    AppAction::DescribeInEditor,
    AppAction::Edit,
    AppAction::New,
    AppAction::NewInsertAfter,
    AppAction::NewInsertBefore,
    AppAction::Squash,
    AppAction::SquashSelect(SquashKind::Into),
    AppAction::SquashSelect(SquashKind::Onto),
    AppAction::SquashSelect(SquashKind::After),
    AppAction::SquashSelect(SquashKind::Before),
    AppAction::RebaseRevision,
    AppAction::RebaseSource,
    AppAction::RebaseBranch,
    AppAction::Restore,
    AppAction::RestoreFrom,
    AppAction::RestoreInto,
    AppAction::Split,
    AppAction::SplitOnto,
    AppAction::SplitAfter,
    AppAction::SplitBefore,
    AppAction::EditRevset,
    AppAction::EditRevsetInEditor,
    AppAction::ResetRevset,
    AppAction::BookmarkCreate,
    AppAction::BookmarkSet,
    AppAction::BookmarkDelete,
    AppAction::BookmarkForget,
    AppAction::BookmarkMove,
    AppAction::BookmarkRename,
    AppAction::BookmarkAdvance,
    AppAction::BookmarkTrack,
    AppAction::BookmarkUntrack,
    AppAction::ShowHelp,
    AppAction::Undo,
    AppAction::Redo,
    AppAction::GitFetch,
    AppAction::GitFetchAllRemotes,
    AppAction::GitPush,
    AppAction::GitPushAll,
    AppAction::GitPushChange,
    AppAction::GitPushBookmark,
    AppAction::GitExport,
    AppAction::GitImport,
    AppAction::Duplicate,
    AppAction::DuplicateOnto,
    AppAction::Parallelize,
    AppAction::SimplifyParents,
    AppAction::Revert,
    AppAction::Arrange(crate::types::ArrangeDirection::Up),
    AppAction::Arrange(crate::types::ArrangeDirection::Down),
    AppAction::RepeatLast,
    AppAction::Fix,
    AppAction::FileUntrack,
    AppAction::ResolveOurs,
    AppAction::ResolveTheirs,
    AppAction::ResolveMergeTool,
    AppAction::ConflictPickOurs,
    AppAction::ConflictPickTheirs,
    AppAction::ConflictPickBase,
    AppAction::ToggleIgnoreImmutable,
    AppAction::ToggleIgnoreWorkingCopy,
    AppAction::ToggleDebug,
    AppAction::ToggleGitDiff,
    AppAction::ToggleLineNumbers,
    AppAction::ToggleDiffUnderline,
    AppAction::WorkspaceAdd,
    AppAction::WorkspaceForget,
    AppAction::WorkspaceList,
    AppAction::WorkspaceRename,
    AppAction::ToggleSelect,
    AppAction::EnterVisualMode,
    AppAction::StartSearch,
    AppAction::NextMatch,
    AppAction::PrevMatch,
    AppAction::TagSet,
    AppAction::TagDelete,
    AppAction::SelectPreset,
    AppAction::SwitchPreset(0),
    AppAction::SwitchPreset(1),
    AppAction::SwitchPreset(2),
    AppAction::SwitchPreset(3),
    AppAction::SwitchPreset(4),
    AppAction::SwitchToDagView,
    AppAction::SwitchToBookmarkView,
    AppAction::SwitchToTagView,
    AppAction::SwitchToOpLogView,
    AppAction::SwitchToWorkspaceView,
    AppAction::SwitchToEvoLogView,
    AppAction::SwitchToCommandLogView,
    AppAction::BookmarkViewDelete,
    AppAction::BookmarkViewTrack,
    AppAction::BookmarkViewUntrack,
    AppAction::BookmarkViewPush,
    AppAction::BookmarkViewJumpToCommit,
    AppAction::BookmarkViewEdit,
    AppAction::BookmarkViewRename,
    AppAction::BookmarkViewMove,
    AppAction::BookmarkViewForget,
    AppAction::BookmarkViewSet,
    AppAction::BookmarkViewFetchDefault,
    AppAction::BookmarkViewFetchBookmark,
    AppAction::BookmarkViewFetchAllRemotes,
    AppAction::TagViewDelete,
    AppAction::TagViewSet,
    AppAction::TagViewJumpToCommit,
    AppAction::Jump,
    AppAction::WorkspaceViewForget,
    AppAction::WorkspaceViewJumpToCommit,
    AppAction::EvoLogRestore,
    AppAction::EvoLogEdit,
    AppAction::EvoLogNew,
    AppAction::Interdiff,
    AppAction::EvoLogInterdiff,
    AppAction::FileAnnotate,
    AppAction::AnnotateGoToCommit,
    AppAction::AnnotateTimeTravel,
    AppAction::AnnotateForward,
    AppAction::ToggleAnnotateSeparator,
    AppAction::EditFileWorkingCopy,
    AppAction::EditFileAtRevision,
    AppAction::CheckoutAndEditFile,
    AppAction::OpLogRestore,
    AppAction::OpLogRevert,
    AppAction::OpLogAbandon,
    AppAction::OpLogFilterWorkspace,
    AppAction::CommandMode,
    AppAction::FileList,
];

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
