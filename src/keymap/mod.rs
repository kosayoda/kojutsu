mod bindings;
mod help;
mod registry;
mod spec;
mod trie;

use keymap_parser::{Key, Modifier, Node};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use bindings::{BindTarget, BindingSpec, Scope, default_bindings};
pub use help::{HelpEntry, HelpGroup, help_entries, select_mode_help_entries};
pub use registry::{ActionId, ActionRegistry, Availability};
pub use spec::{ActionSpec, Effect, Requires};
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
    /// `self` is what is selected: empty means nothing is, which rules
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
/// generated type definitions: data-carrying variants would silently fall
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
    ReloadConfig,
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
    GitFetchBookmark,
    JumpToCommit,
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
    SwitchToOpLogView,
    SwitchToWorkspaceView,
    #[strum(serialize = "switch_to_evolog_view")]
    SwitchToEvoLogView,
    SwitchToCommandLogView,
    Jump,
    #[strum(serialize = "evolog_restore")]
    EvoLogRestore,
    Interdiff,
    FileAnnotate,
    AnnotateTimeTravel,
    AnnotateForward,
    ToggleSeparators,
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

/// The keys of one binding, pressed in order.
pub type Keys = smallvec::SmallVec<[Node; 3]>;

/// Parse a space-separated key sequence such as `"g p c"`.
pub fn parse_sequence(seq: &str) -> Result<Keys, String> {
    let keys = seq
        .split_whitespace()
        .map(|key| try_parse_key(key).ok_or_else(|| format!("invalid key `{key}`")))
        .collect::<Result<Keys, _>>()?;
    if keys.is_empty() {
        return Err("empty key sequence".into());
    }
    Ok(keys)
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
    use super::{AppAction, SelectionKindSet};
    use strum::IntoEnumIterator as _;

    /// A refusal names the action. Anything that can be selection-blocked
    /// reaches this message, and a placeholder there tells the user nothing.
    #[test]
    fn every_blockable_action_names_itself() {
        for action in AppAction::iter() {
            if action.spec().selection == SelectionKindSet::ALL {
                continue;
            }
            let label = action.label();
            assert!(!label.is_empty(), "{action:?} has an empty label");
            assert_ne!(label, "action", "{action:?} falls back to a placeholder");
        }
    }

    /// The conflict picks were the group that reached the placeholder. They
    /// should read as something written for a person: asserted as "not the
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
                action.label(),
                action.id_name(),
                "{action:?} has no label of its own"
            );
        }
    }
}
