use super::{AppAction, Gate, SelectionKindSet};
use crate::jj_version::JjFeature;

/// What running an action does, beyond its own effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Only moves the cursor or viewport. These stay available inside
    /// target- and commit-select, where every other action would either
    /// mutate mid-pick or collide with the mode's own keys (space toggles a
    /// target, Enter confirms, Esc cancels).
    Navigate,
    /// Changes UI state (views, toggles, picks, prompts) but not the repo.
    Ui,
    /// Changes the repo, so it ends any repeat chain it isn't part of.
    Mutate,
}

/// Cursor context an action needs, as a hint for greying entries out:
/// enforcement lives in the handlers, which report which precondition failed
/// far more precisely than this can. Keep it matching what the handler
/// actually tests, or the UI will grey out something that works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requires {
    /// Available anywhere.
    Nothing,
    /// Needs the cursor on a file (or its diff).
    File,
    /// Needs the cursor on a conflict hunk (or conflicted file).
    Conflict,
}

/// Everything static about an action, in one place.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    pub effect: Effect,
    /// Whether `.` repeats it.
    pub repeatable: bool,
    /// The selection kinds it can act on.
    pub selection: SelectionKindSet,
    pub requires: Requires,
    /// The jj feature the command it runs needs, as a hint for greying it
    /// out on an older jj: dispatch refuses the built command itself, from
    /// the flags it actually carries. Keep it matching what the action runs.
    pub jj: Option<JjFeature>,
    /// A friendlier name for messages; `None` reads as the stable id.
    label: Option<&'static str>,
}

impl ActionSpec {
    const fn new(effect: Effect, selection: SelectionKindSet) -> Self {
        Self {
            effect,
            repeatable: false,
            selection,
            requires: Requires::Nothing,
            jj: None,
            label: None,
        }
    }

    /// What the grey-out tests this action against.
    pub const fn gate(&self) -> Gate {
        Gate {
            selection: self.selection,
            requires: self.requires,
            jj: self.jj,
        }
    }

    const fn repeatable(mut self) -> Self {
        self.repeatable = true;
        self
    }

    const fn requires(mut self, requires: Requires) -> Self {
        self.requires = requires;
        self
    }

    const fn needs(mut self, feature: JjFeature) -> Self {
        self.jj = Some(feature);
        self
    }

    const fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

const fn nav() -> ActionSpec {
    ActionSpec::new(Effect::Navigate, SelectionKindSet::ALL)
}

const fn ui(selection: SelectionKindSet) -> ActionSpec {
    ActionSpec::new(Effect::Ui, selection)
}

const fn mutate(selection: SelectionKindSet) -> ActionSpec {
    ActionSpec::new(Effect::Mutate, selection)
}

const ALL: SelectionKindSet = SelectionKindSet::ALL;
const C: SelectionKindSet = SelectionKindSet::COMMIT;
const F: SelectionKindSet = SelectionKindSet::FILE;
const CF: SelectionKindSet = SelectionKindSet::COMMIT.union(SelectionKindSet::FILE);

impl AppAction {
    /// Static facts about this action. One arm per action, so adding one
    /// means deciding all of them here.
    pub const fn spec(self) -> ActionSpec {
        use AppAction::*;
        match self {
            Quit => ui(ALL),
            MoveDown => nav(),
            MoveUp => nav(),
            MoveDownSection => nav(),
            MoveUpSection => nav(),
            PageDown => nav(),
            PageUp => nav(),
            JumpToWorkingCopy => nav(),
            MoveToTop => nav(),
            MoveToBottom => nav(),
            MoveToScreenTop => nav(),
            MoveToScreenMiddle => nav(),
            MoveToScreenBottom => nav(),
            ScrollLeft => nav(),
            ScrollRight => nav(),
            ToggleFold => nav(),
            Refresh => ui(ALL),
            ReloadConfig => ui(ALL),
            ExpandAncestors => ui(ALL).label("expand"),
            ExpandDescendants => ui(ALL).label("expand"),
            Abandon => mutate(C).repeatable(),
            Absorb => mutate(ALL).repeatable(),
            Commit => mutate(ALL),
            CommitWithMessage => mutate(ALL).label("commit"),
            Describe => mutate(C),
            DescribeInEditor => mutate(C).label("describe"),
            Diffedit => mutate(CF).label("diffedit"),
            Edit => mutate(C),
            New => mutate(C),
            NewInsertAfter => mutate(C).label("new"),
            NewInsertBefore => mutate(C).label("new"),
            Squash => mutate(ALL),
            SquashInto => mutate(ALL).label("squash"),
            SquashOnto => mutate(ALL).label("squash"),
            SquashAfter => mutate(ALL).label("squash"),
            SquashBefore => mutate(ALL).label("squash"),
            RebaseRevision => mutate(C).label("rebase"),
            RebaseSource => mutate(C).label("rebase"),
            RebaseBranch => mutate(C).label("rebase"),
            Restore => mutate(ALL),
            RestoreFrom => mutate(ALL).label("restore"),
            RestoreInto => mutate(ALL).label("restore"),
            Split => mutate(ALL),
            SplitOnto => mutate(ALL).label("split"),
            SplitAfter => mutate(ALL).label("split"),
            SplitBefore => mutate(ALL).label("split"),
            EditRevset => ui(ALL).label("revset"),
            EditRevsetInEditor => ui(ALL).label("revset"),
            ResetRevset => ui(ALL),
            ToggleConflictedRevset => ui(ALL),
            BookmarkCreate => mutate(C).label("bookmark"),
            BookmarkSet => mutate(C).label("bookmark"),
            BookmarkDelete => mutate(C).label("bookmark"),
            BookmarkForget => mutate(C).label("bookmark"),
            BookmarkMove => mutate(C).label("bookmark"),
            BookmarkRename => mutate(C).label("bookmark"),
            BookmarkAdvance => mutate(C)
                .needs(JjFeature::BookmarkAdvance)
                .label("bookmark"),
            BookmarkTrack => mutate(C).label("bookmark"),
            BookmarkUntrack => mutate(C).label("bookmark"),
            ShowHelp => nav(),
            Undo => mutate(C).repeatable(),
            Redo => mutate(C).repeatable(),
            GitFetch => mutate(C).label("git"),
            GitFetchAllRemotes => mutate(C).label("git"),
            GitPush => mutate(C).label("git"),
            GitPushAll => mutate(C).label("git"),
            GitPushChange => mutate(C).label("git"),
            GitExport => mutate(C).label("git"),
            GitImport => mutate(C).label("git"),
            Duplicate => mutate(C).repeatable(),
            DuplicateOnto => mutate(C).label("duplicate"),
            Parallelize => mutate(C).repeatable(),
            SimplifyParents => mutate(C).repeatable().label("simplify-parents"),
            Revert => mutate(C),
            ArrangeUp => mutate(ALL).repeatable().label("arrange"),
            ArrangeDown => mutate(ALL).repeatable().label("arrange"),
            Fix => mutate(CF).repeatable(),
            Run => mutate(C).needs(JjFeature::Run),
            FileUntrack => mutate(F).label("untrack"),
            ResolveOurs => mutate(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("resolve"),
            ResolveTheirs => mutate(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("resolve"),
            ResolveMergeTool => mutate(F).requires(Requires::Conflict).label("resolve"),
            ConflictPickOurs => ui(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("pick conflict side"),
            ConflictPickTheirs => ui(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("pick conflict side"),
            ConflictPickBase => ui(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("pick conflict side"),
            ConflictUnpick => ui(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("pick conflict side"),
            ConflictApplyPicks => mutate(F)
                .requires(Requires::Conflict)
                .repeatable()
                .label("apply picks"),
            ConflictEditFile => mutate(F)
                .requires(Requires::Conflict)
                .label("edit conflict"),
            ConflictEditHunk => ui(F).requires(Requires::Conflict).label("edit conflict"),
            ToggleIgnoreImmutable => ui(ALL),
            ToggleIgnoreWorkingCopy => ui(ALL),
            ToggleDebug => ui(ALL),
            ToggleGitDiff => ui(ALL),
            ToggleLineNumbers => ui(ALL),
            ToggleDiffUnderline => ui(ALL),
            WorkspaceAdd => mutate(ALL),
            WorkspaceForget => mutate(ALL),
            WorkspaceList => ui(ALL),
            WorkspaceRename => mutate(ALL),
            ToggleSelect => ui(ALL),
            EnterVisualMode => ui(ALL),
            StartSearch => nav(),
            NextMatch => nav(),
            PrevMatch => nav(),
            NextConflict => ui(ALL).repeatable(),
            PrevConflict => ui(ALL).repeatable(),
            GitPushBookmark => mutate(C).label("git"),
            GitFetchBookmark => mutate(ALL).label("git"),
            JumpToCommit => ui(ALL).label("jump"),
            TagSet => mutate(C).label("tag"),
            TagDelete => mutate(C).label("tag"),
            TagTrack => mutate(C).needs(JjFeature::TagTracking).label("tag"),
            TagUntrack => mutate(C).needs(JjFeature::TagTracking).label("tag"),
            Converge => mutate(C).needs(JjFeature::Converge).label("converge"),
            SelectPreset => ui(ALL).label("preset"),
            SwitchPreset1 => ui(ALL),
            SwitchPreset2 => ui(ALL),
            SwitchPreset3 => ui(ALL),
            SwitchPreset4 => ui(ALL),
            SwitchPreset5 => ui(ALL),
            SwitchToDagView => ui(ALL),
            SwitchToBookmarkView => ui(ALL),
            SwitchToTagView => ui(ALL),
            SwitchToOpLogView => ui(ALL),
            SwitchToWorkspaceView => ui(ALL),
            SwitchToEvoLogView => ui(ALL),
            SwitchToCommandLogView => ui(ALL),
            Jump => nav(),
            EvoLogRestore => mutate(ALL),
            Interdiff => ui(C),
            FileAnnotate => ui(F).requires(Requires::File).label("annotate"),
            AnnotateTimeTravel => ui(C).label("annotate"),
            AnnotateForward => ui(C).label("annotate"),
            ToggleSeparators => ui(ALL),
            EditFileWorkingCopy => ui(CF).requires(Requires::File).label("edit"),
            EditFileAtRevision => ui(CF).requires(Requires::File).label("edit"),
            CheckoutAndEditFile => ui(CF).requires(Requires::File).label("edit"),
            OpLogRestore => mutate(ALL),
            OpLogRevert => mutate(ALL),
            OpLogAbandon => mutate(ALL),
            OpLogFilterWorkspace => ui(ALL),
            CommandMode => mutate(ALL),
            FileList => ui(ALL),
            RepeatLast => ui(ALL).label("repeat"),
        }
    }

    /// Stable snake_case identifier: the name Lua plugins bind and hook on.
    pub fn id_name(self) -> &'static str {
        self.into()
    }

    /// The name used in messages about this action. Anything without a
    /// friendlier name reads as its stable id, which is always meaningful.
    pub fn label(self) -> &'static str {
        self.spec().label.unwrap_or_else(|| self.id_name())
    }
}
