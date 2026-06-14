use compact_str::format_compact;

use crate::keymap::CommandFlags;
use crate::types::{ChangeSelection, Str, GLOBAL_TOGGLES};

use super::{JJCommand, JJCommandKind, ResolveTool};

impl JJCommand {
    pub fn args(&self) -> Vec<Str> {
        let flags = self.flags;
        let mut args = match &self.kind {
            JJCommandKind::Abandon { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["abandon".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::RETAIN_BOOKMARKS, "--retain-bookmarks"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::Describe {
                change_ids,
                message,
                ..
            } => {
                let mut args: Vec<Str> =
                    vec!["describe".into(), "-m".into(), Str::from(message.as_str())];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::DescribeInEditor { change_id, .. } => {
                vec!["describe".into(), format_compact!("{change_id}")]
            }
            JJCommandKind::Edit { change_id, .. } => {
                vec!["edit".into(), format_compact!("{change_id}")]
            }
            JJCommandKind::New {
                change_ids, insert, ..
            } => {
                let mut args: Vec<Str> = vec!["new".into()];
                if let Some(pos) = insert {
                    args.push(pos.flag().into());
                }
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                push_flags(&mut args, flags, &[(CommandFlags::NO_EDIT, "--no-edit")]);
                args
            }
            JJCommandKind::Rebase {
                change_ids,
                source_mode,
                dest,
                ..
            } => {
                let mut args: Vec<Str> = vec!["rebase".into()];
                let flag = match source_mode {
                    crate::types::RebaseSource::Revision => "-r",
                    crate::types::RebaseSource::Source => "-s",
                    crate::types::RebaseSource::Branch => "-b",
                };
                for id in change_ids {
                    args.push(flag.into());
                    args.push(format_compact!("{id}"));
                }
                for target in &dest.targets {
                    args.push(format_compact!("{}", dest.kind.flag()));
                    args.push(format_compact!("{target}"));
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
                let mut args: Vec<Str> = vec!["restore".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::RESTORE_DESCENDANTS, "--restore-descendants"),
                    ],
                );
                if let Some(id) = from {
                    args.push("--from".into());
                    args.push(format_compact!("{id}"));
                }
                if let Some(id) = into {
                    args.push("--into".into());
                    args.push(format_compact!("{id}"));
                }
                if let Some(id) = changes_in {
                    args.push("--changes-in".into());
                    args.push(format_compact!("{id}"));
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
                let mut args: Vec<Str> =
                    vec!["split".into(), "-r".into(), format_compact!("{change_id}")];
                push_flags(
                    &mut args,
                    flags,
                    &[
                        (CommandFlags::INTERACTIVE, "--interactive"),
                        (CommandFlags::PARALLEL, "--parallel"),
                    ],
                );
                if let Some(t) = target {
                    args.push(format_compact!("{}", t.kind.flag()));
                    args.push(format_compact!("{}", t.target));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::BookmarkCreate {
                name, change_id, ..
            } => {
                vec![
                    "bookmark".into(),
                    "create".into(),
                    "-r".into(),
                    format_compact!("{change_id}"),
                    Str::from(name.as_str()),
                ]
            }
            JJCommandKind::BookmarkSet {
                name, change_id, ..
            } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "set".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::BookmarkDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::BookmarkForget { names, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::BookmarkMove { name, target, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "move".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("--to".into());
                args.push(format_compact!("{target}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::BookmarkRename {
                old_name, new_name, ..
            } => {
                vec![
                    "bookmark".into(),
                    "rename".into(),
                    Str::from(old_name.as_str()),
                    Str::from(new_name.as_str()),
                ]
            }
            JJCommandKind::BookmarkAdvance { change_id, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "advance".into()];
                if let Some(id) = change_id {
                    args.push("--to".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::BookmarkTrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "track".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => {
                let mut args: Vec<Str> = vec!["bookmark".into(), "untrack".into()];
                for br in bookmarks {
                    args.push(Str::from(br.name.as_str()));
                    args.push("--remote".into());
                    args.push(Str::from(br.remote.as_str()));
                }
                args
            }
            JJCommandKind::Undo => vec!["undo".into()],
            JJCommandKind::Redo => vec!["redo".into()],
            JJCommandKind::GitFetch {
                all_remotes,
                remote,
                ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "fetch".into()];
                if *all_remotes {
                    args.push("--all-remotes".into());
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                args
            }
            JJCommandKind::GitPush { all, remote, .. } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                if *all {
                    args.push("--all".into());
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushChange {
                change_id, remote, ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                args.push("-c".into());
                args.push(format_compact!("{change_id}"));
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitPushBookmark {
                bookmarks, remote, ..
            } => {
                let mut args: Vec<Str> = vec!["git".into(), "push".into()];
                for name in bookmarks {
                    args.push("--bookmark".into());
                    args.push(Str::from(name.as_str()));
                }
                if let Some(r) = remote {
                    args.push("--remote".into());
                    args.push(Str::from(r.as_str()));
                }
                push_flags(&mut args, flags, &[(CommandFlags::DRY_RUN, "--dry-run")]);
                args
            }
            JJCommandKind::GitFetchBookmark {
                bookmark, remote, ..
            } => {
                vec![
                    "git".into(),
                    "fetch".into(),
                    "-b".into(),
                    Str::from(bookmark.as_str()),
                    "--remote".into(),
                    Str::from(remote.as_str()),
                ]
            }
            JJCommandKind::GitExport => vec!["git".into(), "export".into()],
            JJCommandKind::GitImport => vec!["git".into(), "import".into()],
            JJCommandKind::Absorb {
                from, selection, ..
            } => {
                let mut args: Vec<Str> = vec!["absorb".into()];
                if let Some(id) = from {
                    args.push("--from".into());
                    args.push(format_compact!("{id}"));
                }
                match selection {
                    ChangeSelection::All => {}
                    ChangeSelection::Files(paths) => {
                        args.extend(paths.iter().cloned());
                    }
                    ChangeSelection::Lines(_) => {
                        debug_assert!(false, "line selection should be blocked for absorb");
                    }
                }
                args
            }
            JJCommandKind::Commit {
                message, selection, ..
            } => {
                let mut args: Vec<Str> = vec!["commit".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::INTERACTIVE, "--interactive")],
                );
                if let Some(msg) = message {
                    args.push("-m".into());
                    args.push(Str::from(msg.as_str()));
                }
                push_change_selection(&mut args, selection);
                args
            }
            JJCommandKind::Parallelize { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["parallelize".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::SimplifyParents { change_ids, .. } => {
                let mut args: Vec<Str> = vec!["simplify-parents".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                args
            }
            JJCommandKind::Revert {
                change_ids, dest, ..
            } => {
                let mut args: Vec<Str> = vec!["revert".into()];
                for id in change_ids {
                    args.push("-r".into());
                    args.push(format_compact!("{id}"));
                }
                for target in &dest.targets {
                    args.push(dest.kind.flag().into());
                    args.push(format_compact!("{target}"));
                }
                args
            }
            JJCommandKind::Duplicate {
                change_ids, onto, ..
            } => {
                let mut args: Vec<Str> = vec!["duplicate".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                if let Some(target) = onto {
                    args.push("--onto".into());
                    args.push(format_compact!("{target}"));
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
                let mut args: Vec<Str> = vec!["squash".into()];
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
                        args.push("-m".into());
                        args.push(Str::from(msg.as_str()));
                    }
                    crate::types::MessageMode::UseDestination => {
                        args.push("--use-destination-message".into());
                    }
                }
                match target {
                    None => {
                        args.push("-r".into());
                        args.push(format_compact!("{change_id}"));
                    }
                    Some(t) => {
                        args.push("--from".into());
                        args.push(format_compact!("{change_id}"));
                        args.push(format_compact!("{}", t.kind.flag()));
                        args.push(format_compact!("{}", t.target));
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
                let mut args: Vec<Str> = vec!["workspace".into(), "add".into()];
                if let Some(n) = name {
                    args.push("--name".into());
                    args.push(Str::from(n.as_str()));
                }
                args.push("-r".into());
                args.push(format_compact!("{revision}"));
                args.push(Str::from(path.as_str()));
                args
            }
            JJCommandKind::WorkspaceForget { names, .. } => {
                let mut args: Vec<Str> = vec!["workspace".into(), "forget".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::WorkspaceList => {
                vec!["workspace".into(), "list".into()]
            }
            JJCommandKind::WorkspaceRename { new_name, .. } => {
                vec![
                    "workspace".into(),
                    "rename".into(),
                    Str::from(new_name.as_str()),
                ]
            }
            JJCommandKind::TagSet {
                name, change_id, ..
            } => {
                let mut args: Vec<Str> = vec!["tag".into(), "set".into()];
                push_flags(
                    &mut args,
                    flags,
                    &[(CommandFlags::ALLOW_BACKWARDS, "--allow-backwards")],
                );
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                args.push(Str::from(name.as_str()));
                args
            }
            JJCommandKind::TagDelete { names, .. } => {
                let mut args: Vec<Str> = vec!["tag".into(), "delete".into()];
                args.extend(names.iter().map(|n| Str::from(n.as_str())));
                args
            }
            JJCommandKind::OpRestore { op_id, .. } => {
                vec!["op".into(), "restore".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::OpRevert { op_id, .. } => {
                vec!["op".into(), "revert".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::OpAbandon { op_id, .. } => {
                vec!["op".into(), "abandon".into(), Str::from(op_id.as_str())]
            }
            JJCommandKind::Fix {
                change_ids,
                selection,
                ..
            } => {
                let mut args: Vec<Str> = vec!["fix".into(), "-s".into()];
                for id in change_ids {
                    args.push(format_compact!("{id}"));
                }
                push_change_selection(&mut args, &selection);
                args
            }
            JJCommandKind::FileUntrack { paths, .. } => {
                let mut args: Vec<Str> = vec!["file".into(), "untrack".into()];
                args.extend(paths.iter().cloned());
                args
            }
            JJCommandKind::Resolve {
                change_id,
                path,
                tool,
                ..
            } => {
                let mut args: Vec<Str> = vec!["resolve".into()];
                args.push("-r".into());
                args.push(format_compact!("{change_id}"));
                match tool {
                    ResolveTool::Ours => args.push("--tool=:ours".into()),
                    ResolveTool::Theirs => args.push("--tool=:theirs".into()),
                    ResolveTool::Default => {}
                }
                args.push(path.clone());
                args
            }
            JJCommandKind::Raw { args } => return args.clone(),
        };

        push_global_flags(&mut args, flags);
        args
    }
}

fn push_flags(args: &mut Vec<Str>, flags: CommandFlags, mapping: &[(CommandFlags, &str)]) {
    for (flag, arg) in mapping {
        if flags.contains(*flag) {
            args.push(Str::from(*arg));
        }
    }
}

fn push_change_selection(args: &mut Vec<Str>, selection: &ChangeSelection) {
    match selection {
        ChangeSelection::All => {}
        ChangeSelection::Files(paths) => {
            args.extend(paths.iter().cloned());
        }
        ChangeSelection::Lines(json_path) => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "kojutsu".into());
            args.extend([
                "--interactive".into(),
                "--tool".into(),
                "kojutsu-select".into(),
                "--config".into(),
                Str::from(format!(
                    "merge-tools.kojutsu-select.program={}",
                    toml_string_escape(&exe.display().to_string())
                )),
                "--config".into(),
                Str::from(format!(
                    "merge-tools.kojutsu-select.edit-args=[\"--apply-diff\", {}, \"$left\", \"$right\"]",
                    toml_string_escape(&json_path.display().to_string())
                )),
            ]);
        }
    }
}

fn push_global_flags(args: &mut Vec<Str>, flags: CommandFlags) {
    for toggle in GLOBAL_TOGGLES {
        if flags.contains(toggle.flag) {
            args.push(Str::from(toggle.cli_flag));
        }
    }
}

fn toml_string_escape(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
