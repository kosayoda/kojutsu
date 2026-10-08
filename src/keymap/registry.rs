use super::{AppAction, Requires, SelectionKindSet};
use crate::jj_version::{InstalledJj, JjFeature};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    Builtin(AppAction),
    Lua(u16),
}

/// What an entry needs to be available: the selection kinds it takes, the
/// cursor context it reads, and the jj feature it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub selection: SelectionKindSet,
    pub requires: Requires,
    pub jj: Option<JjFeature>,
}

impl Gate {
    /// Open to everything.
    pub const OPEN: Self = Self {
        selection: SelectionKindSet::ALL,
        requires: Requires::Nothing,
        jj: None,
    };
}

/// What the cursor, the selection and the installed jj currently make
/// available. Single source for the grey-out shown in help and in the
/// submenu, which drifted apart once before.
#[derive(Clone, Copy)]
pub struct Availability {
    /// Every selected kind; empty when nothing is selected.
    pub selection: SelectionKindSet,
    pub on_file: bool,
    pub on_conflict: bool,
    pub jj: InstalledJj,
}

impl Availability {
    pub fn blocks(&self, gate: Gate) -> bool {
        self.selection.blocked_by(gate.selection)
            || match gate.requires {
                Requires::Nothing => false,
                Requires::File => !self.on_file,
                Requires::Conflict => !self.on_conflict,
            }
            || self.lacks(gate.jj)
    }

    /// Whether the installed jj predates `feature`.
    pub fn lacks(&self, feature: Option<JjFeature>) -> bool {
        feature.is_some_and(|feature| !self.jj.supports(feature))
    }
}

pub struct ActionRegistry {
    /// Selection support of Lua-registered actions, indexed by
    /// `ActionId::Lua`. Builtins carry theirs in [`AppAction::spec`].
    lua_selection: Vec<SelectionKindSet>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self {
            lua_selection: Vec::new(),
        }
    }

    /// Register a Lua action's selection support. Lua actions are never
    /// file- or conflict-gated.
    pub fn register_lua(&mut self, selection_support: SelectionKindSet) -> ActionId {
        let id = self.lua_selection.len() as u16;
        self.lua_selection.push(selection_support);
        ActionId::Lua(id)
    }

    pub fn selection_support(&self, id: ActionId) -> SelectionKindSet {
        match id {
            ActionId::Builtin(action) => action.spec().selection,
            ActionId::Lua(idx) => self
                .lua_selection
                .get(idx as usize)
                .copied()
                .unwrap_or(SelectionKindSet::ALL),
        }
    }

    pub fn gate(&self, id: ActionId) -> Gate {
        match id {
            ActionId::Builtin(action) => action.spec().gate(),
            ActionId::Lua(_) => Gate {
                selection: self.selection_support(id),
                ..Gate::OPEN
            },
        }
    }

    pub fn find_by_name(&self, name: &str) -> Option<ActionId> {
        name.parse::<AppAction>().ok().map(ActionId::Builtin)
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    use crate::keymap::AppAction;

    fn ctx(selection: SelectionKindSet, on_file: bool) -> Availability {
        Availability {
            selection,
            on_file,
            on_conflict: false,
            jj: InstalledJj::default(),
        }
    }

    fn blocks(ctx: &Availability, action: AppAction) -> bool {
        ctx.blocks(action.spec().gate())
    }

    /// FileUntrack acts on the file selection, so it stays available with the
    /// cursor parked anywhere: claiming Requires::File greyed it out while
    /// it worked.
    #[test]
    fn file_untrack_follows_the_selection_not_the_cursor() {
        let away_from_a_file = ctx(SelectionKindSet::FILE, false);
        assert!(!blocks(&away_from_a_file, AppAction::FileUntrack));
        // Annotate really does read the cursor row, so it stays gated.
        assert!(blocks(&away_from_a_file, AppAction::FileAnnotate));
    }

    /// The submenu used to test only the kind that won the precedence, so a
    /// mixed selection could offer an action the dispatcher would refuse.
    #[test]
    fn an_action_is_blocked_by_a_selection_it_cannot_take() {
        let mixed = ctx(SelectionKindSet::FILE | SelectionKindSet::LINE, true);
        // Fix takes commits and files, not lines.
        assert!(blocks(&mixed, AppAction::Fix));
        // Squash takes any selection.
        assert!(!blocks(&mixed, AppAction::Squash));
    }

    /// An action is greyed out on a jj older than what it runs, and on
    /// nothing else: an undated binary is held to nothing.
    #[test]
    fn an_action_is_blocked_by_a_jj_too_old_for_it() {
        use crate::jj_version::JjVersion;
        let mut old = ctx(SelectionKindSet::empty(), false);
        old.jj = InstalledJj::known(JjVersion::new(0, 44, 0));
        assert!(blocks(&old, AppAction::Converge));
        assert!(!blocks(&old, AppAction::Run));
        assert!(!blocks(&old, AppAction::Squash));
        let unknown = ctx(SelectionKindSet::empty(), false);
        assert!(!blocks(&unknown, AppAction::Converge));
    }

    #[test]
    fn nothing_selected_blocks_on_context_alone() {
        let empty = ctx(SelectionKindSet::empty(), true);
        assert!(!blocks(&empty, AppAction::Absorb));
        assert!(!blocks(&empty, AppAction::FileAnnotate));
        assert!(!blocks(&empty, AppAction::FileUntrack));
    }
}
