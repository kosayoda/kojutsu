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
        const ALLOW_NEW           = 1 << 11;
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
    ToggleFold,
    Refresh,
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
    ToggleIgnoreImmutable,
    ToggleIgnoreWorkingCopy,
    ToggleDebug,
    ToggleLineNumbers,
    WorkspaceAdd,
    WorkspaceForget,
    WorkspaceList,
    ToggleSelect,
    EnterVisualMode,
    StartSearch,
    NextMatch,
    PrevMatch,
    GitPushBookmark,
    SwitchPreset(usize),
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

// ---------------------------------------------------------------------------
// Default keymap construction
// ---------------------------------------------------------------------------

impl Default for Keymap {
    fn default() -> Self {
        use HelpGroup::{Commands as C, General as G, Navigation as N};

        let root = vec![
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
            // Section navigation (jump between commits)
            bind("shift-j", AppAction::MoveDownSection, "next commit", N),
            bind("shift-k", AppAction::MoveUpSection, "prev commit", N),
            // Paging
            bind("ctrl-d", AppAction::PageDown, "page down", N),
            bind("pagedown", AppAction::PageDown, "page down", N),
            bind("ctrl-u", AppAction::PageUp, "page up", N),
            bind("pageup", AppAction::PageUp, "page up", N),
            // Jump
            bind("@", AppAction::JumpToWorkingCopy, "jump to @", N),
            bind("0", AppAction::MoveToTop, "go to top", N),
            bind("$", AppAction::MoveToBottom, "go to bottom", N),
            // Revset presets
            bind("1", AppAction::SwitchPreset(0), "preset 1", G),
            bind("2", AppAction::SwitchPreset(1), "preset 2", G),
            bind("3", AppAction::SwitchPreset(2), "preset 3", G),
            bind("4", AppAction::SwitchPreset(3), "preset 4", G),
            bind("5", AppAction::SwitchPreset(4), "preset 5", G),
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
            // Absorb
            bind("a", AppAction::Absorb, "absorb", C),
            // Bookmark submenu
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
            // Commit submenu
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
            // Describe submenu
            prefix(
                "d",
                "describe",
                C,
                vec![
                    bind("d", AppAction::Describe, "describe", C),
                    bind("shift-d", AppAction::DescribeInEditor, "in $EDITOR", C),
                ],
            ),
            // Edit
            bind("e", AppAction::Edit, "edit", C),
            // Git submenu
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
                            toggle("n", CommandFlags::ALLOW_NEW, "allow new"),
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
            // New submenu
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
            // Rebase submenu
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
            // Restore submenu
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
            // Split submenu
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
            // Squash submenu
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
            // Undo submenu
            prefix(
                "u",
                "undo/redo",
                C,
                vec![
                    bind("u", AppAction::Undo, "undo", C),
                    bind("r", AppAction::Redo, "redo", C),
                ],
            ),
            // Abandon submenu
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
            // Duplicate submenu
            prefix(
                "y",
                "duplicate",
                C,
                vec![
                    bind("y", AppAction::Duplicate, "duplicate", C),
                    bind("t", AppAction::DuplicateOnto, "onto…", C),
                ],
            ),
            // Workspace
            prefix(
                "w",
                "workspace",
                C,
                vec![
                    bind("a", AppAction::WorkspaceAdd, "add", C),
                    bind("f", AppAction::WorkspaceForget, "forget", C),
                    bind("l", AppAction::WorkspaceList, "list", C),
                ],
            ),
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
                    bind("l", AppAction::ToggleLineNumbers, "toggle line numbers", G),
                ],
            ),
        ];

        Keymap { root }
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
}

pub fn action_supported_selection_kinds(action: AppAction) -> &'static [SelectionKind] {
    use SelectionKind::{Commit, File, Line};
    match action {
        AppAction::Squash | AppAction::SquashSelect(_) => &[Commit, File, Line],
        AppAction::Restore | AppAction::RestoreFrom | AppAction::RestoreInto => {
            &[Commit, File, Line]
        }
        AppAction::Split
        | AppAction::SplitOnto
        | AppAction::SplitAfter
        | AppAction::SplitBefore => &[Commit, File, Line],
        AppAction::Commit | AppAction::CommitWithMessage => &[Commit, File, Line],
        AppAction::Absorb => &[Commit, File],
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
        | AppAction::DuplicateOnto => &[Commit],
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
        AppAction::Duplicate | AppAction::DuplicateOnto => "duplicate",
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

/// Generate grouped help entries from the keymap trie.
///
/// Returns groups in order, each with its entries. Duplicate actions
/// (multiple keys for the same action) are merged into one entry
/// with keys joined by ` / `.
pub fn help_entries(keymap: &Keymap) -> Vec<(HelpGroup, Vec<HelpEntry>)> {
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
                });
            }
            KeymapNode::Toggle { .. } => {} // toggles don't appear at root
        }
    }

    // Convert action_keys into HelpEntries.
    let mut entries: Vec<HelpEntry> = action_keys
        .into_iter()
        .map(|(_action, keys, desc, group)| HelpEntry {
            keys: keys.join(" / "),
            description: desc.to_string(),
            group,
            selection_support: selection_kind_set_for_action(_action),
        })
        .collect();
    entries.extend(prefix_entries);

    // Sort by group
    entries.sort_by(|a, b| a.group.cmp(&b.group));

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
    let s = SelectionKindSet::ALL;

    let nav = vec![
        HelpEntry {
            keys: "j / down".into(),
            description: "move down".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "k / up".into(),
            description: "move up".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "J".into(),
            description: "next commit".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "K".into(),
            description: "prev commit".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "ctrl-d / pagedown".into(),
            description: "page down".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "ctrl-u / pageup".into(),
            description: "page up".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "@".into(),
            description: "jump to @".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "0".into(),
            description: "go to top".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "$".into(),
            description: "go to bottom".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "tab".into(),
            description: "toggle fold".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "/".into(),
            description: "search".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "ctrl-n".into(),
            description: "next match".into(),
            group: N,
            selection_support: s,
        },
        HelpEntry {
            keys: "ctrl-p".into(),
            description: "prev match".into(),
            group: N,
            selection_support: s,
        },
    ];

    let general = vec![
        HelpEntry {
            keys: "Enter".into(),
            description: "confirm selection".into(),
            group: G,
            selection_support: s,
        },
        HelpEntry {
            keys: "Esc".into(),
            description: "cancel".into(),
            group: G,
            selection_support: s,
        },
        HelpEntry {
            keys: "?".into(),
            description: "help".into(),
            group: G,
            selection_support: s,
        },
    ];

    vec![(N, nav), (G, general)]
}
