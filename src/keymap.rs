use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ---------------------------------------------------------------------------
// CommandFlags -- toggleable flags that modify command behavior.
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CommandFlags: u16 {
        const IGNORE_IMMUTABLE    = 1 << 0;
        const NO_EDIT             = 1 << 1;
        const RETAIN_BOOKMARKS    = 1 << 2;
        const RESTORE_DESCENDANTS = 1 << 3;
        const INTERACTIVE         = 1 << 4;
        const KEEP_EMPTIED        = 1 << 5;
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
    ToggleFold,
    Refresh,
    Abandon,
    Describe,
    DescribeInEditor,
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
    ShowHelp,
    Undo,
    Redo,
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
    },
    /// Branch: this key opens a submenu with further options.
    Prefix {
        label: &'static str,
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
                    KeymapNode::Prefix { label, children } => {
                        LookupResult::Prefix { label, children }
                    }
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
        let root = vec![
            // Quit
            bind("q", AppAction::Quit, "quit"),
            bind("ctrl-c", AppAction::Quit, "quit"),
            // Line-by-line navigation
            bind("j", AppAction::MoveDown, "move down"),
            bind("down", AppAction::MoveDown, "move down"),
            bind("k", AppAction::MoveUp, "move up"),
            bind("up", AppAction::MoveUp, "move up"),
            // Section navigation (jump between commits)
            bind("shift-j", AppAction::MoveDownSection, "next commit"),
            bind("shift-k", AppAction::MoveUpSection, "prev commit"),
            // Paging
            bind("ctrl-d", AppAction::PageDown, "page down"),
            bind("pagedown", AppAction::PageDown, "page down"),
            bind("ctrl-u", AppAction::PageUp, "page up"),
            bind("pageup", AppAction::PageUp, "page up"),
            // Jump
            bind("@", AppAction::JumpToWorkingCopy, "jump to @"),
            // Fold
            bind("tab", AppAction::ToggleFold, "toggle fold"),
            // Refresh
            bind("ctrl-r", AppAction::Refresh, "refresh"),
            // Help
            bind("?", AppAction::ShowHelp, "help"),
            // Describe submenu
            prefix(
                "d",
                "describe",
                vec![
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("d", AppAction::Describe, "describe"),
                    bind("shift-d", AppAction::DescribeInEditor, "in $EDITOR"),
                ],
            ),
            // Undo submenu
            prefix(
                "u",
                "undo/redo",
                vec![
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("u", AppAction::Undo, "undo"),
                    bind("r", AppAction::Redo, "redo"),
                ],
            ),
            // Abandon submenu
            prefix(
                "a",
                "abandon",
                vec![
                    toggle("b", CommandFlags::RETAIN_BOOKMARKS, "keep bookmarks"),
                    toggle(
                        "d",
                        CommandFlags::RESTORE_DESCENDANTS,
                        "restore descendants",
                    ),
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("a", AppAction::Abandon, "abandon"),
                ],
            ),
            // Edit submenu
            prefix(
                "e",
                "edit",
                vec![
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("e", AppAction::Edit, "edit"),
                ],
            ),
            // New submenu
            prefix(
                "n",
                "new",
                vec![
                    toggle("e", CommandFlags::NO_EDIT, "no-edit"),
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("n", AppAction::New, "new"),
                    bind("a", AppAction::NewInsertAfter, "insert after"),
                    bind("b", AppAction::NewInsertBefore, "insert before"),
                ],
            ),
            // Rebase submenu
            prefix(
                "r",
                "rebase",
                vec![
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("r", AppAction::RebaseRevision, "revision..."),
                    bind("s", AppAction::RebaseSource, "source..."),
                    bind("b", AppAction::RebaseBranch, "branch..."),
                ],
            ),
            // Squash submenu
            prefix(
                "s",
                "squash",
                vec![
                    toggle("i", CommandFlags::INTERACTIVE, "interactive"),
                    toggle("k", CommandFlags::KEEP_EMPTIED, "keep emptied"),
                    toggle(
                        "shift-i",
                        CommandFlags::IGNORE_IMMUTABLE,
                        "ignore immutable",
                    ),
                    bind("s", AppAction::Squash, "into parent"),
                    bind("t", AppAction::SquashInto, "into..."),
                    bind("o", AppAction::SquashOnto, "onto..."),
                    bind("a", AppAction::SquashAfter, "after..."),
                    bind("b", AppAction::SquashBefore, "before..."),
                ],
            ),
        ];

        Keymap { root }
    }
}

/// Helper: create a (Node, KeymapNode::Action) pair from a key string.
fn bind(key_str: &str, action: AppAction, description: &'static str) -> (Node, KeymapNode) {
    let node = keymap_parser::parse(key_str).expect("valid key string in default keymap");
    (
        node,
        KeymapNode::Action {
            action,
            description,
        },
    )
}

/// Helper: create a (Node, KeymapNode::Prefix) pair for a submenu.
pub fn prefix(
    key_str: &str,
    label: &'static str,
    children: Vec<(Node, KeymapNode)>,
) -> (Node, KeymapNode) {
    let node = keymap_parser::parse(key_str).expect("valid key string in default keymap");
    (node, KeymapNode::Prefix { label, children })
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

/// Format a `Node` as a human-readable key string for display in the bottom bar.
pub fn display_key(node: &Node) -> String {
    format!("{node}")
}

// ---------------------------------------------------------------------------
// Help generation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HelpGroup {
    Navigation,
    Commands,
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
}

fn classify_action(action: AppAction) -> HelpGroup {
    match action {
        AppAction::MoveDown
        | AppAction::MoveUp
        | AppAction::MoveDownSection
        | AppAction::MoveUpSection
        | AppAction::PageDown
        | AppAction::PageUp
        | AppAction::JumpToWorkingCopy
        | AppAction::ToggleFold
        | AppAction::Refresh => HelpGroup::Navigation,
        AppAction::Abandon
        | AppAction::Describe
        | AppAction::DescribeInEditor
        | AppAction::Edit
        | AppAction::New
        | AppAction::NewInsertAfter
        | AppAction::NewInsertBefore
        | AppAction::Squash
        | AppAction::SquashInto
        | AppAction::SquashOnto
        | AppAction::SquashAfter
        | AppAction::SquashBefore
        | AppAction::Undo
        | AppAction::Redo
        | AppAction::RebaseRevision
        | AppAction::RebaseSource
        | AppAction::RebaseBranch => HelpGroup::Commands,
        AppAction::Quit | AppAction::ShowHelp => HelpGroup::General,
    }
}

/// Generate grouped help entries from the keymap trie.
///
/// Returns groups in order, each with its entries. Duplicate actions
/// (multiple keys for the same action) are merged into one entry
/// with keys joined by ` / `.
pub fn help_entries(keymap: &Keymap) -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    // Collect raw entries, merging duplicate actions.
    let mut action_keys: Vec<(AppAction, Vec<String>, &'static str)> = Vec::new();
    let mut prefix_entries: Vec<HelpEntry> = Vec::new();

    for (node, km_node) in &keymap.root {
        let key_str = display_key(node);
        match km_node {
            KeymapNode::Action {
                action,
                description,
            } => {
                if let Some(existing) = action_keys.iter_mut().find(|(a, _, _)| a == action) {
                    existing.1.push(key_str);
                } else {
                    action_keys.push((*action, vec![key_str], description));
                }
            }
            KeymapNode::Prefix { label, .. } => {
                prefix_entries.push(HelpEntry {
                    keys: format!("{key_str} ..."),
                    description: label.to_string(),
                    group: HelpGroup::Commands,
                });
            }
            KeymapNode::Toggle { .. } => {} // toggles don't appear at root
        }
    }

    // Convert action_keys into HelpEntries.
    let mut entries: Vec<HelpEntry> = action_keys
        .into_iter()
        .map(|(action, keys, desc)| HelpEntry {
            keys: keys.join(" / "),
            description: desc.to_string(),
            group: classify_action(action),
        })
        .collect();
    entries.extend(prefix_entries);

    // Sort by group, then by description within group.
    entries.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.description.cmp(&b.description))
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
