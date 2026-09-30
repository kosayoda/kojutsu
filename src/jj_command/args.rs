use compact_str::format_compact;

use crate::conflict::MARKER_STYLE_CONFIG;
use crate::keymap::CommandFlags;
use crate::types::{ChangeSelection, GLOBAL_TOGGLES, Str};

use super::{CommandPartKind, JJCommand, JJCommandKind, ResolveTool};

/// A CLI argument together with its display kind, assigned at construction
/// so the command log never has to guess what an argument is.
pub(super) type TaggedArg = (Str, CommandPartKind);

fn sub(name: &'static str) -> TaggedArg {
    (name.into(), CommandPartKind::Subcommand)
}

fn flag(text: impl Into<Str>) -> TaggedArg {
    (text.into(), CommandPartKind::Flag)
}

/// A revision-like identifier: change/commit/op IDs and bookmark/tag names
/// (which resolve to revisions and share their color in the theme).
fn rev(id: impl std::fmt::Display) -> TaggedArg {
    (format_compact!("{id}"), CommandPartKind::Revision)
}

/// A bookmark or tag on one remote, as the `name@remote` symbol jj resolves
/// exactly, quoted where the name or remote needs it.
fn remote_symbol(name: &str, remote: &crate::types::RemoteName) -> TaggedArg {
    rev(jj_lib::revset::format_remote_symbol(name, remote.as_str()))
}

/// A plain value: message, path, count, remote or workspace name.
fn arg(text: impl Into<Str>) -> TaggedArg {
    (text.into(), CommandPartKind::String)
}

impl JJCommand {
    pub fn args(&self) -> Vec<Str> {
        self.tagged_args()
            .into_iter()
            .map(|(text, kind)| {
                if kind == CommandPartKind::Fileset {
                    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
                    format_compact!("\"{escaped}\"")
                } else {
                    text
                }
            })
            .collect()
    }

    pub(super) fn tagged_args(&self) -> Vec<TaggedArg> {
        let flags = self.flags;
        let mut args = match &self.kind {
            JJCommandKind::Abandon { change_ids, .. } => {
                let mut args = vec![sub("abandon")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                args.extend(change_ids.iter().map(rev));
                args
            }
            JJCommandKind::Describe {
                change_ids,
                message,
                ..
            } => {
                let mut args = vec![sub("describe"), flag("-m"), arg(message.as_str())];
                args.extend(change_ids.iter().map(rev));
                args
            }
            JJCommandKind::DescribeInEditor { change_id, .. } => {
                vec![sub("describe"), rev(change_id)]
            }
            JJCommandKind::Diffedit {
                change_id,
                selection,
                ..
            } => {
                let mut args = vec![sub("diffedit"), flag("-r"), rev(change_id)];
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Edit { change_id, .. } => {
                vec![sub("edit"), rev(change_id)]
            }
            JJCommandKind::New {
                change_ids, insert, ..
            } => {
                let mut args = vec![sub("new")];
                if let Some(pos) = insert {
                    args.push(flag(pos.flag()));
                }
                args.extend(change_ids.iter().map(rev));
                push_flags(&mut args, flags, &[(CommandFlags::NO_EDIT, "--no-edit")]);
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
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
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
                push_change_selection(&mut args, selection);
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
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::PARALLEL, "--parallel"),
                    ],
                );
                if let Some(t) = target {
                    args.push(flag(t.kind.flag()));
                    args.push(rev(&t.target));
                }
                push_change_selection(&mut args, selection);
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
                    rev(name.as_str()),
                ]
            }
            JJCommandKind::BookmarkSet {
                name, change_id, ..
            } => {
                let mut args = vec![sub("bookmark"), sub("set")];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push(flag("-r"));
                args.push(rev(change_id));
                args.push(rev(name.as_str()));
                args
            }
            JJCommandKind::BookmarkDelete { names, .. } => {
                let mut args = vec![sub("bookmark"), sub("delete")];
                args.extend(names.iter().map(|n| rev(n.as_str())));
                args
            }
            JJCommandKind::BookmarkForget { names, .. } => {
                let mut args = vec![sub("bookmark"), sub("forget")];
                args.extend(names.iter().map(|n| rev(n.as_str())));
                args
            }
            JJCommandKind::BookmarkMove { name, target, .. } => {
                let mut args = vec![sub("bookmark"), sub("move")];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push(flag("--to"));
                args.push(rev(target));
                args.push(rev(name.as_str()));
                args
            }
            JJCommandKind::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    sub("bookmark"),
                    sub("rename"),
                    rev(old_name.as_str()),
                    rev(new_name.as_str()),
                ]
            }
            JJCommandKind::BookmarkAdvance { change_id, .. } => {
                let mut args = vec![sub("bookmark"), sub("advance")];
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
                        .map(|b| remote_symbol(b.name.as_str(), &b.remote)),
                );
                args
            }
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => {
                let mut args = vec![sub("bookmark"), sub("untrack")];
                args.extend(
                    bookmarks
                        .iter()
                        .map(|b| remote_symbol(b.name.as_str(), &b.remote)),
                );
                args
            }
            JJCommandKind::Undo => vec![sub("undo")],
            JJCommandKind::Redo => vec![sub("redo")],
            JJCommandKind::GitFetch {
                all_remotes,
                remote,
                ..
            } => {
                let mut args = vec![sub("git"), sub("fetch")];
                if *all_remotes {
                    args.push(flag("--all-remotes"));
                }
                if let Some(r) = remote {
                    args.push(flag("--remote"));
                    args.push(arg(r.as_str()));
                }
                args
            }
            JJCommandKind::GitPush { all, remote, .. } => {
                let mut args = vec![sub("git"), sub("push")];
                if *all {
                    args.push(flag("--all"));
                }
                if let Some(r) = remote {
                    args.push(flag("--remote"));
                    args.push(arg(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushChange {
                change_id, remote, ..
            } => {
                let mut args = vec![sub("git"), sub("push"), flag("-c"), rev(change_id)];
                if let Some(r) = remote {
                    args.push(flag("--remote"));
                    args.push(arg(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushBookmark {
                bookmarks, remote, ..
            } => {
                let mut args = vec![sub("git"), sub("push")];
                for name in bookmarks {
                    args.push(flag("--bookmark"));
                    args.push(rev(name.as_str()));
                }
                if let Some(r) = remote {
                    args.push(flag("--remote"));
                    args.push(arg(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitFetchBookmark {
                bookmark, remote, ..
            } => {
                vec![
                    sub("git"),
                    sub("fetch"),
                    flag("-b"),
                    rev(bookmark.as_str()),
                    flag("--remote"),
                    arg(remote.as_str()),
                ]
            }
            JJCommandKind::GitExport => vec![sub("git"), sub("export")],
            JJCommandKind::GitImport => vec![sub("git"), sub("import")],
            JJCommandKind::Absorb {
                from, selection, ..
            } => {
                let mut args = vec![sub("absorb")];
                if let Some(id) = from {
                    args.push(flag("--from"));
                    args.push(rev(id));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Commit {
                message, selection, ..
            } => {
                let mut args = vec![sub("commit")];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::INTERACTIVE, "--interactive")],
                );
                if let Some(msg) = message {
                    args.push(flag("-m"));
                    args.push(arg(msg.as_str()));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Parallelize { change_ids, .. } => {
                let mut args = vec![sub("parallelize")];
                args.extend(change_ids.iter().map(rev));
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
                args.extend(change_ids.iter().map(rev));
                if let Some(target) = onto {
                    args.push(flag("--onto"));
                    args.push(rev(target));
                }
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
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::KEEP_EMPTIED, "--keep-emptied"),
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
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::WorkspaceAdd {
                path,
                name,
                revision,
                ..
            } => {
                let mut args = vec![sub("workspace"), sub("add")];
                if let Some(n) = name {
                    args.push(flag("--name"));
                    args.push(arg(n.as_str()));
                }
                args.push(flag("-r"));
                args.push(rev(revision));
                args.push(arg(path.as_str()));
                args
            }
            JJCommandKind::WorkspaceForget { names, .. } => {
                let mut args = vec![sub("workspace"), sub("forget")];
                args.extend(names.iter().map(|n| arg(n.as_str())));
                args
            }
            JJCommandKind::WorkspaceList => {
                vec![sub("workspace"), sub("list")]
            }
            JJCommandKind::WorkspaceRename { new_name, .. } => {
                vec![sub("workspace"), sub("rename"), arg(new_name.as_str())]
            }
            JJCommandKind::TagSet {
                name, change_id, ..
            } => {
                let mut args = vec![sub("tag"), sub("set")];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push(flag("-r"));
                args.push(rev(change_id));
                args.push(rev(name.as_str()));
                args
            }
            JJCommandKind::TagDelete { names, .. } => {
                let mut args = vec![sub("tag"), sub("delete")];
                args.extend(names.iter().map(|n| rev(n.as_str())));
                args
            }
            JJCommandKind::TagTrack { tags } => {
                let mut args = vec![sub("tag"), sub("track")];
                args.extend(
                    tags.iter()
                        .map(|t| remote_symbol(t.name.as_str(), &t.remote)),
                );
                args
            }
            JJCommandKind::TagUntrack { tags } => {
                let mut args = vec![sub("tag"), sub("untrack")];
                args.extend(
                    tags.iter()
                        .map(|t| remote_symbol(t.name.as_str(), &t.remote)),
                );
                args
            }
            JJCommandKind::OpRestore { op_id, .. } => {
                vec![sub("op"), sub("restore"), rev(op_id.as_str())]
            }
            JJCommandKind::OpRevert { op_id, .. } => {
                vec![sub("op"), sub("revert"), rev(op_id.as_str())]
            }
            JJCommandKind::OpAbandon { op_id, .. } => {
                vec![sub("op"), sub("abandon"), rev(op_id.as_str())]
            }
            JJCommandKind::Fix {
                change_ids,
                selection,
                ..
            } => {
                let mut args = vec![sub("fix"), flag("-s")];
                args.extend(change_ids.iter().map(rev));
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Run {
                change_ids,
                argv,
                jobs,
            } => {
                let mut args = vec![sub("run")];
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
                        (CommandFlags::CLEAN, "--clean"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                        (CommandFlags::PASSTHROUGH, "--passthrough"),
                        (CommandFlags::IGNORE_CHANGES, "--ignore-changes"),
                        (CommandFlags::IGNORE_ERRORS, "--ignore-errors"),
                    ],
                );
                // Global flags must precede `--`: everything after it is
                // passed verbatim to the subprocess, not to jj.
                push_global_flags(&mut args, flags);
                args.push(flag("--"));
                args.extend(argv.iter().cloned().map(arg));
                return args;
            }
            JJCommandKind::FileUntrack { paths, .. } => {
                let mut args = vec![sub("file"), sub("untrack")];
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
                        args.extend(self_invoking_tool("kojutsu-apply"));
                        args.extend([
                            flag("--config"),
                            arg(format!(
                                "merge-tools.kojutsu-apply.merge-args=[\"--apply-resolution\", {}, \"$output\"]",
                                toml_string_escape(&content_path.display().to_string())
                            )),
                            // Partially picked files keep conflict markers;
                            // jj parses them back into a conflicted state,
                            // so it has to read them the way we wrote them.
                            flag("--config"),
                            arg("merge-tools.kojutsu-apply.merge-tool-edits-conflict-markers=true"),
                            flag("--config"),
                            arg(format!(
                                "merge-tools.kojutsu-apply.conflict-marker-style=\"{}\"",
                                MARKER_STYLE_CONFIG
                            )),
                        ]);
                    }
                }
                args.push(fileset_arg(path));
                args
            }
            JJCommandKind::Raw { args } => return lex_raw_args(args),
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
                        (arg.clone(), kind)
                    })
                    .collect();
            }
        };

        push_global_flags(&mut args, flags);
        args
    }
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
        parts.push((first.clone(), CommandPartKind::Subcommand));
        rest = tail;
        if COMPOUND_SUBCOMMANDS.contains(&first.as_str())
            && let Some((second, tail)) = rest.split_first()
            && !second.starts_with('-')
        {
            parts.push((second.clone(), CommandPartKind::Subcommand));
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
        parts.push((a.clone(), kind));
    }
    parts
}

fn push_flags(args: &mut Vec<TaggedArg>, flags: CommandFlags, mapping: &[(CommandFlags, &str)]) {
    for (flag_bit, cli_flag) in mapping {
        if flags.contains(*flag_bit) {
            args.push(flag(*cli_flag));
        }
    }
}

fn push_change_selection(args: &mut Vec<TaggedArg>, selection: &ChangeSelection) {
    match selection {
        ChangeSelection::All => {}
        ChangeSelection::Files(paths) => {
            args.extend(paths.iter().map(|p| fileset_arg(p)));
        }
        ChangeSelection::Lines(json_path) => {
            args.push(flag("--interactive"));
            args.extend(self_invoking_tool("kojutsu-select"));
            args.extend([
                flag("--config"),
                arg(format!(
                    "merge-tools.kojutsu-select.edit-args=[\"--apply-diff\", {}, \"$left\", \"$right\"]",
                    toml_string_escape(&json_path.display().to_string())
                )),
                // Selections carry line numbers computed against kojutsu's
                // materialization; pin jj's tool-side materialization to the
                // same style so lines align in conflicted files.
                flag("--config"),
                arg(format!(
                    "ui.conflict-marker-style=\"{}\"",
                    MARKER_STYLE_CONFIG
                )),
            ]);
        }
    }
}

/// Configure a merge tool that re-invokes the running kojutsu binary:
/// `--tool <name> --config merge-tools.<name>.program=<exe>`. Callers
/// append the tool-specific `*-args` and any extra configs. Single home
/// for the injection-sensitive program-path construction.
fn self_invoking_tool(name: &str) -> Vec<TaggedArg> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "kojutsu".into());
    vec![
        flag("--tool"),
        arg(name.to_string()),
        flag("--config"),
        arg(format!(
            "merge-tools.{name}.program={}",
            toml_string_escape(&exe.display().to_string())
        )),
    ]
}

fn fileset_arg(path: &str) -> TaggedArg {
    (path.into(), CommandPartKind::Fileset)
}

fn push_global_flags(args: &mut Vec<TaggedArg>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(flag(toggle.cli_flag));
        }
    }
}

fn toml_string_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
