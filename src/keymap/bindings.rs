use compact_str::CompactString;
use smallvec::SmallVec;

use super::registry::ActionId;
use super::{AppAction, CommandFlags, HelpGroup, Keys, parse_sequence};
use crate::jj_version::JjFeature;
use crate::types::ActiveView;

pub struct BindingSpec {
    pub keys: Keys,
    pub target: BindTarget,
    pub scope: Scope,
}

pub enum BindTarget {
    Action {
        id: ActionId,
        description: CompactString,
        group: HelpGroup,
    },
    Prefix {
        label: CompactString,
        group: HelpGroup,
    },
    Toggle {
        flag: CommandFlags,
        description: CompactString,
        jj: Option<JjFeature>,
    },
    Unbind,
}

pub enum Scope {
    All,
    Views(SmallVec<[ActiveView; 4]>),
}

/// The keys of a built-in binding, which are known to parse.
fn keys(seq: &str) -> Keys {
    parse_sequence(seq).expect("valid built-in key sequence")
}

fn bind(seq: &str, action: AppAction, desc: &str, group: HelpGroup, scope: Scope) -> BindingSpec {
    BindingSpec {
        keys: keys(seq),
        target: BindTarget::Action {
            id: ActionId::Builtin(action),
            description: desc.into(),
            group,
        },
        scope,
    }
}

fn prefix(seq: &str, label: &str, group: HelpGroup, scope: Scope) -> BindingSpec {
    BindingSpec {
        keys: keys(seq),
        target: BindTarget::Prefix {
            label: label.into(),
            group,
        },
        scope,
    }
}

fn toggle(seq: &str, flag: CommandFlags, desc: &str, scope: Scope) -> BindingSpec {
    BindingSpec {
        keys: keys(seq),
        target: BindTarget::Toggle {
            flag,
            description: desc.into(),
            jj: None,
        },
        scope,
    }
}

/// A toggle for a flag only jj from `feature`'s release on takes. A hint, as
/// for actions: dispatch refuses the command the flag ends up in.
fn newer_toggle(
    seq: &str,
    flag: CommandFlags,
    desc: &str,
    feature: JjFeature,
    scope: Scope,
) -> BindingSpec {
    BindingSpec {
        keys: keys(seq),
        target: BindTarget::Toggle {
            flag,
            description: desc.into(),
            jj: Some(feature),
        },
        scope,
    }
}

fn undo_redo(mk: impl Fn() -> Scope) -> [BindingSpec; 3] {
    use AppAction::*;
    use HelpGroup::Commands as C;
    [
        prefix("u", "undo/redo", C, mk()),
        bind("u u", Undo, "undo", C, mk()),
        bind("u r", Redo, "redo", C, mk()),
    ]
}

fn file_prefix_bindings(mk: impl Fn() -> Scope) -> [BindingSpec; 7] {
    use AppAction::*;
    use HelpGroup::Commands as C;
    [
        prefix("shift-f", super::FILE_PREFIX, C, mk()),
        bind("shift-f a", FileAnnotate, "annotate", C, mk()),
        bind("shift-f l", FileList, "file list", C, mk()),
        prefix("shift-f e", "edit", C, mk()),
        bind("shift-f e e", EditFileWorkingCopy, "working copy", C, mk()),
        bind(
            "shift-f e r",
            EditFileAtRevision,
            "view at revision",
            C,
            mk(),
        ),
        bind(
            "shift-f e c",
            CheckoutAndEditFile,
            "checkout and edit",
            C,
            mk(),
        ),
    ]
}

fn dag() -> Scope {
    Scope::Views(smallvec::smallvec![ActiveView::Dag])
}

fn views(vs: &[ActiveView]) -> Scope {
    Scope::Views(SmallVec::from_slice(vs))
}

pub fn default_bindings() -> Vec<BindingSpec> {
    use ActiveView::*;
    use AppAction::*;
    use HelpGroup::{Commands as C, General as G, Navigation as N};

    let mut specs = Vec::new();

    // Shared bindings (all views)
    let all = || Scope::All;
    specs.extend([
        bind("?", ShowHelp, "help", G, all()),
        bind("q", Quit, "quit", G, all()),
        bind("ctrl-c", Quit, "quit", G, all()),
        bind("j", MoveDown, "move down", N, all()),
        bind("down", MoveDown, "move down", N, all()),
        bind("k", MoveUp, "move up", N, all()),
        bind("up", MoveUp, "move up", N, all()),
        bind("shift-j", MoveDownSection, "next commit", N, all()),
        bind("shift-k", MoveUpSection, "prev commit", N, all()),
        bind("ctrl-d", PageDown, "page down", N, all()),
        bind("pagedown", PageDown, "page down", N, all()),
        bind("ctrl-u", PageUp, "page up", N, all()),
        bind("pageup", PageUp, "page up", N, all()),
        bind("left", ScrollLeft, "scroll left", N, all()),
        bind("right", ScrollRight, "scroll right", N, all()),
        bind("h", ScrollLeft, "scroll left", N, all()),
        bind("l", ScrollRight, "scroll right", N, all()),
        bind("@", JumpToWorkingCopy, "jump to @", N, all()),
        bind("0", MoveToTop, "go to top", N, all()),
        bind("$", MoveToBottom, "go to bottom", N, all()),
        bind("shift-h", MoveToScreenTop, "screen top", N, all()),
        bind("shift-m", MoveToScreenMiddle, "screen middle", N, all()),
        bind("shift-l", MoveToScreenBottom, "screen bottom", N, all()),
        bind("1", SwitchToDagView, "DAG view", G, all()),
        bind("2", SwitchToBookmarkView, "bookmarks view", G, all()),
        bind("3", SwitchToTagView, "tags view", G, all()),
        bind("4", SwitchToWorkspaceView, "workspaces view", G, all()),
        bind("5", SwitchToOpLogView, "operations view", G, all()),
        bind("6", SwitchToEvoLogView, "evolog view", G, all()),
        bind("7", SwitchToCommandLogView, "command log", G, all()),
        bind("'", Jump, "jump", N, all()),
        bind("tab", ToggleFold, "toggle fold", N, all()),
        bind("space", ToggleSelect, "toggle select", N, all()),
        bind("v", EnterVisualMode, "visual select", N, all()),
        bind("/", StartSearch, "search", N, all()),
        bind("ctrl-n", NextMatch, "next match", N, all()),
        bind("ctrl-p", PrevMatch, "prev match", N, all()),
        bind(
            "shift-i",
            ToggleIgnoreImmutable,
            "toggle ignore-immutable",
            G,
            all(),
        ),
        bind(
            "shift-w",
            ToggleIgnoreWorkingCopy,
            "toggle ignore-working-copy",
            G,
            all(),
        ),
        bind("shift-d", ToggleDebug, "toggle debug", G, all()),
        bind("ctrl-r", Refresh, "refresh", N, all()),
        bind(".", RepeatLast, "repeat last", C, all()),
        bind(":", CommandMode, "command mode", N, all()),
        // Command palette (`;` prefix)
        prefix(";", "command", G, all()),
        bind("; r", EditRevset, "edit revset", G, all()),
        bind(
            "; shift-r",
            EditRevsetInEditor,
            "edit revset in $EDITOR",
            G,
            all(),
        ),
        bind("; c", ReloadConfig, "reload init.lua", G, all()),
        bind("; d", ResetRevset, "default revset", G, all()),
        bind("; p", SelectPreset, "switch preset", G, all()),
        bind("; l", ToggleLineNumbers, "toggle line numbers", G, all()),
        bind("; g", ToggleGitDiff, "toggle diff style", G, all()),
        bind(
            "; u",
            ToggleDiffUnderline,
            "toggle diff underline",
            G,
            all(),
        ),
        bind("; s", ToggleSeparators, "toggle separator lines", G, all()),
        bind("; 1", SwitchPreset1, "preset 1", G, all()),
        bind("; 2", SwitchPreset2, "preset 2", G, all()),
        bind("; 3", SwitchPreset3, "preset 3", G, all()),
        bind("; 4", SwitchPreset4, "preset 4", G, all()),
        bind("; 5", SwitchPreset5, "preset 5", G, all()),
    ]);

    // DAG view
    specs.extend([
        bind("ctrl-e", SwitchToEvoLogView, "evolog", C, dag()),
        bind("shift-e", Diffedit, "diffedit", C, dag()),
        bind("a", Absorb, "absorb", C, dag()),
        bind("+", ExpandAncestors, "expand ancestors", C, dag()),
        bind("-", ExpandDescendants, "expand descendants", C, dag()),
        // Conflict prefix
        prefix("shift-c", super::CONFLICT_PREFIX, C, dag()),
        bind("shift-c o", ResolveOurs, "take ours", C, dag()),
        bind("shift-c t", ResolveTheirs, "take theirs", C, dag()),
        bind("shift-c b", ConflictPickBase, "take base", C, dag()),
        bind("shift-c u", ConflictUnpick, "unpick hunk", C, dag()),
        bind("shift-c a", ConflictApplyPicks, "apply picks", C, dag()),
        bind("shift-c e", ConflictEditHunk, "edit hunk", C, dag()),
        bind(
            "shift-c shift-e",
            ConflictEditFile,
            "edit resolution",
            C,
            dag(),
        ),
        bind("shift-c m", ResolveMergeTool, "merge tool", C, dag()),
        bind(
            "shift-c r",
            ToggleConflictedRevset,
            "conflicted() revset",
            C,
            dag(),
        ),
        // Next/prev navigation prefixes
        prefix("]", "next", N, dag()),
        prefix("[", "prev", N, dag()),
        bind("] c", NextConflict, "next conflict", N, dag()),
        bind("[ c", PrevConflict, "prev conflict", N, dag()),
        // Fix
        bind("f", Fix, "fix", C, dag()),
        // Converge prefix
        prefix("m", "converge", C, dag()),
        toggle(
            "m shift-n",
            CommandFlags::NO_INTERACTIVE,
            "no prompts",
            dag(),
        ),
        bind("m m", Converge, "converge divergent", C, dag()),
        // Run prefix
        prefix("!", "run", C, dag()),
        toggle("! shift-c", CommandFlags::CLEAN, "clean", dag()),
        toggle(
            "! shift-d",
            CommandFlags::RESTORE_DESCENDANTS,
            "restore descendants",
            dag(),
        ),
        newer_toggle(
            "! shift-p",
            CommandFlags::PASSTHROUGH,
            "passthrough",
            JjFeature::RunPassthrough,
            dag(),
        ),
        newer_toggle(
            "! shift-i",
            CommandFlags::IGNORE_CHANGES,
            "ignore changes",
            JjFeature::RunIgnoreChanges,
            dag(),
        ),
        newer_toggle(
            "! shift-e",
            CommandFlags::IGNORE_ERRORS,
            "ignore errors",
            JjFeature::RunIgnoreErrors,
            dag(),
        ),
        bind("! !", Run, "run command\u{2026}", C, dag()),
        // File prefix (dag also has untrack)
        bind("shift-f u", FileUntrack, "untrack", C, dag()),
        // Bookmark prefix
        prefix("b", "bookmark", C, dag()),
        toggle(
            "b shift-b",
            CommandFlags::ALLOW_BACKWARDS,
            "allow backwards",
            dag(),
        ),
        bind("b c", BookmarkCreate, "create", C, dag()),
        bind("b s", BookmarkSet, "set", C, dag()),
        bind("b d", BookmarkDelete, "delete", C, dag()),
        bind("b f", BookmarkForget, "forget", C, dag()),
        bind("b m", BookmarkMove, "move\u{2026}", C, dag()),
        bind("b r", BookmarkRename, "rename", C, dag()),
        bind("b a", BookmarkAdvance, "advance", C, dag()),
        bind("b t", BookmarkTrack, "track", C, dag()),
        bind("b u", BookmarkUntrack, "untrack", C, dag()),
        // Tag prefix
        prefix("t", "tag", C, dag()),
        toggle("t shift-m", CommandFlags::ALLOW_MOVE, "allow move", dag()),
        bind("t s", TagSet, "set", C, dag()),
        bind("t d", TagDelete, "delete", C, dag()),
        bind("t t", TagTrack, "track", C, dag()),
        bind("t u", TagUntrack, "untrack", C, dag()),
        // Commit prefix
        prefix("c", "commit", C, dag()),
        toggle("c shift-i", CommandFlags::INTERACTIVE, "interactive", dag()),
        bind("c c", Commit, "commit (in $EDITOR)", C, dag()),
        bind("c m", CommitWithMessage, "with message", C, dag()),
        // Describe prefix
        prefix("d", "describe", C, dag()),
        bind("d d", Describe, "describe", C, dag()),
        bind("d shift-d", DescribeInEditor, "in $EDITOR", C, dag()),
        // Edit
        bind("e", Edit, "edit", C, dag()),
        // Git prefix
        prefix("g", "git", C, dag()),
        toggle(
            "g shift-d",
            CommandFlags::DRY_RUN,
            "dry run (push only)",
            dag(),
        ),
        prefix("g f", "fetch", C, dag()),
        bind("g f f", GitFetch, "fetch", C, dag()),
        bind("g f a", GitFetchAllRemotes, "all remotes", C, dag()),
        prefix("g p", "push", C, dag()),
        bind("g p p", GitPush, "push", C, dag()),
        bind("g p a", GitPushAll, "all bookmarks", C, dag()),
        bind("g p c", GitPushChange, "change", C, dag()),
        bind("g p b", GitPushBookmark, "bookmark", C, dag()),
        bind("g e", GitExport, "export (jj -> git)", C, dag()),
        bind("g i", GitImport, "import (git -> jj)", C, dag()),
        // New prefix
        prefix("n", "new", C, dag()),
        toggle("n shift-e", CommandFlags::NO_EDIT, "no-edit", dag()),
        bind("n n", New, "new", C, dag()),
        bind("n a", NewInsertAfter, "insert after", C, dag()),
        bind("n b", NewInsertBefore, "insert before", C, dag()),
        // Rebase prefix
        prefix("r", "rebase", C, dag()),
        bind("r r", RebaseRevision, "revision\u{2026}", C, dag()),
        bind("r s", RebaseSource, "source\u{2026}", C, dag()),
        bind("r b", RebaseBranch, "branch\u{2026}", C, dag()),
        bind("r k", ArrangeUp, "arrange up", C, dag()),
        bind("r j", ArrangeDown, "arrange down", C, dag()),
        // Restore prefix
        prefix("shift-r", "restore", C, dag()),
        toggle(
            "shift-r shift-i",
            CommandFlags::INTERACTIVE,
            "interactive",
            dag(),
        ),
        toggle(
            "shift-r shift-d",
            CommandFlags::RESTORE_DESCENDANTS,
            "restore descendants",
            dag(),
        ),
        bind("shift-r shift-r", Restore, "changes-in", C, dag()),
        bind("shift-r f", RestoreFrom, "from\u{2026}", C, dag()),
        bind("shift-r t", RestoreInto, "into\u{2026}", C, dag()),
        // Split prefix
        prefix("shift-s", "split", C, dag()),
        toggle(
            "shift-s shift-i",
            CommandFlags::INTERACTIVE,
            "interactive",
            dag(),
        ),
        toggle("shift-s shift-p", CommandFlags::PARALLEL, "parallel", dag()),
        bind("shift-s shift-s", Split, "split", C, dag()),
        bind("shift-s o", SplitOnto, "onto\u{2026}", C, dag()),
        bind("shift-s a", SplitAfter, "after\u{2026}", C, dag()),
        bind("shift-s b", SplitBefore, "before\u{2026}", C, dag()),
        // Squash prefix
        prefix("s", "squash", C, dag()),
        toggle("s shift-i", CommandFlags::INTERACTIVE, "interactive", dag()),
        toggle(
            "s shift-k",
            CommandFlags::KEEP_EMPTIED,
            "keep emptied",
            dag(),
        ),
        bind("s s", Squash, "into parent", C, dag()),
        bind("s t", SquashInto, "into\u{2026}", C, dag()),
        bind("s o", SquashOnto, "onto\u{2026}", C, dag()),
        bind("s a", SquashAfter, "after\u{2026}", C, dag()),
        bind("s b", SquashBefore, "before\u{2026}", C, dag()),
        // Abandon prefix
        prefix("x", "abandon", C, dag()),
        toggle(
            "x shift-b",
            CommandFlags::RETAIN_BOOKMARKS,
            "keep bookmarks",
            dag(),
        ),
        toggle(
            "x shift-d",
            CommandFlags::RESTORE_DESCENDANTS,
            "restore descendants",
            dag(),
        ),
        bind("x x", Abandon, "abandon", C, dag()),
        // Duplicate prefix
        prefix("y", "duplicate", C, dag()),
        bind("y y", Duplicate, "duplicate", C, dag()),
        bind("y t", DuplicateOnto, "onto\u{2026}", C, dag()),
        // Direct bindings
        bind("i", AppAction::Interdiff, "interdiff\u{2026}", C, dag()),
        bind("p", Parallelize, "parallelize", C, dag()),
        bind("shift-p", SimplifyParents, "simplify parents", C, dag()),
        bind("z", Revert, "revert", C, dag()),
        // Workspace prefix
        prefix("w", "workspace", C, dag()),
        newer_toggle(
            "w shift-c",
            CommandFlags::COLOCATE,
            "colocate",
            JjFeature::WorkspaceColocation,
            dag(),
        ),
        newer_toggle(
            "w shift-n",
            CommandFlags::NO_COLOCATE,
            "no colocate",
            JjFeature::WorkspaceColocation,
            dag(),
        ),
        bind("w a", WorkspaceAdd, "add", C, dag()),
        bind("w f", WorkspaceForget, "forget", C, dag()),
        bind("w x", WorkspaceRemove, "remove (deletes it)", C, dag()),
        bind("w l", WorkspaceList, "list", C, dag()),
        bind("w r", WorkspaceRename, "rename", C, dag()),
    ]);
    specs.extend(undo_redo(dag));
    specs.extend(file_prefix_bindings(dag));

    // Bookmark view
    let bookmark = || views(&[Bookmarks]);
    specs.extend([
        bind("d", BookmarkDelete, "delete", C, bookmark()),
        bind("t", BookmarkTrack, "track", C, bookmark()),
        bind("shift-u", BookmarkUntrack, "untrack", C, bookmark()),
        bind("p", GitPushBookmark, "push", C, bookmark()),
        bind("enter", JumpToCommit, "jump / pick", C, bookmark()),
        bind("e", Edit, "edit (checkout)", C, bookmark()),
        bind("r", BookmarkRename, "rename", C, bookmark()),
        bind("m", BookmarkMove, "move\u{2026}", C, bookmark()),
        prefix("f", "fetch", C, bookmark()),
        bind("f f", GitFetch, "fetch", C, bookmark()),
        bind("f b", GitFetchBookmark, "bookmark", C, bookmark()),
        bind("f a", GitFetchAllRemotes, "all remotes", C, bookmark()),
        bind("s", BookmarkSet, "set\u{2026}", C, bookmark()),
        bind("shift-f", BookmarkForget, "forget", C, bookmark()),
        bind("i", AppAction::Interdiff, "interdiff", C, bookmark()),
    ]);
    specs.extend(undo_redo(bookmark));

    // Tag view
    let tag = || views(&[Tags]);
    specs.extend([
        bind("d", TagDelete, "delete", C, tag()),
        bind("t", TagTrack, "track", C, tag()),
        bind("shift-u", TagUntrack, "untrack", C, tag()),
        bind("s", TagSet, "set\u{2026}", C, tag()),
        bind("e", Edit, "edit (checkout)", C, tag()),
        bind("enter", JumpToCommit, "jump to commit", C, tag()),
    ]);
    specs.extend(undo_redo(tag));

    // Operations view
    let op = || views(&[Operations]);
    specs.extend([
        bind("ctrl-f", OpLogFilterWorkspace, "filter workspace", C, op()),
        bind("x", OpLogAbandon, "abandon op", C, op()),
        bind("r", OpLogRevert, "revert op", C, op()),
        bind("shift-r", OpLogRestore, "restore to op", C, op()),
    ]);
    specs.extend(undo_redo(op));

    // Workspaces view
    let workspace = || views(&[Workspaces]);
    specs.extend([
        bind("a", WorkspaceAdd, "add", C, workspace()),
        bind("f", WorkspaceForget, "forget", C, workspace()),
        bind("x", WorkspaceRemove, "remove (deletes it)", C, workspace()),
        bind("r", WorkspaceRename, "rename", C, workspace()),
        bind("enter", JumpToCommit, "jump to commit", C, workspace()),
    ]);
    specs.extend(undo_redo(workspace));

    // Evolog view
    let evo = || views(&[Evolog]);
    specs.extend([
        bind("ctrl-e", SwitchToEvoLogView, "evolog", C, evo()),
        bind("d", AppAction::Interdiff, "interdiff vs current", C, evo()),
        bind("r", EvoLogRestore, "restore from", C, evo()),
        bind("e", Edit, "edit (checkout)", C, evo()),
        bind("n", New, "new from", C, evo()),
    ]);
    specs.extend(file_prefix_bindings(evo));

    // Command log (shared bindings only, no extra specs needed)

    // Interdiff view
    let id = || views(&[ActiveView::Interdiff]);
    specs.extend(file_prefix_bindings(id));

    // Annotate view
    let ann = || views(&[Annotate]);
    specs.extend([
        bind("enter", JumpToCommit, "go to commit", C, ann()),
        bind("b", AnnotateTimeTravel, "blame at this commit", C, ann()),
        bind("f", AnnotateForward, "forward (undo blame)", C, ann()),
        prefix("e", "edit", C, ann()),
        bind("e e", EditFileWorkingCopy, "working copy", C, ann()),
        bind("e r", EditFileAtRevision, "view at revision", C, ann()),
        bind("e c", CheckoutAndEditFile, "checkout and edit", C, ann()),
    ]);

    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Target- and commit-select resolve keys through the keymap and admit
    /// only cursor movement. Guards the class of bug where a mode grows its
    /// own key table and quietly omits half the movement bindings.
    #[test]
    fn select_modes_admit_movement_keys_only() {
        use crate::keymap::{ActionRegistry, Keymaps, LookupResult, try_parse_key};

        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let keymap = keymaps.for_view(ActiveView::Dag);
        let admits = |key: &str| {
            let node = try_parse_key(key).unwrap_or_else(|| panic!("unparsable key: {key}"));
            match keymap.lookup(&node) {
                LookupResult::Action(ActionId::Builtin(action)) => {
                    action.spec().effect == crate::keymap::Effect::Navigate
                }
                _ => false,
            }
        };

        for key in [
            "j", "k", "down", "up", "shift-j", "shift-k", "ctrl-d", "ctrl-u", "pagedown", "pageup",
            "shift-h", "shift-m", "shift-l", "h", "l", "left", "right", "0", "$", "@", "tab", "'",
            "/", "ctrl-n", "ctrl-p", "?",
        ] {
            assert!(admits(key), "{key} should move the cursor in select modes");
        }

        // Mutations, selection changes and mode switches stay out; `x` is a
        // prefix in the DAG view, standing in for sequence bindings.
        for key in ["space", "v", ":", "ctrl-r", "x"] {
            assert!(!admits(key), "{key} must not act in select modes");
        }
    }

    /// The jump overlay labels a reachable row with the key that reaches it,
    /// so the labels have to be read from the bindings rather than assumed.
    #[test]
    fn jump_labels_read_the_bindings() {
        use crate::keymap::{ActionRegistry, Keymaps};

        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let keymap = keymaps.for_view(ActiveView::Dag);
        let label = |action| keymap.typeable_keys(action);

        assert_eq!(label(AppAction::MoveDown).as_deref(), Some("j"));
        assert_eq!(label(AppAction::MoveUpSection).as_deref(), Some("K"));
        assert_eq!(label(AppAction::MoveToScreenTop).as_deref(), Some("H"));
        assert_eq!(label(AppAction::MoveToScreenMiddle).as_deref(), Some("M"));
        assert_eq!(label(AppAction::MoveToScreenBottom).as_deref(), Some("L"));
        assert_eq!(label(AppAction::MoveToTop).as_deref(), Some("0"));
        assert_eq!(label(AppAction::JumpToWorkingCopy).as_deref(), Some("@"));
        // A sequence binding labels as the whole sequence.
        assert_eq!(label(AppAction::NextConflict).as_deref(), Some("]c"));
        // ctrl-d and pagedown are both unreadable as typed text.
        assert_eq!(label(AppAction::PageDown), None);
        // Conflict navigation is DAG-only, so other views offer no label.
        assert_eq!(
            keymaps
                .for_view(ActiveView::Bookmarks)
                .typeable_keys(AppAction::NextConflict),
            None
        );
    }

    /// The select-mode help and the select-mode key handling read the same
    /// keymap through the same predicate, so the listing tracks the keys.
    #[test]
    fn select_mode_help_lists_the_keys_that_work() {
        use crate::keymap::{ActionRegistry, Keymaps, select_mode_help_entries};

        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let listed: Vec<String> = select_mode_help_entries(keymaps.for_view(ActiveView::Dag))
            .into_iter()
            .flat_map(|(_, entries)| entries)
            .map(|e| e.keys)
            .collect();
        let lists = |key: &str| listed.iter().any(|k| k.split(" / ").any(|k| k == key));

        for key in ["H", "M", "L", "h", "l", "j", "k", "@", "0", "$", "?"] {
            assert!(lists(key), "select-mode help should list {key}: {listed:?}");
        }
        assert!(lists("Enter") && lists("Esc"));
        for key in ["v", ":", "ctrl-r"] {
            assert!(!lists(key), "select-mode help must not list {key}");
        }
    }

    #[test]
    fn a_rebound_movement_key_relabels_its_jump_target() {
        use crate::keymap::{ActionRegistry, Keymaps};
        use HelpGroup::Navigation as N;

        let specs = vec![bind(
            "t",
            AppAction::MoveToScreenTop,
            "screen top",
            N,
            Scope::All,
        )];
        let keymaps = Keymaps::build(specs, ActionRegistry::new());
        assert_eq!(
            keymaps
                .for_view(ActiveView::Dag)
                .typeable_keys(AppAction::MoveToScreenTop)
                .as_deref(),
            Some("t")
        );
    }
}
