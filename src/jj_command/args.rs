use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use compact_str::format_compact;
use jj_lib::config::ConfigValue;

use crate::conflict::MARKER_STYLE_CONFIG;
use crate::jj_version::JjFeature;
use crate::keymap::CommandFlags;
use crate::types::{ChangeSelection, GLOBAL_TOGGLES, Str};

use super::{CommandPartKind, JJCommand, JJCommandKind, MetaeditChange, ResolveTool};

/// A CLI argument together with its display kind, assigned at construction
/// so the command log never has to guess what an argument is, and the jj
/// feature it needs when it is newer than the oldest jj kojutsu supports.
/// Dated where it is spelled, so the two can't drift apart.
pub(super) struct TaggedArg {
    pub(super) text: Str,
    pub(super) kind: CommandPartKind,
    pub(super) needs: Option<JjFeature>,
    /// A positional operand rather than an option or an option's value.
    /// Names from the repository are operands, and one beginning with `-`
    /// would read as an option, so `finish` ends jj's options before the
    /// first operand when any looks like one.
    pub(super) operand: bool,
}

impl TaggedArg {
    fn new(text: impl Into<Str>, kind: CommandPartKind) -> Self {
        Self {
            text: text.into(),
            kind,
            needs: None,
            operand: false,
        }
    }

    fn operand(mut self) -> Self {
        self.operand = true;
        self
    }

    fn needs(mut self, feature: JjFeature) -> Self {
        self.needs = Some(feature);
        self
    }
}

fn sub(name: &'static str) -> TaggedArg {
    TaggedArg::new(name, CommandPartKind::Subcommand)
}

fn flag(text: impl Into<Str>) -> TaggedArg {
    TaggedArg::new(text, CommandPartKind::Flag)
}

/// A revision-like identifier: change/commit/op IDs and bookmark/tag names
/// (which resolve to revisions and share their color in the theme).
fn rev(id: impl std::fmt::Display) -> TaggedArg {
    TaggedArg::new(format_compact!("{id}"), CommandPartKind::Revision)
}

/// A flag a [`CommandFlags`] bit turns on.
struct FlagOption {
    bit: CommandFlags,
    spelling: &'static str,
    needs: Option<JjFeature>,
}

const fn opt(bit: CommandFlags, spelling: &'static str) -> FlagOption {
    FlagOption {
        bit,
        spelling,
        needs: None,
    }
}

impl FlagOption {
    const fn needs(mut self, feature: JjFeature) -> Self {
        self.needs = Some(feature);
        self
    }
}

/// The options every kind of `jj git push` takes.
const PUSH_FLAGS: &[FlagOption] = &[
    opt(CommandFlags::DRY_RUN, "--dry-run"),
    opt(CommandFlags::ALLOW_CONFLICTS, "--allow-conflicts").needs(JjFeature::PushAllowConflicts),
    opt(
        CommandFlags::ALLOW_EMPTY_DESCRIPTION,
        "--allow-empty-description",
    ),
];

/// A bookmark or tag on one remote, as the `name@remote` symbol jj resolves
/// exactly, quoted where the name or remote needs it.
fn remote_symbol(name: &str, remote: &crate::types::RemoteName) -> TaggedArg {
    rev(jj_lib::revset::format_remote_symbol(name, remote.as_str()))
}

/// A bookmark or tag name where jj takes exactly one name, quoted when it
/// isn't a bare revset identifier (`"🚀-launch"`): jj rejects it otherwise.
fn name_symbol(name: &str) -> TaggedArg {
    rev(jj_lib::revset::format_symbol(name))
}

/// A bookmark or tag name where jj takes a string pattern.
fn name_pattern(name: &str) -> TaggedArg {
    rev(exact_pattern(name))
}

/// `name` as a string pattern that matches only itself. Patterns glob by
/// default, so a name that needs quoting, or contains a `*` (which is legal
/// in a bare identifier), is spelled `exact:`.
fn exact_pattern(name: &str) -> Str {
    let symbol = jj_lib::revset::format_symbol(name);
    if symbol == name && !name.contains('*') {
        symbol.into()
    } else {
        format_compact!("exact:{symbol}")
    }
}

/// The options `jj file edit` and `jj file delete` both take.
const FILE_REWRITE_FLAGS: &[FlagOption] = &[opt(
    CommandFlags::RESTORE_DESCENDANTS,
    "--restore-descendants",
)];

/// The options `jj undo` and `jj redo` both take.
const UNDO_FLAGS: &[FlagOption] = &[opt(
    CommandFlags::ALLOW_CROSS_WORKSPACE,
    "--allow-cross-workspace",
)
.needs(JjFeature::UndoCrossWorkspace)];

/// A plain value: message, path, count, remote or workspace name.
fn arg(text: impl Into<Str>) -> TaggedArg {
    TaggedArg::new(text, CommandPartKind::String)
}

fn url_arg(text: impl Into<Str>) -> TaggedArg {
    TaggedArg::new(text, CommandPartKind::Url)
}

impl JJCommand {
    pub fn args(&self) -> Vec<Str> {
        self.tagged_args()
            .into_iter()
            .map(|TaggedArg { text, kind, .. }| {
                if kind == CommandPartKind::Fileset {
                    exact_file_fileset(&text)
                } else {
                    text
                }
            })
            .collect()
    }

    pub(super) fn tagged_args(&self) -> Vec<TaggedArg> {
        self.tagged_args_with(self_exe())
    }

    /// The args with `exe` as the binary jj runs for the self-invoking
    /// tools; `None` leaves those tools unconfigured.
    pub(super) fn tagged_args_with(&self, exe: Option<&Path>) -> Vec<TaggedArg> {
        let flags = self.flags;
        let globals = flags & self.global_flags();
        let mut args = match &self.kind {
            JJCommandKind::Abandon { change_ids, .. } => {
                let mut args = vec![sub("abandon")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        opt(CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::Describe {
                change_ids,
                message,
                ..
            } => {
                let mut args = vec![sub("describe"), flag("-m"), arg(message.as_str())];
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::DescribeInEditor { change_id, .. } => {
                vec![sub("describe"), rev(change_id).operand()]
            }
            JJCommandKind::Metaedit { change_ids, change } => {
                let mut args = vec![sub("metaedit")];
                match change {
                    MetaeditChange::NewChangeId => args.push(flag("--update-change-id")),
                    MetaeditChange::ResetAuthor => args.push(flag("--update-author")),
                    MetaeditChange::Author(author) => {
                        args.push(flag("--author"));
                        args.push(arg(author.as_str()));
                    }
                    MetaeditChange::AuthorTimestampNow => {
                        args.push(flag("--update-author-timestamp"));
                    }
                    MetaeditChange::AuthorTimestamp(timestamp) => {
                        args.push(flag("--author-timestamp"));
                        args.push(arg(timestamp.as_str()));
                    }
                }
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::Diffedit {
                change_id,
                selection,
                ..
            } => {
                let mut args = vec![sub("diffedit"), flag("-r"), rev(change_id)];
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::Edit { change_id, .. } => {
                vec![sub("edit"), rev(change_id).operand()]
            }
            JJCommandKind::New {
                change_ids, insert, ..
            } => {
                let mut args = vec![sub("new")];
                if let Some(pos) = insert {
                    args.push(flag(pos.flag()));
                }
                push_flags(&mut args, flags, &[opt(CommandFlags::NO_EDIT, "--no-edit")]);
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::Rebase {
                change_ids,
                source_mode,
                dest,
                ..
            } => {
                let mut args = vec![sub("rebase")];
                let source_flag = match source_mode {
                    crate::types::RebaseSource::Revision => "-r",
                    crate::types::RebaseSource::Source => "-s",
                    crate::types::RebaseSource::Branch => "-b",
                };
                for id in change_ids {
                    args.push(flag(source_flag));
                    args.push(rev(id));
                }
                for target in &dest.targets {
                    args.push(flag(dest.kind.flag()));
                    args.push(rev(target));
                }
                args
            }
            JJCommandKind::Restore {
                from,
                into,
                changes_in,
                selection,
                ..
            } => {
                let mut args = vec![sub("restore")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::INTERACTIVE, "--interactive"),
                        opt(CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                if let Some(id) = from {
                    args.push(flag("--from"));
                    args.push(rev(id));
                }
                if let Some(id) = into {
                    args.push(flag("--into"));
                    args.push(rev(id));
                }
                if let Some(id) = changes_in {
                    args.push(flag("--changes-in"));
                    args.push(rev(id));
                }
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::Split {
                change_id,
                target,
                selection,
                ..
            } => {
                let mut args = vec![sub("split"), flag("-r"), rev(change_id)];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::INTERACTIVE, "--interactive"),
                        opt(CommandFlags::PARALLEL, "--parallel"),
                    ],
                );
                if let Some(t) = target {
                    args.push(flag(t.kind.flag()));
                    args.push(rev(&t.target));
                }
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::BookmarkCreate {
                name, change_id, ..
            } => {
                vec![
                    sub("bookmark"),
                    sub("create"),
                    flag("-r"),
                    rev(change_id),
                    name_symbol(name.as_str()).operand(),
                ]
            }
            JJCommandKind::BookmarkSet {
                name, change_id, ..
            } => {
                let mut args = vec![sub("bookmark"), sub("set")];
                push_flags(
                    &mut args,
                    flags,
                    &[opt(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push(flag("-r"));
                args.push(rev(change_id));
                args.push(name_symbol(name.as_str()).operand());
                args
            }
            JJCommandKind::BookmarkDelete { names, .. } => {
                let mut args = vec![sub("bookmark"), sub("delete")];
                args.extend(names.iter().map(|n| name_pattern(n.as_str()).operand()));
                args
            }
            JJCommandKind::BookmarkForget { names, .. } => {
                let mut args = vec![sub("bookmark"), sub("forget")];
                args.extend(names.iter().map(|n| name_pattern(n.as_str()).operand()));
                args
            }
            JJCommandKind::BookmarkMove { name, target, .. } => {
                let mut args = vec![sub("bookmark"), sub("move")];
                push_flags(
                    &mut args,
                    flags,
                    &[opt(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push(flag("--to"));
                args.push(rev(target));
                args.push(name_pattern(name.as_str()).operand());
                args
            }
            JJCommandKind::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    sub("bookmark"),
                    sub("rename"),
                    name_symbol(old_name.as_str()).operand(),
                    name_symbol(new_name.as_str()).operand(),
                ]
            }
            JJCommandKind::BookmarkAdvance { change_id, .. } => {
                let mut args = vec![
                    sub("bookmark"),
                    sub("advance").needs(JjFeature::BookmarkAdvance),
                ];
                if let Some(id) = change_id {
                    args.push(flag("--to"));
                    args.push(rev(id));
                }
                args
            }
            // Exact `name@remote` symbols: the names and `--remote` values
            // are separate lists to jj, which tracks every pairing of them.
            JJCommandKind::BookmarkTrack { bookmarks, .. } => {
                let mut args = vec![sub("bookmark"), sub("track")];
                args.extend(
                    bookmarks
                        .iter()
                        .map(|b| remote_symbol(b.name.as_str(), &b.remote).operand()),
                );
                args
            }
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => {
                let mut args = vec![sub("bookmark"), sub("untrack")];
                args.extend(
                    bookmarks
                        .iter()
                        .map(|b| remote_symbol(b.name.as_str(), &b.remote).operand()),
                );
                args
            }
            JJCommandKind::Undo => {
                let mut args = vec![sub("undo")];
                push_flags(&mut args, flags, UNDO_FLAGS);
                args
            }
            JJCommandKind::Redo => {
                let mut args = vec![sub("redo")];
                push_flags(&mut args, flags, UNDO_FLAGS);
                args
            }
            JJCommandKind::GitFetch {
                all_remotes,
                remotes,
                ..
            } => {
                let mut args = vec![sub("git"), sub("fetch")];
                if *all_remotes {
                    args.push(flag("--all-remotes"));
                }
                push_remotes(&mut args, remotes, None);
                args
            }
            JJCommandKind::GitPush { all, remotes, .. } => {
                let mut args = vec![sub("git"), sub("push")];
                if *all {
                    args.push(flag("--all"));
                }
                push_remotes(&mut args, remotes, Some(JjFeature::PushToRemotes));
                push_flags(&mut args, flags, PUSH_FLAGS);
                args
            }
            JJCommandKind::GitPushChange {
                change_id, remotes, ..
            } => {
                let mut args = vec![sub("git"), sub("push"), flag("-c"), rev(change_id)];
                push_remotes(&mut args, remotes, Some(JjFeature::PushToRemotes));
                push_flags(&mut args, flags, PUSH_FLAGS);
                args
            }
            JJCommandKind::GitPushBookmark {
                bookmarks, remotes, ..
            } => {
                let mut args = vec![sub("git"), sub("push")];
                for name in bookmarks {
                    args.push(flag("--bookmark"));
                    args.push(name_pattern(name.as_str()));
                }
                push_remotes(&mut args, remotes, Some(JjFeature::PushToRemotes));
                push_flags(&mut args, flags, PUSH_FLAGS);
                args
            }
            JJCommandKind::GitFetchBookmark {
                bookmark, remote, ..
            } => {
                vec![
                    sub("git"),
                    sub("fetch"),
                    flag("-b"),
                    name_pattern(bookmark.as_str()),
                    flag("--remote"),
                    arg(exact_pattern(remote.as_str())),
                ]
            }
            JJCommandKind::GitPushTag { tags, remotes } => {
                let mut args = vec![sub("git"), sub("push")];
                for name in tags {
                    args.push(flag("--tag").needs(JjFeature::TagPush));
                    args.push(name_pattern(name.as_str()));
                }
                push_remotes(&mut args, remotes, Some(JjFeature::PushToRemotes));
                push_flags(&mut args, flags, PUSH_FLAGS);
                args
            }
            JJCommandKind::GitFetchTag { tag, remotes } => {
                let mut args = vec![
                    sub("git"),
                    sub("fetch"),
                    flag("--tag").needs(JjFeature::TagFetch),
                    name_pattern(tag.as_str()),
                ];
                push_remotes(&mut args, remotes, None);
                args
            }
            JJCommandKind::GitExport => vec![sub("git"), sub("export")],
            JJCommandKind::GitImport => vec![sub("git"), sub("import")],
            JJCommandKind::GitRemoteAdd { name, url } => vec![
                sub("git"),
                sub("remote"),
                sub("add"),
                arg(name.as_str()).operand(),
                url_arg(url.as_str()).operand(),
            ],
            JJCommandKind::GitRemoteRemove { name } => {
                vec![
                    sub("git"),
                    sub("remote"),
                    sub("remove"),
                    arg(name.as_str()).operand(),
                ]
            }
            JJCommandKind::GitRemoteRename { old, new } => vec![
                sub("git"),
                sub("remote"),
                sub("rename"),
                arg(old.as_str()).operand(),
                arg(new.as_str()).operand(),
            ],
            JJCommandKind::GitRemoteSetUrl { name, url, push } => {
                let mut args = vec![sub("git"), sub("remote"), sub("set-url")];
                // The bare URL sets where it fetches from, and, with no
                // separate push URL, where it pushes to as well.
                if *push {
                    args.push(flag("--push").needs(JjFeature::SeparatePushUrl));
                }
                args.push(arg(name.as_str()).operand());
                args.push(url_arg(url.as_str()).operand());
                args
            }
            JJCommandKind::GitColocation {
                command,
                secondary_workspace,
            } => {
                let colocation = sub("colocation");
                vec![
                    sub("git"),
                    if *secondary_workspace {
                        colocation.needs(JjFeature::ColocationInWorkspaces)
                    } else {
                        colocation
                    },
                    sub(command.subcommand()),
                ]
            }
            JJCommandKind::Absorb {
                from, selection, ..
            } => {
                let mut args = vec![sub("absorb")];
                if let Some(id) = from {
                    args.push(flag("--from"));
                    args.push(rev(id));
                }
                push_change_selection_through(
                    &mut args,
                    selection,
                    flag("--interactive").needs(JjFeature::AbsorbLines),
                    exe,
                );
                args
            }
            JJCommandKind::Commit {
                message, selection, ..
            } => {
                let mut args = vec![sub("commit")];
                push_flags(
                    &mut args,
                    flags,
                    &[opt(CommandFlags::INTERACTIVE, "--interactive")],
                );
                if let Some(msg) = message {
                    args.push(flag("-m"));
                    args.push(arg(msg.as_str()));
                }
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::Parallelize { change_ids, .. } => {
                let mut args = vec![sub("parallelize")];
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::SimplifyParents { change_ids, .. } => {
                let mut args = vec![sub("simplify-parents")];
                for id in change_ids {
                    args.push(flag("-r"));
                    args.push(rev(id));
                }
                args
            }
            JJCommandKind::Revert {
                change_ids, dest, ..
            } => {
                let mut args = vec![sub("revert")];
                for id in change_ids {
                    args.push(flag("-r"));
                    args.push(rev(id));
                }
                for target in &dest.targets {
                    args.push(flag(dest.kind.flag()));
                    args.push(rev(target));
                }
                args
            }
            JJCommandKind::Duplicate {
                change_ids, onto, ..
            } => {
                let mut args = vec![sub("duplicate")];
                if let Some(target) = onto {
                    args.push(flag("--onto"));
                    args.push(rev(target));
                }
                args.extend(change_ids.iter().map(|id| rev(id).operand()));
                args
            }
            JJCommandKind::Squash {
                change_id,
                target,
                message,
                selection,
                ..
            } => {
                let mut args = vec![sub("squash")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::INTERACTIVE, "--interactive"),
                        opt(CommandFlags::KEEP_EMPTIED, "--keep-emptied"),
                    ],
                );
                match message {
                    crate::types::MessageMode::Default => {}
                    crate::types::MessageMode::Inline(msg) => {
                        args.push(flag("-m"));
                        args.push(arg(msg.as_str()));
                    }
                    crate::types::MessageMode::UseDestination => {
                        args.push(flag("--use-destination-message"));
                    }
                }
                match target {
                    None => {
                        args.push(flag("-r"));
                        args.push(rev(change_id));
                    }
                    Some(t) => {
                        args.push(flag("--from"));
                        args.push(rev(change_id));
                        args.push(flag(t.kind.flag()));
                        args.push(rev(&t.target));
                    }
                }
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::WorkspaceAdd {
                path,
                name,
                revision,
                ..
            } => {
                let mut args = vec![sub("workspace"), sub("add")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::COLOCATE, "--colocate")
                            .needs(JjFeature::WorkspaceColocation),
                        opt(CommandFlags::NO_COLOCATE, "--no-colocate")
                            .needs(JjFeature::WorkspaceColocation),
                    ],
                );
                if let Some(n) = name {
                    args.push(flag("--name"));
                    args.push(arg(n.as_str()));
                }
                args.push(flag("-r"));
                args.push(rev(revision));
                args.push(arg(path.as_str()).operand());
                args
            }
            JJCommandKind::WorkspaceForget { names, .. } => {
                let mut args = vec![sub("workspace"), sub("forget")];
                args.extend(names.iter().map(|n| arg(n.as_str()).operand()));
                args
            }
            JJCommandKind::WorkspaceRemove { names, .. } => {
                let mut args = vec![
                    sub("workspace"),
                    sub("remove").needs(JjFeature::WorkspaceRemove),
                ];
                args.extend(names.iter().map(|n| arg(n.as_str()).operand()));
                args
            }
            JJCommandKind::WorkspaceList => {
                vec![sub("workspace"), sub("list")]
            }
            JJCommandKind::WorkspaceRename { new_name, .. } => {
                vec![
                    sub("workspace"),
                    sub("rename"),
                    arg(new_name.as_str()).operand(),
                ]
            }
            JJCommandKind::TagSet {
                name, change_id, ..
            } => {
                let mut args = vec![sub("tag"), sub("set")];
                push_flags(
                    &mut args,
                    flags,
                    &[opt(CommandFlags::ALLOW_MOVE, "--allow-move")],
                );
                args.push(flag("-r"));
                args.push(rev(change_id));
                args.push(name_symbol(name.as_str()).operand());
                args
            }
            JJCommandKind::TagDelete { names, .. } => {
                let mut args = vec![sub("tag"), sub("delete")];
                args.extend(names.iter().map(|n| name_pattern(n.as_str()).operand()));
                args
            }
            JJCommandKind::Converge { changes } => {
                let mut args = vec![sub("converge").needs(JjFeature::Converge)];
                for change in changes {
                    args.push(flag("-r"));
                    args.push(rev(format_compact!("change_id({change})")));
                }
                push_flags(
                    &mut args,
                    flags,
                    &[opt(CommandFlags::NO_INTERACTIVE, "--no-interactive")],
                );
                args
            }
            JJCommandKind::TagTrack { tags } => {
                let mut args = vec![sub("tag"), sub("track").needs(JjFeature::TagTracking)];
                args.extend(
                    tags.iter()
                        .map(|t| remote_symbol(t.name.as_str(), &t.remote).operand()),
                );
                args
            }
            JJCommandKind::TagUntrack { tags } => {
                let mut args = vec![sub("tag"), sub("untrack").needs(JjFeature::TagTracking)];
                args.extend(
                    tags.iter()
                        .map(|t| remote_symbol(t.name.as_str(), &t.remote).operand()),
                );
                args
            }
            JJCommandKind::OpRestore { op_id, .. } => {
                vec![sub("op"), sub("restore"), rev(op_id.as_str()).operand()]
            }
            JJCommandKind::OpRevert { op_id, .. } => {
                vec![sub("op"), sub("revert"), rev(op_id.as_str()).operand()]
            }
            JJCommandKind::OpAbandon { op_id, .. } => {
                vec![sub("op"), sub("abandon"), rev(op_id.as_str()).operand()]
            }
            JJCommandKind::Fix {
                change_ids,
                selection,
                ..
            } => {
                let mut args = vec![sub("fix")];
                for id in change_ids {
                    args.push(flag("-s"));
                    args.push(rev(id));
                }
                push_change_selection(&mut args, selection, exe);
                args
            }
            JJCommandKind::Run {
                change_ids,
                argv,
                jobs,
            } => {
                let mut args = vec![sub("run").needs(JjFeature::Run)];
                for id in change_ids {
                    args.push(flag("-r"));
                    args.push(rev(id));
                }
                if let Some(jobs) = jobs {
                    args.push(flag("--jobs"));
                    args.push(arg(format_compact!("{jobs}")));
                }
                push_flags(
                    &mut args,
                    flags,
                    &[
                        opt(CommandFlags::CLEAN, "--clean"),
                        opt(CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                        opt(CommandFlags::PASSTHROUGH, "--passthrough")
                            .needs(JjFeature::RunPassthrough),
                        opt(CommandFlags::IGNORE_CHANGES, "--ignore-changes")
                            .needs(JjFeature::RunIgnoreChanges),
                        opt(CommandFlags::IGNORE_ERRORS, "--ignore-errors")
                            .needs(JjFeature::RunIgnoreErrors),
                    ],
                );
                args.push(flag("--"));
                args.extend(argv.iter().cloned().map(|a| arg(a).operand()));
                args
            }
            JJCommandKind::FileUntrack { paths, .. } => {
                let mut args = vec![sub("file"), sub("untrack")];
                args.extend(paths.iter().map(|p| fileset_arg(p)));
                args
            }
            JJCommandKind::FileEdit { change_id, path } => {
                let mut args = vec![
                    sub("file"),
                    sub("edit").needs(JjFeature::FileEdit),
                    flag("-r"),
                    rev(change_id),
                ];
                push_flags(&mut args, flags, FILE_REWRITE_FLAGS);
                // A path, not a fileset: jj would take the quotes as part of
                // the name.
                args.push(arg(path.clone()).operand());
                args
            }
            JJCommandKind::FileDelete { change_id, paths } => {
                let mut args = vec![
                    sub("file"),
                    sub("delete").needs(JjFeature::FileDelete),
                    flag("-r"),
                    rev(change_id),
                ];
                push_flags(&mut args, flags, FILE_REWRITE_FLAGS);
                args.extend(paths.iter().map(|p| fileset_arg(p)));
                args
            }
            JJCommandKind::Resolve {
                change_id,
                path,
                tool,
                ..
            } => {
                let mut args = vec![sub("resolve"), flag("-r"), rev(change_id)];
                match tool {
                    ResolveTool::Ours => args.push(flag("--tool=:ours")),
                    ResolveTool::Theirs => args.push(flag("--tool=:theirs")),
                    ResolveTool::Default => {}
                    ResolveTool::Content(content_path) => {
                        args.extend(self_invoking_tool(APPLY_TOOL, exe));
                        args.extend([
                            flag("--config"),
                            arg(format!(
                                "merge-tools.kojutsu-apply.merge-args={}",
                                toml_value(ConfigValue::from_iter([
                                    "--apply-resolution",
                                    &content_path.path().display().to_string(),
                                    "$output",
                                ]))
                            )),
                            // Partially picked files keep conflict markers;
                            // jj parses them back into a conflicted state,
                            // so it has to read them the way we wrote them.
                            flag("--config"),
                            arg("merge-tools.kojutsu-apply.merge-tool-edits-conflict-markers=true"),
                            flag("--config"),
                            arg(format!(
                                "merge-tools.kojutsu-apply.conflict-marker-style={}",
                                toml_value(MARKER_STYLE_CONFIG)
                            )),
                        ]);
                    }
                }
                args.push(fileset_arg(path));
                args
            }
            JJCommandKind::Raw { args } => lex_raw_args(args),
            // Not a jj command line, so there is no grammar to lex: the
            // program names itself in the log and the rest are plain values.
            JJCommandKind::Exec { args, .. } => {
                return args
                    .iter()
                    .map(|arg| {
                        let kind = if arg.starts_with('-') {
                            CommandPartKind::Flag
                        } else {
                            CommandPartKind::String
                        };
                        TaggedArg::new(arg.clone(), kind)
                    })
                    .collect();
            }
        };

        finish(&mut args, globals);
        args
    }
}

/// Every jj command line passes through here last. jj reads a bare `--`
/// as the end of its options, so an operand that begins with `-` gets one
/// put before the first operand, and the global toggles go ahead of any
/// `--`, since past it jj hands words on verbatim. Options must precede
/// operands for the `--` to land right; the builders keep that order.
fn finish(args: &mut Vec<TaggedArg>, globals: CommandFlags) {
    let is_dashes = |a: &TaggedArg| a.kind == CommandPartKind::Flag && a.text == "--";
    let options_end = args.iter().position(is_dashes).unwrap_or(args.len());
    let operands = &args[..options_end];
    if let Some(first) = operands.iter().position(|a| a.operand) {
        debug_assert!(
            operands[first..]
                .iter()
                .all(|a| a.kind != CommandPartKind::Flag),
            "an option after an operand: {:?}",
            operands.iter().map(|a| a.text.as_str()).collect::<Vec<_>>()
        );
        if operands[first..]
            .iter()
            .any(|a| a.operand && a.text.starts_with('-'))
        {
            args.insert(first, flag("--"));
        }
    }
    let at = args.iter().position(is_dashes).unwrap_or(args.len());
    let mut toggles = Vec::new();
    push_global_flags(&mut toggles, globals);
    args.splice(at..at, toggles);
}

/// Raw commands are user-typed, so argument kinds cannot be known at
/// construction. Lex them from jj's factual grammar: subcommand word(s),
/// `-`-prefixed flags, and verbatim passthrough after a bare `--`, without
/// guessing which values are revisions.
fn lex_raw_args(args: &[Str]) -> Vec<TaggedArg> {
    const COMPOUND_SUBCOMMANDS: &[&str] = &["git", "bookmark", "workspace", "tag", "op", "file"];

    let mut parts: Vec<TaggedArg> = Vec::with_capacity(args.len());
    let mut rest = args;
    if let Some((first, tail)) = rest.split_first() {
        parts.push(TaggedArg::new(first.clone(), CommandPartKind::Subcommand));
        rest = tail;
        if COMPOUND_SUBCOMMANDS.contains(&first.as_str())
            && let Some((second, tail)) = rest.split_first()
            && !second.starts_with('-')
        {
            parts.push(TaggedArg::new(second.clone(), CommandPartKind::Subcommand));
            rest = tail;
        }
    }
    let mut passthrough = false;
    for a in rest {
        let kind = if passthrough {
            CommandPartKind::String
        } else if a == "--" {
            passthrough = true;
            CommandPartKind::Flag
        } else if a.starts_with('-') {
            CommandPartKind::Flag
        } else {
            CommandPartKind::String
        };
        parts.push(TaggedArg::new(a.clone(), kind));
    }
    parts
}

fn push_flags(args: &mut Vec<TaggedArg>, flags: CommandFlags, mapping: &[FlagOption]) {
    for option in mapping {
        if flags.contains(option.bit) {
            let arg = flag(option.spelling);
            args.push(match option.needs {
                Some(feature) => arg.needs(feature),
                None => arg,
            });
        }
    }
}

/// A `--remote` for each of `remotes`, spelled as an exact pattern since
/// jj globs the value. A command that took only one until `several` came
/// needs it from the second on.
fn push_remotes(
    args: &mut Vec<TaggedArg>,
    remotes: &[crate::types::RemoteName],
    several: Option<JjFeature>,
) {
    for (i, remote) in remotes.iter().enumerate() {
        let remote_flag = flag("--remote");
        args.push(match several {
            Some(feature) if i > 0 => remote_flag.needs(feature),
            _ => remote_flag,
        });
        args.push(arg(exact_pattern(remote.as_str())));
    }
}

fn push_change_selection(
    args: &mut Vec<TaggedArg>,
    selection: &ChangeSelection,
    exe: Option<&Path>,
) {
    push_change_selection_through(args, selection, flag("--interactive"), exe);
}

/// [`push_change_selection`], with the flag that opens the command's diff
/// editor for a line selection given by the caller, for a command that
/// gained it later than the rest.
fn push_change_selection_through(
    args: &mut Vec<TaggedArg>,
    selection: &ChangeSelection,
    interactive: TaggedArg,
    exe: Option<&Path>,
) {
    match selection {
        ChangeSelection::All => {}
        ChangeSelection::Files(paths) => {
            args.extend(paths.iter().map(|p| fileset_arg(p)));
        }
        ChangeSelection::Lines(json_path) => {
            args.push(interactive);
            args.extend(self_invoking_tool(SELECT_TOOL, exe));
            args.extend([
                flag("--config"),
                arg(format!(
                    "merge-tools.kojutsu-select.edit-args={}",
                    toml_value(ConfigValue::from_iter([
                        "--apply-diff",
                        &json_path.path().display().to_string(),
                        "$left",
                        "$right",
                    ]))
                )),
                // Selections carry line numbers computed against kojutsu's
                // materialization; pin jj's tool-side materialization to the
                // same style so lines align in conflicted files.
                flag("--config"),
                arg(format!(
                    "ui.conflict-marker-style={}",
                    toml_value(MARKER_STYLE_CONFIG)
                )),
            ]);
        }
    }
}

/// The merge tools that re-invoke the running kojutsu binary.
pub(super) const SELECT_TOOL: &str = "kojutsu-select";
pub(super) const APPLY_TOOL: &str = "kojutsu-apply";

/// Where this binary is, for jj to run it as a tool. Resolved once. `None`
/// means the self-invoking tools cannot be configured; their commands are
/// refused rather than left to jj's `PATH` lookup of a bare name.
pub(super) fn self_exe() -> Option<&'static Path> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().ok()).as_deref()
}

/// Configure a merge tool that re-invokes the running kojutsu binary:
/// `--tool <name> --config merge-tools.<name>.program=<exe>`. Callers
/// append the tool-specific `*-args` and any extra configs. Single home
/// for the injection-sensitive program-path construction. Without `exe`
/// the tool is named but given no program, so jj can only refuse it.
fn self_invoking_tool(name: &str, exe: Option<&Path>) -> Vec<TaggedArg> {
    let mut args = vec![flag("--tool"), arg(name.to_string())];
    if let Some(exe) = exe {
        args.extend([
            flag("--config"),
            arg(format!(
                "merge-tools.{name}.program={}",
                toml_value(exe.display().to_string())
            )),
        ]);
    }
    args
}

fn fileset_arg(path: &str) -> TaggedArg {
    TaggedArg::new(path, CommandPartKind::Fileset)
}

/// A repo path as the fileset matching exactly that file. A bare string
/// literal is a cwd-relative prefix glob to jj, so a path's `*`, `?` or `[`
/// would match other files.
fn exact_file_fileset(path: &str) -> Str {
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    format_compact!("root-file:\"{escaped}\"")
}

fn push_global_flags(args: &mut Vec<TaggedArg>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(flag(toggle.cli_flag));
        }
    }
}

/// `value` spelled as TOML, which is how jj reads a `--config` value, by
/// jj's own encoder, so a path's control characters are escaped too.
fn toml_value(value: impl Into<ConfigValue>) -> String {
    value.into().to_string()
}

#[cfg(test)]
mod fileset_tests {
    use jj_lib::fileset::{FilesetAliasesMap, FilesetDiagnostics, FilesetParseContext};
    use jj_lib::matchers::Matcher;
    use jj_lib::repo_path::RepoPathBuf;
    use jj_lib::ui_path::RepoPathUiConverter;

    use super::*;
    use crate::types::{MessageMode, RevisionArg};

    /// The arguments that went to jj as filesets, as jj receives them.
    fn filesets(cmd: &JJCommand) -> Vec<String> {
        cmd.tagged_args()
            .iter()
            .zip(cmd.args())
            .filter(|(tagged, _)| tagged.kind == CommandPartKind::Fileset)
            .map(|(_, arg)| arg.to_string())
            .collect()
    }

    /// The files `fileset` matches, as jj parses it from the workspace root.
    fn matcher(fileset: &str) -> Box<dyn Matcher> {
        let converter = RepoPathUiConverter::Fs {
            cwd: "/repo".into(),
            base: "/repo".into(),
        };
        let context = FilesetParseContext {
            aliases_map: &FilesetAliasesMap::new(),
            path_converter: &converter,
        };
        jj_lib::fileset::parse(&mut FilesetDiagnostics::new(), fileset, &context)
            .unwrap_or_else(|e| panic!("jj rejects {fileset}: {e}"))
            .to_matcher()
    }

    fn matches(matcher: &dyn Matcher, path: &str) -> bool {
        matcher.matches(&RepoPathBuf::from_internal_string(path).expect("a repo path"))
    }

    /// Each selected path comes back as exactly that file: quotes and
    /// backslashes are escaped, operators are inert, and glob characters,
    /// which jj reads in a bare string literal, name themselves.
    #[test]
    fn a_selected_path_reaches_jj_as_exactly_that_file() {
        let cases = [
            ("src/lib.rs", None),
            ("dir/a file.rs", Some("dir/a")),
            ("say \"hi\".txt", None),
            ("back\\slash.txt", Some("backslash.txt")),
            ("a[1].rs", Some("a1.rs")),
            ("*.rs", Some("lib.rs")),
            ("a?c", Some("abc")),
            ("~not|this&that", Some("not")),
        ];
        let paths = || cases.iter().map(|(path, _)| Str::from(*path));
        let id = || RevisionArg::new("x");
        let commands = [
            JJCommandKind::Squash {
                change_id: id(),
                target: None,
                message: MessageMode::Default,
                selection: ChangeSelection::Files(paths().collect()),
            },
            JJCommandKind::FileUntrack {
                paths: paths().collect(),
            },
            JJCommandKind::FileDelete {
                change_id: id(),
                paths: paths().collect(),
            },
        ]
        .into_iter()
        .map(|kind| JJCommand {
            kind,
            flags: CommandFlags::empty(),
        });
        for cmd in commands {
            let filesets = filesets(&cmd);
            assert_eq!(filesets.len(), cases.len(), "{:?}", cmd.args());
            for ((path, decoy), fileset) in cases.iter().zip(&filesets) {
                let matcher = matcher(fileset);
                assert!(matches(&*matcher, path), "{fileset} misses {path:?}");
                if let Some(decoy) = decoy {
                    assert!(!matches(&*matcher, decoy), "{fileset} takes {decoy:?}");
                }
            }
        }
    }

    /// Resolve names one file, whichever tool resolves it.
    #[test]
    fn a_resolved_path_reaches_jj_as_exactly_that_file() {
        for tool in [
            ResolveTool::Default,
            ResolveTool::Ours,
            ResolveTool::Content(crate::types::StagedFile::empty()),
        ] {
            let cmd = JJCommand {
                kind: JJCommandKind::Resolve {
                    change_id: RevisionArg::new("x"),
                    path: "conf[lict].rs".into(),
                    tool,
                },
                flags: CommandFlags::empty(),
            };
            let [fileset] = filesets(&cmd).try_into().expect("one fileset");
            let matcher = matcher(&fileset);
            assert!(matches(&*matcher, "conf[lict].rs"), "{fileset}");
            assert!(!matches(&*matcher, "confl.rs"), "{fileset}");
        }
    }
}

#[cfg(test)]
mod tool_config_tests {
    use std::path::{Path, PathBuf};

    use jj_lib::config::{
        ConfigLayer, ConfigNamePathBuf, ConfigSource, ConfigValue, StackedConfig,
    };

    use super::*;
    use crate::types::{MessageMode, RevisionArg};

    /// The tool `--tool` names, and the `--config` values as jj reads them:
    /// each split at its first `=` and the value parsed as TOML. jj falls
    /// back to a bare string for a value that isn't TOML, so a mis-quoted
    /// path would be taken with its quotes; parsing strictly here catches it.
    fn tool_config(cmd: &JJCommand) -> (String, StackedConfig) {
        let args = cmd.args();
        let mut tool = None;
        let mut layer = ConfigLayer::empty(ConfigSource::CommandArg);
        for pair in args.windows(2) {
            match pair[0].as_str() {
                "--tool" => tool = Some(pair[1].to_string()),
                "--config" => {
                    let (key, value) = pair[1].split_once('=').expect("a key=value");
                    let key: ConfigNamePathBuf = key.parse().expect("a config key");
                    let value: ConfigValue = value
                        .parse()
                        .unwrap_or_else(|e| panic!("not TOML: {value}: {e}"));
                    layer.set_value(&key, value).expect("a settable key");
                }
                _ => {}
            }
        }
        let mut config = StackedConfig::empty();
        config.add_layer(layer);
        (tool.expect("a --tool"), config)
    }

    fn exe() -> String {
        std::env::current_exe()
            .expect("the test binary")
            .display()
            .to_string()
    }

    const AWKWARD_PATHS: &[&str] = &[
        "/tmp/plain.json",
        "/tmp/with space/sel.json",
        "/tmp/say \"hi\"/sel.json",
        "/tmp/it's.json",
        "C:\\Users\\me\\sel.json",
        "/tmp/tab\there.json",
        "/tmp/line\nbreak.json",
    ];

    fn squash_lines(path: &Path) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Squash {
                change_id: RevisionArg::new("x"),
                target: None,
                message: MessageMode::Default,
                selection: ChangeSelection::Lines(std::sync::Arc::new(
                    crate::types::StagedFile::adopt(path.to_path_buf()),
                )),
            },
            flags: CommandFlags::empty(),
        }
    }

    /// The diff tool jj is told to use is the one configured, invoking this
    /// binary on the selection file, whatever characters either path holds.
    #[test]
    fn the_line_selection_tool_reads_back_as_this_binary_and_the_selection() {
        for path in AWKWARD_PATHS {
            let (tool, config) = tool_config(&squash_lines(&PathBuf::from(path)));
            let program: String = config
                .get(["merge-tools", &tool, "program"])
                .unwrap_or_else(|e| panic!("{path:?}: {e}"));
            assert_eq!(program, exe(), "{path:?}");
            let edit_args: Vec<String> = config
                .get(["merge-tools", &tool, "edit-args"])
                .unwrap_or_else(|e| panic!("{path:?}: {e}"));
            assert_eq!(
                edit_args,
                ["--apply-diff", path, "$left", "$right"],
                "{path:?}"
            );
            let style: String = config
                .get(["ui", "conflict-marker-style"])
                .expect("a style");
            assert_eq!(style, MARKER_STYLE_CONFIG);
        }
    }

    /// The merge tool jj is told to use is the one configured, invoking this
    /// binary on the picked content, and reading back the conflict markers
    /// a partial pick leaves in the style they were written.
    #[test]
    fn the_resolution_tool_reads_back_as_this_binary_and_the_content() {
        for path in AWKWARD_PATHS {
            let cmd = JJCommand {
                kind: JJCommandKind::Resolve {
                    change_id: RevisionArg::new("x"),
                    path: "src/lib.rs".into(),
                    tool: ResolveTool::Content(std::sync::Arc::new(
                        crate::types::StagedFile::adopt(PathBuf::from(path)),
                    )),
                },
                flags: CommandFlags::empty(),
            };
            let (tool, config) = tool_config(&cmd);
            let program: String = config
                .get(["merge-tools", &tool, "program"])
                .unwrap_or_else(|e| panic!("{path:?}: {e}"));
            assert_eq!(program, exe(), "{path:?}");
            let merge_args: Vec<String> = config
                .get(["merge-tools", &tool, "merge-args"])
                .unwrap_or_else(|e| panic!("{path:?}: {e}"));
            assert_eq!(
                merge_args,
                ["--apply-resolution", path, "$output"],
                "{path:?}"
            );
            let edits: bool = config
                .get(["merge-tools", &tool, "merge-tool-edits-conflict-markers"])
                .expect("a bool");
            assert!(edits);
            let style: String = config
                .get(["merge-tools", &tool, "conflict-marker-style"])
                .expect("a style");
            assert_eq!(style, MARKER_STYLE_CONFIG);
        }
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use crate::types::{
        BookmarkName, ChangeId, RebaseKind, RebaseSource, RebaseTarget, RemoteName, RevisionArg,
        SmallVec,
    };

    fn cmd(kind: JJCommandKind, flags: CommandFlags) -> JJCommand {
        JJCommand { kind, flags }
    }

    fn args(cmd: &JJCommand) -> Vec<String> {
        cmd.args().iter().map(|a| a.to_string()).collect()
    }

    fn ids(ids: &[&str]) -> SmallVec<RevisionArg> {
        ids.iter().map(|id| RevisionArg::new(*id)).collect()
    }

    /// A typed command line, word by word as the log colours it: `S`ubcommand,
    /// `F`lag or `V`alue.
    fn lexed(words: &[&str], flags: CommandFlags) -> Vec<(String, char)> {
        let raw = cmd(
            JJCommandKind::Raw {
                args: words.iter().map(|w| Str::from(*w)).collect(),
            },
            flags,
        );
        raw.tagged_args()
            .into_iter()
            .map(|arg| {
                let kind = match arg.kind {
                    CommandPartKind::Subcommand => 'S',
                    CommandPartKind::Flag => 'F',
                    CommandPartKind::String => 'V',
                    _ => '?',
                };
                (arg.text.to_string(), kind)
            })
            .collect()
    }

    /// Only a compound command's second word is a subcommand, and only when
    /// it isn't a flag; past a bare `--` every word is the subprocess's, flag
    /// spelling or not. A global toggle spliced in lands ahead of the `--`
    /// without shifting what the words after it are taken for.
    #[test]
    fn a_typed_command_is_read_by_jjs_grammar() {
        let cases: &[(&[&str], CommandFlags, &str)] = &[
            (
                &["git", "push", "--remote", "o"],
                CommandFlags::empty(),
                "SSFV",
            ),
            (&["git", "--help"], CommandFlags::empty(), "SF"),
            (&["log", "git", "-r", "@"], CommandFlags::empty(), "SVFV"),
            (&["run", "--", "-x", "--"], CommandFlags::empty(), "SFVV"),
            (&["run", "--", "--debug"], CommandFlags::DEBUG, "SFFV"),
            (&["bookmark", "list"], CommandFlags::DEBUG, "SSF"),
        ];
        for (words, flags, kinds) in cases {
            let lexed = lexed(words, *flags);
            let got: String = lexed.iter().map(|(_, kind)| kind).collect();
            assert_eq!(got, *kinds, "{lexed:?}");
        }
        let texts: Vec<String> = lexed(&["run", "--", "--debug"], CommandFlags::DEBUG)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(texts, ["run", "--debug", "--", "--debug"]);
    }

    /// jj takes one revision per occurrence of these flags; a second value
    /// after one flag would be read as a positional argument.
    #[test]
    fn each_listed_revision_gets_its_own_flag() {
        let two = || ids(&["a", "b"]);
        let dest = || RebaseTarget {
            targets: ids(&["c", "d"]),
            kind: RebaseKind::After,
        };
        let commands = [
            JJCommandKind::Rebase {
                change_ids: two(),
                source_mode: RebaseSource::Source,
                dest: dest(),
            },
            JJCommandKind::Revert {
                change_ids: two(),
                dest: dest(),
            },
            JJCommandKind::SimplifyParents { change_ids: two() },
            JJCommandKind::Fix {
                change_ids: two(),
                selection: ChangeSelection::All,
            },
            JJCommandKind::GitPushBookmark {
                bookmarks: [BookmarkName::new("a"), BookmarkName::new("b")]
                    .into_iter()
                    .collect(),
                remotes: SmallVec::new(),
            },
            JJCommandKind::Run {
                change_ids: two(),
                argv: vec!["make".into()],
                jobs: None,
            },
            JJCommandKind::Converge {
                changes: [ChangeId::new("a"), ChangeId::new("b")]
                    .into_iter()
                    .collect(),
            },
        ];
        for kind in commands {
            let cmd = cmd(kind, CommandFlags::empty());
            let tagged = cmd.tagged_args();
            let revisions = tagged
                .iter()
                .enumerate()
                .filter(|(_, arg)| arg.kind == CommandPartKind::Revision)
                .map(|(at, _)| at)
                .collect::<Vec<_>>();
            assert!(revisions.len() >= 2, "{:?}", cmd.args());
            for at in revisions {
                assert!(
                    tagged[at - 1].kind == CommandPartKind::Flag,
                    "{:?}",
                    cmd.args()
                );
            }
        }
    }

    /// Nothing chosen means jj's own default: its configured remotes, its
    /// `converge` revset, its nearest bookmarks, its `run.jobs`.
    #[test]
    fn nothing_chosen_leaves_the_choice_to_jj() {
        let cases = [
            (
                JJCommandKind::GitPush {
                    all: false,
                    remotes: SmallVec::new(),
                },
                &["git", "push"][..],
            ),
            (
                JJCommandKind::GitFetch {
                    all_remotes: false,
                    remotes: SmallVec::new(),
                },
                &["git", "fetch"],
            ),
            (
                JJCommandKind::Converge {
                    changes: SmallVec::new(),
                },
                &["converge"],
            ),
            (
                JJCommandKind::BookmarkAdvance { change_id: None },
                &["bookmark", "advance"],
            ),
            (
                JJCommandKind::Run {
                    change_ids: ids(&["x"]),
                    argv: vec!["make".into()],
                    jobs: None,
                },
                &["run", "-r", "x", "--", "make"],
            ),
        ];
        for (kind, expected) in cases {
            assert_eq!(args(&cmd(kind, CommandFlags::empty())), expected);
        }
    }

    /// A key's flags can reach a command of another kind (going to a commit
    /// can set a bookmark), so any bit can arrive on any command; passing one
    /// jj gives that command no option for would only get it refused.
    #[test]
    fn a_command_drops_the_toggles_it_has_no_option_for() {
        let globals: Vec<&str> = GLOBAL_TOGGLES.iter().map(|t| t.cli_flag).collect();
        let commands = [
            JJCommandKind::Edit {
                change_id: RevisionArg::new("x"),
            },
            JJCommandKind::Describe {
                change_ids: ids(&["x"]),
                message: "m".into(),
            },
            JJCommandKind::Rebase {
                change_ids: ids(&["x"]),
                source_mode: RebaseSource::Revision,
                dest: RebaseTarget {
                    targets: ids(&["y"]),
                    kind: RebaseKind::Onto,
                },
            },
            JJCommandKind::BookmarkCreate {
                name: BookmarkName::new("b"),
                change_id: RevisionArg::new("x"),
            },
            JJCommandKind::GitFetch {
                all_remotes: false,
                remotes: [RemoteName::new("origin")].into_iter().collect(),
            },
        ];
        for kind in commands {
            let bare = args(&cmd(kind.clone(), CommandFlags::empty()));
            let mut toggled = args(&cmd(kind, CommandFlags::all()));
            toggled.retain(|a| !globals.contains(&a.as_str()));
            assert_eq!(toggled, bare);
        }
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;
    use crate::types::{BookmarkName, RemoteName, RevisionArg};

    fn args(kind: JJCommandKind) -> Vec<String> {
        let cmd = JJCommand {
            kind,
            flags: CommandFlags::empty(),
        };
        cmd.args().iter().map(|a| a.to_string()).collect()
    }

    fn create(name: &str) -> Vec<String> {
        args(JJCommandKind::BookmarkCreate {
            name: BookmarkName::new(name),
            change_id: RevisionArg::new("x"),
        })
    }

    fn delete(name: &str) -> Vec<String> {
        args(JJCommandKind::BookmarkDelete {
            names: [BookmarkName::new(name)].into_iter().collect(),
        })
    }

    /// jj parses every name argument as a revset symbol, which a bare emoji
    /// is not.
    #[test]
    fn a_name_that_is_not_an_identifier_is_quoted() {
        for name in [
            "main",
            "feat/x",
            "🚀-launch",
            "feat/🦀crab",
            "a b",
            r#"q"uote"#,
        ] {
            let spelled = create(name).pop().unwrap();
            assert_eq!(jj_lib::revset::parse_symbol(&spelled).unwrap(), name);
        }
        assert_eq!(create("main").last().unwrap(), "main");
        assert_eq!(create("🚀-launch").last().unwrap(), r#""🚀-launch""#);
    }

    /// A name pattern globs unless told otherwise, so only a plain name
    /// without `*` can go bare.
    #[test]
    fn a_name_pattern_matches_only_that_name() {
        assert_eq!(delete("main"), ["bookmark", "delete", "main"]);
        assert_eq!(
            delete("🚀-launch"),
            ["bookmark", "delete", r#"exact:"🚀-launch""#]
        );
        assert_eq!(delete("feat-*"), ["bookmark", "delete", "exact:feat-*"]);
        assert_eq!(delete("a?b"), ["bookmark", "delete", r#"exact:"a?b""#]);
    }

    #[test]
    fn a_remote_matches_only_that_remote() {
        let fetch = |remote: &str| {
            args(JJCommandKind::GitFetch {
                all_remotes: false,
                remotes: [RemoteName::new(remote)].into_iter().collect(),
            })
        };
        assert_eq!(fetch("origin"), ["git", "fetch", "--remote", "origin"]);
        assert_eq!(fetch("🛰"), ["git", "fetch", "--remote", r#"exact:"🛰""#]);
    }
}
