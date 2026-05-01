use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    app::GLOBAL_TOGGLES,
    types::{SelectionKind, SquashKind},
};

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

// ---------------------------------------------------------------------------
// CommandFlags -- toggleable flags that modify command behavior.
// ---------------------------------------------------------------------------

/// Label used for the conflict prefix submenu.
pub const CONFLICT_PREFIX: &str = "conflict";

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
        const GIT_DIFF            = 1 << 11;
    }
}

// ---------------------------------------------------------------------------
// AppAction -- every action the application supports.
// Flag-specific variants are gone; flags are toggled separately.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    // Bookmark view actions (operate on selected BookmarkViewEntry)
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
    // Tag view actions (operate on selected TagViewEntry)
    TgViewDelete,
    TgViewSet,
    TgViewJumpToCommit,
    // View switching
    SwitchToOpLogView,
    SwitchToWorkspaceView,
    SwitchToEvoLogView,
    SwitchToCommandLogView,
    // Workspace view actions
    WsViewForget,
    WsViewJumpToCommit,
    // Evolog view actions
    EvoLogRestore,
    EvoLogEdit,
    EvoLogNew,
    // Op log view actions
    OpLogRestore,
    OpLogRevert,
    OpLogAbandon,
    OpLogFilterWorkspace,
}

// ---------------------------------------------------------------------------
// Trie types
// ---------------------------------------------------------------------------

/// A node in the keymap trie.
pub enum KeymapNode {
    /// Leaf: this key (or key sequence) triggers an action.
    Action {
        action: AppAction,
        description: &'static str,
        group: HelpGroup,
    },
    /// Branch: this key opens a submenu with further options.
    Prefix {
        label: &'static str,
        group: HelpGroup,
        children: Vec<(Node, KeymapNode)>,
    },
    /// Toggle: this key flips a command flag and stays in the submenu.
    Toggle {
        flag: CommandFlags,
        description: &'static str,
    },
}

/// The top-level keymap: a list of (key, node) pairs forming the root of the trie.
pub struct Keymap {
    pub root: Vec<(Node, KeymapNode)>,
}

/// Result of looking up a key in the keymap.
pub enum LookupResult<'a> {
    /// Resolved to a concrete action.
    Action(AppAction),
    /// Entered a prefix node -- a submenu is available.
    Prefix {
        label: &'a str,
        children: &'a [(Node, KeymapNode)],
    },
    /// Toggled a command flag.
    Toggle(CommandFlags),
    /// Key not bound.
    Unbound,
}

impl Keymap {
    /// Look up a key at the root level.
    pub fn lookup(&self, key: &Node) -> LookupResult<'_> {
        Self::lookup_in(&self.root, key)
    }

    /// Look up a key within a set of children (for the second key of a sequence).
    pub fn lookup_in<'a>(entries: &'a [(Node, KeymapNode)], key: &Node) -> LookupResult<'a> {
        for (k, node) in entries {
            if k == key {
                return match node {
                    KeymapNode::Action { action, .. } => LookupResult::Action(*action),
                    KeymapNode::Prefix {
                        label, children, ..
                    } => LookupResult::Prefix { label, children },
                    KeymapNode::Toggle { flag, .. } => LookupResult::Toggle(*flag),
                };
            }
        }
        LookupResult::Unbound
    }
}

/// Holds per-view keymaps.
pub struct Keymaps {
    pub dag: Keymap,
    pub bookmarks: Keymap,
    pub tags: Keymap,
    pub operations: Keymap,
    pub workspaces: Keymap,
    pub evolog: Keymap,
    pub command_log: Keymap,
}

impl Keymaps {
    pub fn for_view(&self, view: crate::app::ActiveView) -> &Keymap {
        match view {
            crate::app::ActiveView::Dag => &self.dag,
            crate::app::ActiveView::Bookmarks => &self.bookmarks,
            crate::app::ActiveView::Tags => &self.tags,
            crate::app::ActiveView::Operations => &self.operations,
            crate::app::ActiveView::Evolog => &self.evolog,
            crate::app::ActiveView::Workspaces => &self.workspaces,
            crate::app::ActiveView::CommandLog => &self.command_log,
        }
    }
}

// ---------------------------------------------------------------------------
// Default keymap construction
// ---------------------------------------------------------------------------

/// Bindings shared across all views (navigation, search, quit, help, etc.).
fn shared_bindings() -> Vec<(Node, KeymapNode)> {
    use HelpGroup::{General as G, Navigation as N};
    vec![
        // Help
        bind("?", AppAction::ShowHelp, "help", G),
        // Quit
        bind("q", AppAction::Quit, "quit", G),
        bind("ctrl-c", AppAction::Quit, "quit", G),
        // Line-by-line navigation
        bind("j", AppAction::MoveDown, "move down", N),
        bind("down", AppAction::MoveDown, "move down", N),
        bind("k", AppAction::MoveUp, "move up", N),
        bind("up", AppAction::MoveUp, "move up", N),
        // Section navigation
        bind("shift-j", AppAction::MoveDownSection, "next commit", N),
        bind("shift-k", AppAction::MoveUpSection, "prev commit", N),
        // Paging
        bind("ctrl-d", AppAction::PageDown, "page down", N),
        bind("pagedown", AppAction::PageDown, "page down", N),
        bind("ctrl-u", AppAction::PageUp, "page up", N),
        bind("pageup", AppAction::PageUp, "page up", N),
        // Horizontal scroll
        bind("left", AppAction::ScrollLeft, "scroll left", N),
        bind("right", AppAction::ScrollRight, "scroll right", N),
        bind("h", AppAction::ScrollLeft, "scroll left", N),
        bind("l", AppAction::ScrollRight, "scroll right", N),
        // Jump
        bind("@", AppAction::JumpToWorkingCopy, "jump to @", N),
        bind("0", AppAction::MoveToTop, "go to top", N),
        bind("$", AppAction::MoveToBottom, "go to bottom", N),
        // View switching
        bind("1", AppAction::SwitchToDagView, "DAG view", G),
        bind("2", AppAction::SwitchToBookmarkView, "bookmarks view", G),
        bind("3", AppAction::SwitchToTagView, "tags view", G),
        bind("4", AppAction::SwitchToWorkspaceView, "workspaces view", G),
        bind("5", AppAction::SwitchToOpLogView, "operations view", G),
        bind("6", AppAction::SwitchToEvoLogView, "evolog view", G),
        bind("7", AppAction::SwitchToCommandLogView, "command log", G),
        // Fold / Select
        bind("tab", AppAction::ToggleFold, "toggle fold", N),
        bind("space", AppAction::ToggleSelect, "toggle select", N),
        bind("v", AppAction::EnterVisualMode, "visual select", N),
        bind("/", AppAction::StartSearch, "search", N),
        bind("ctrl-n", AppAction::NextMatch, "next match", N),
        bind("ctrl-p", AppAction::PrevMatch, "prev match", N),
        // Global toggles
        bind(
            "shift-i",
            AppAction::ToggleIgnoreImmutable,
            "toggle ignore-immutable",
            G,
        ),
        bind(
            "shift-w",
            AppAction::ToggleIgnoreWorkingCopy,
            "toggle ignore-working-copy",
            G,
        ),
        bind("shift-d", AppAction::ToggleDebug, "toggle debug", G),
        // Refresh
        bind("ctrl-r", AppAction::Refresh, "refresh", N),
        // Command palette
        prefix(
            ";",
            "command",
            G,
            vec![
                bind("r", AppAction::EditRevset, "edit revset", G),
                bind(
                    "shift-r",
                    AppAction::EditRevsetInEditor,
                    "edit revset in $EDITOR",
                    G,
                ),
                bind("d", AppAction::ResetRevset, "default revset", G),
                bind("p", AppAction::SelectPreset, "switch preset", G),
                bind("l", AppAction::ToggleLineNumbers, "toggle line numbers", G),
                bind("g", AppAction::ToggleGitDiff, "toggle diff style", G),
                bind("1", AppAction::SwitchPreset(0), "preset 1", G),
                bind("2", AppAction::SwitchPreset(1), "preset 2", G),
                bind("3", AppAction::SwitchPreset(2), "preset 3", G),
                bind("4", AppAction::SwitchPreset(3), "preset 4", G),
                bind("5", AppAction::SwitchPreset(4), "preset 5", G),
            ],
        ),
    ]
}

impl Default for Keymaps {
    fn default() -> Self {
        use HelpGroup::Commands as C;

        // DAG view: shared bindings + all commit/graph operations.
        let mut dag_root = shared_bindings();
        dag_root.extend(vec![
            bind("shift-e", AppAction::SwitchToEvoLogView, "evolog", C),
            bind("a", AppAction::Absorb, "absorb", C),
            bind("+", AppAction::ExpandAncestors, "expand ancestors", C),
            prefix(
                "shift-c",
                CONFLICT_PREFIX,
                C,
                vec![
                    bind("o", AppAction::ResolveOurs, "take ours", C),
                    bind("t", AppAction::ResolveTheirs, "take theirs", C),
                    bind("b", AppAction::ConflictPickBase, "take base", C),
                    bind("m", AppAction::ResolveMergeTool, "merge tool", C),
                ],
            ),
            bind("f", AppAction::Fix, "fix", C),
            prefix(
                "shift-f",
                "file",
                C,
                vec![bind("u", AppAction::FileUntrack, "untrack", C)],
            ),
            prefix(
                "b",
                "bookmark",
                C,
                vec![
                    toggle("shift-b", CommandFlags::ALLOW_BACKWARDS, "allow backwards"),
                    bind("c", AppAction::BookmarkCreate, "create", C),
                    bind("s", AppAction::BookmarkSet, "set", C),
                    bind("d", AppAction::BookmarkDelete, "delete", C),
                    bind("f", AppAction::BookmarkForget, "forget", C),
                    bind("m", AppAction::BookmarkMove, "move…", C),
                    bind("r", AppAction::BookmarkRename, "rename", C),
                    bind("a", AppAction::BookmarkAdvance, "advance", C),
                    bind("t", AppAction::BookmarkTrack, "track", C),
                    bind("u", AppAction::BookmarkUntrack, "untrack", C),
                ],
            ),
            prefix(
                "t",
                "tag",
                C,
                vec![
                    toggle("shift-b", CommandFlags::ALLOW_BACKWARDS, "allow backwards"),
                    bind("s", AppAction::TagSet, "set", C),
                    bind("d", AppAction::TagDelete, "delete", C),
                ],
            ),
            prefix(
                "c",
                "commit",
                C,
                vec![
                    toggle("i", CommandFlags::INTERACTIVE, "interactive"),
                    bind("c", AppAction::Commit, "commit (in $EDITOR)", C),
                    bind("m", AppAction::CommitWithMessage, "with message", C),
                ],
            ),
            prefix(
                "d",
                "describe",
                C,
                vec![
                    bind("d", AppAction::Describe, "describe", C),
                    bind("shift-d", AppAction::DescribeInEditor, "in $EDITOR", C),
                ],
            ),
            bind("e", AppAction::Edit, "edit", C),
            prefix(
                "g",
                "git",
                C,
                vec![
                    toggle("d", CommandFlags::DRY_RUN, "dry run (push only)"),
                    prefix(
                        "f",
                        "fetch",
                        C,
                        vec![
                            bind("f", AppAction::GitFetch, "fetch", C),
                            bind("a", AppAction::GitFetchAllRemotes, "all remotes", C),
                        ],
                    ),
                    prefix(
                        "p",
                        "push",
                        C,
                        vec![
                            bind("p", AppAction::GitPush, "push", C),
                            bind("a", AppAction::GitPushAll, "all bookmarks", C),
                            bind("c", AppAction::GitPushChange, "change", C),
                            bind("b", AppAction::GitPushBookmark, "bookmark", C),
                        ],
                    ),
                    bind("e", AppAction::GitExport, "export (jj -> git)", C),
                    bind("i", AppAction::GitImport, "import (git -> jj)", C),
                ],
            ),
            prefix(
                "n",
                "new",
                C,
                vec![
                    toggle("e", CommandFlags::NO_EDIT, "no-edit"),
                    bind("n", AppAction::New, "new", C),
                    bind("a", AppAction::NewInsertAfter, "insert after", C),
                    bind("b", AppAction::NewInsertBefore, "insert before", C),
                ],
            ),
            prefix(
                "r",
                "rebase",
                C,
                vec![
                    bind("r", AppAction::RebaseRevision, "revision…", C),
                    bind("s", AppAction::RebaseSource, "source…", C),
                    bind("b", AppAction::RebaseBranch, "branch…", C),
                ],
            ),
            prefix(
                "shift-r",
                "restore",
                C,
                vec![
                    toggle("i", CommandFlags::INTERACTIVE, "interactive"),
                    toggle(
                        "d",
                        CommandFlags::RESTORE_DESCENDANTS,
                        "restore descendants",
                    ),
                    bind("shift-r", AppAction::Restore, "changes-in", C),
                    bind("f", AppAction::RestoreFrom, "from…", C),
                    bind("t", AppAction::RestoreInto, "into…", C),
                ],
            ),
            prefix(
                "shift-s",
                "split",
                C,
                vec![
                    toggle("i", CommandFlags::INTERACTIVE, "interactive"),
                    toggle("p", CommandFlags::PARALLEL, "parallel"),
                    bind("shift-s", AppAction::Split, "split", C),
                    bind("o", AppAction::SplitOnto, "onto…", C),
                    bind("a", AppAction::SplitAfter, "after…", C),
                    bind("b", AppAction::SplitBefore, "before…", C),
                ],
            ),
            prefix(
                "s",
                "squash",
                C,
                vec![
                    toggle("i", CommandFlags::INTERACTIVE, "interactive"),
                    toggle("k", CommandFlags::KEEP_EMPTIED, "keep emptied"),
                    bind("s", AppAction::Squash, "into parent", C),
                    bind("t", AppAction::SquashSelect(SquashKind::Into), "into…", C),
                    bind("o", AppAction::SquashSelect(SquashKind::Onto), "onto…", C),
                    bind("a", AppAction::SquashSelect(SquashKind::After), "after…", C),
                    bind(
                        "b",
                        AppAction::SquashSelect(SquashKind::Before),
                        "before…",
                        C,
                    ),
                ],
            ),
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
            prefix(
                "x",
                "abandon",
                C,
                vec![
                    toggle("b", CommandFlags::RETAIN_BOOKMARKS, "keep bookmarks"),
                    toggle(
                        "d",
                        CommandFlags::RESTORE_DESCENDANTS,
                        "restore descendants",
                    ),
                    bind("x", AppAction::Abandon, "abandon", C),
                ],
            ),
            prefix(
                "y",
                "duplicate",
                C,
                vec![
                    bind("y", AppAction::Duplicate, "duplicate", C),
                    bind("t", AppAction::DuplicateOnto, "onto…", C),
                ],
            ),
            bind("p", AppAction::Parallelize, "parallelize", C),
            bind("shift-p", AppAction::SimplifyParents, "simplify parents", C),
            bind("z", AppAction::Revert, "revert", C),
            prefix(
                "w",
                "workspace",
                C,
                vec![
                    bind("a", AppAction::WorkspaceAdd, "add", C),
                    bind("f", AppAction::WorkspaceForget, "forget", C),
                    bind("l", AppAction::WorkspaceList, "list", C),
                    bind("r", AppAction::WorkspaceRename, "rename", C),
                ],
            ),
        ]);

        // Bookmark view: shared bindings + bookmark-specific actions.
        let mut bm_root = shared_bindings();
        bm_root.extend(vec![
            bind("d", AppAction::BmViewDelete, "delete", C),
            bind("t", AppAction::BmViewTrack, "track", C),
            bind("shift-u", AppAction::BmViewUntrack, "untrack", C),
            bind("p", AppAction::BmViewPush, "push", C),
            bind("enter", AppAction::BmViewJumpToCommit, "jump / pick", C),
            bind("e", AppAction::BmViewEdit, "edit (checkout)", C),
            bind("r", AppAction::BmViewRename, "rename", C),
            bind("m", AppAction::BmViewMove, "move…", C),
            bind("f", AppAction::BmViewFetch, "fetch", C),
            bind("s", AppAction::BmViewSet, "set…", C),
            bind("shift-f", AppAction::BmViewForget, "forget", C),
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
        ]);

        // Tag view: shared bindings + tag-specific actions.
        let mut tg_root = shared_bindings();
        tg_root.extend(vec![
            bind("d", AppAction::TgViewDelete, "delete", C),
            bind("s", AppAction::TgViewSet, "set…", C),
            bind("enter", AppAction::TgViewJumpToCommit, "jump to commit", C),
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
        ]);

        // Operations view: shared bindings + op log-specific actions.
        let mut op_root = shared_bindings();
        op_root.extend(vec![
            bind(
                "ctrl-f",
                AppAction::OpLogFilterWorkspace,
                "filter workspace",
                C,
            ),
            bind("x", AppAction::OpLogAbandon, "abandon op", C),
            bind("r", AppAction::OpLogRevert, "revert op", C),
            bind("shift-r", AppAction::OpLogRestore, "restore to op", C),
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
        ]);

        // Workspaces view: shared bindings + workspace actions.
        let mut ws_root = shared_bindings();
        ws_root.extend(vec![
            bind("a", AppAction::WorkspaceAdd, "add", C),
            bind("f", AppAction::WsViewForget, "forget", C),
            bind("r", AppAction::WorkspaceRename, "rename", C),
            bind("enter", AppAction::WsViewJumpToCommit, "jump to commit", C),
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
        ]);

        let mut evolog_root = shared_bindings();
        evolog_root.extend(vec![
            bind("shift-e", AppAction::SwitchToEvoLogView, "evolog", C),
            bind("r", AppAction::EvoLogRestore, "restore from", C),
            bind("e", AppAction::EvoLogEdit, "edit (checkout)", C),
            bind("n", AppAction::EvoLogNew, "new from", C),
        ]);

        let cmd_log_root = shared_bindings();

        Keymaps {
            dag: Keymap { root: dag_root },
            bookmarks: Keymap { root: bm_root },
            tags: Keymap { root: tg_root },
            operations: Keymap { root: op_root },
            workspaces: Keymap { root: ws_root },
            evolog: Keymap { root: evolog_root },
            command_log: Keymap {
                root: cmd_log_root,
            },
        }
    }
}

/// Helper: create a (Node, KeymapNode::Action) pair from a key string.
fn bind(
    key_str: &str,
    action: AppAction,
    description: &'static str,
    group: HelpGroup,
) -> (Node, KeymapNode) {
    let node = keymap_parser::parse(key_str).expect("valid key string in default keymap");
    (
        node,
        KeymapNode::Action {
            action,
            description,
            group,
        },
    )
}

/// Helper: create a (Node, KeymapNode::Prefix) pair for a submenu.
pub fn prefix(
    key_str: &str,
    label: &'static str,
    group: HelpGroup,
    children: Vec<(Node, KeymapNode)>,
) -> (Node, KeymapNode) {
    let node = keymap_parser::parse(key_str).expect("valid key string in default keymap");
    (
        node,
        KeymapNode::Prefix {
            label,
            group,
            children,
        },
    )
}

/// Helper: create a (Node, KeymapNode::Toggle) pair for a flag toggle.
fn toggle(key_str: &str, flag: CommandFlags, description: &'static str) -> (Node, KeymapNode) {
    let node = keymap_parser::parse(key_str).expect("valid key string in default keymap");
    (node, KeymapNode::Toggle { flag, description })
}

// ---------------------------------------------------------------------------
// Crossterm KeyEvent → keymap_parser Node conversion
// ---------------------------------------------------------------------------

/// Convert a crossterm `KeyEvent` to a `keymap_parser::Node` for trie lookup.
///
/// Returns `None` for key codes we don't handle (e.g. media keys).
pub fn key_event_to_node(key: &KeyEvent) -> Option<Node> {
    let k = match key.code {
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Char(' ') => Key::Space,
        // Crossterm reports Shift+d as Char('D') + SHIFT modifier.
        // Normalize to lowercase so it matches keymap_parser's "shift-d".
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

const MODIFIER_MAP: [(KeyModifiers, Modifier); 4] = [
    (KeyModifiers::ALT, Modifier::Alt),
    (KeyModifiers::CONTROL, Modifier::Ctrl),
    (KeyModifiers::META, Modifier::Cmd),
    (KeyModifiers::SHIFT, Modifier::Shift),
];

fn convert_modifiers(mods: &KeyModifiers) -> keymap_parser::Modifiers {
    MODIFIER_MAP.iter().fold(0u8, |acc, (ct_mod, kp_mod)| {
        if mods.contains(*ct_mod) {
            acc | *kp_mod as u8
        } else {
            acc
        }
    })
}

// ---------------------------------------------------------------------------
// Display helpers for the submenu popup
// ---------------------------------------------------------------------------

/// Format a `Node` as a human-readable key string for display.
///
/// Shift + lowercase letter is shown as the uppercase letter (e.g., `J` instead
/// of `shift-j`), matching how users think about these keys.
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

// ---------------------------------------------------------------------------
// Help generation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HelpGroup {
    Commands,
    Navigation,
    General,
}

impl HelpGroup {
    pub fn label(self) -> &'static str {
        match self {
            HelpGroup::Navigation => "Navigation",
            HelpGroup::Commands => "Commands",
            HelpGroup::General => "General",
        }
    }
}

pub struct HelpEntry {
    pub keys: String,
    pub description: String,
    pub group: HelpGroup,
    pub selection_support: SelectionKindSet,
    /// Only show/activate when cursor is on a conflict row.
    pub requires_conflict: bool,
}

pub fn action_supported_selection_kinds(action: AppAction) -> &'static [SelectionKind] {
    use SelectionKind::{Commit, File, Line};
    match action {
        // File+line selection support requires explicit opt-in.
        AppAction::Absorb => &[Commit, File],
        // Operations that only make sense at commit level.
        AppAction::Abandon
        | AppAction::Describe
        | AppAction::DescribeInEditor
        | AppAction::Edit
        | AppAction::New
        | AppAction::NewInsertAfter
        | AppAction::NewInsertBefore
        | AppAction::RebaseRevision
        | AppAction::RebaseSource
        | AppAction::RebaseBranch
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
        | AppAction::ExpandAncestors
        | AppAction::Fix
        | AppAction::TagSet
        | AppAction::TagDelete => &[Commit],
        AppAction::FileUntrack
        | AppAction::ResolveOurs
        | AppAction::ResolveTheirs
        | AppAction::ResolveMergeTool => &[File],
        // Everything else (squash, restore, split, commit, etc.) supports all levels.
        _ => &[Commit, File, Line],
    }
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
        AppAction::SelectPreset => "preset",
        AppAction::EditRevset | AppAction::EditRevsetInEditor => "revset",
        _ => "action",
    }
}

pub fn selection_kind_set_for_action(action: AppAction) -> SelectionKindSet {
    action_supported_selection_kinds(action)
        .iter()
        .fold(SelectionKindSet::empty(), |acc, kind| {
            acc | kind.as_bitset()
        })
}

/// Map toggle actions to their hint character from `GLOBAL_TOGGLES`.
/// Returns `Some("I")` for `ToggleIgnoreImmutable`, etc.
fn toggle_hint(action: AppAction) -> Option<&'static str> {
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

/// Sort key: case-insensitive, but lowercase before uppercase.
fn sort_key(keys: &str) -> (String, bool) {
    let upper = keys.starts_with(|c: char| c.is_uppercase());
    (keys.to_lowercase(), upper)
}

/// Generate grouped help entries from the keymap trie.
///
/// Returns groups in order, each with its entries. Duplicate actions
/// (multiple keys for the same action) are merged into one entry
/// with keys joined by ` / `.
pub fn help_entries(
    keymap: &Keymap,
    presets: &[crate::theme::Preset],
) -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    // Collect raw entries, merging duplicate actions.
    let mut action_keys: Vec<(AppAction, Vec<String>, &'static str, HelpGroup)> = Vec::new();
    let mut prefix_entries: Vec<HelpEntry> = Vec::new();

    for (node, km_node) in &keymap.root {
        match km_node {
            KeymapNode::Action {
                action,
                description,
                group,
            } => {
                // Use the hint character for global toggle actions (e.g., "I" instead of "shift-i").
                let key_str = toggle_hint(*action)
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| display_key(node));
                if let Some(existing) = action_keys.iter_mut().find(|(a, _, _, _)| a == action) {
                    existing.1.push(key_str);
                } else {
                    action_keys.push((*action, vec![key_str], description, *group));
                }
            }
            KeymapNode::Prefix {
                label,
                group,
                children,
            } => {
                let key_str = display_key(node);
                prefix_entries.push(HelpEntry {
                    keys: format!("{key_str} …"),
                    description: label.to_string(),
                    group: *group,
                    selection_support: children.iter().fold(
                        SelectionKindSet::empty(),
                        |acc, (_, child)| match child {
                            KeymapNode::Action { action, .. } => {
                                acc | selection_kind_set_for_action(*action)
                            }
                            _ => acc,
                        },
                    ),
                    requires_conflict: *label == CONFLICT_PREFIX,
                });
            }
            KeymapNode::Toggle { .. } => {} // toggles don't appear at root
        }
    }

    // Convert action_keys into HelpEntries.
    let mut entries: Vec<HelpEntry> = action_keys
        .into_iter()
        .map(|(action, keys, desc, group)| {
            let description = if let AppAction::SwitchPreset(idx) = action {
                if let Some(preset) = presets.get(idx) {
                    format!("{desc} ({name})", name = preset.name)
                } else {
                    desc.to_string()
                }
            } else {
                desc.to_string()
            };
            HelpEntry {
                keys: keys.join(" / "),
                description,
                group,
                selection_support: selection_kind_set_for_action(action),
                requires_conflict: false,
            }
        })
        .collect();
    entries.extend(prefix_entries);

    // Sort by group, then alphabetically by key within each group
    // (lowercase before uppercase).
    entries.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(sort_key(&a.keys).cmp(&sort_key(&b.keys)))
    });

    // Group into (HelpGroup, Vec<HelpEntry>).
    let mut groups: Vec<(HelpGroup, Vec<HelpEntry>)> = Vec::new();
    for entry in entries {
        if let Some(last) = groups.last_mut() {
            if last.0 == entry.group {
                last.1.push(entry);
                continue;
            }
        }
        let group = entry.group;
        groups.push((group, vec![entry]));
    }

    groups
}

/// Help entries for TargetSelect / CommitSelect modes.
pub fn select_mode_help_entries() -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    use HelpGroup::{General as G, Navigation as N};

    fn h(keys: &str, desc: &str, group: HelpGroup) -> HelpEntry {
        HelpEntry {
            keys: keys.into(),
            description: desc.into(),
            group,
            selection_support: SelectionKindSet::ALL,
            requires_conflict: false,
        }
    }

    let mut nav = vec![
        h("j / down", "move down", N),
        h("k / up", "move up", N),
        h("J", "next commit", N),
        h("K", "prev commit", N),
        h("ctrl-d / pagedown", "page down", N),
        h("ctrl-u / pageup", "page up", N),
        h("@", "jump to @", N),
        h("0", "go to top", N),
        h("$", "go to bottom", N),
        h("tab", "toggle fold", N),
        h("/", "search", N),
        h("ctrl-n", "next match", N),
        h("ctrl-p", "prev match", N),
    ];
    nav.sort_by(|a, b| sort_key(&a.keys).cmp(&sort_key(&b.keys)));

    let mut general = vec![
        h("Enter", "confirm selection", G),
        h("Esc", "cancel", G),
        h("?", "help", G),
    ];
    general.sort_by(|a, b| sort_key(&a.keys).cmp(&sort_key(&b.keys)));

    vec![(N, nav), (G, general)]
}
