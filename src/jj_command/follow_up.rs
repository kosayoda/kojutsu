use strum::IntoEnumIterator;

use crate::keymap::CommandFlags;
use crate::types::{
    BookmarkName, ChangeId, ChangeSelection, CommitId, MessageMode, PendingCommand,
    PendingCommitSelect, ReadyCommand, RebaseKind, RebaseSource, RebaseTarget, SmallVec, SmallVec1,
    SplitTarget, SquashTarget, Str, TagName, TargetOperation, WorkspaceName,
};

use super::{JJCommand, JJCommandKind};

pub struct FollowUpOption {
    pub key: char,
    pub label: &'static str,
    pub action: FollowUpAction,
}

pub enum FollowUpAction {
    Execute(JJCommand),
    TextInput {
        prompt: String,
        pending: PendingCommand,
    },
    WidenRevset {
        change_id: String,
    },
    EnterInterdiff {
        from: CommitId,
        to: CommitId,
        from_label: Str,
        to_label: Str,
    },
}

impl JJCommand {
    pub fn retry_options(&self, output: &[u8]) -> Vec<FollowUpOption> {
        let mut options = Vec::new();
        let text = String::from_utf8_lossy(output);

        if text.contains("immutable") {
            options.push(FollowUpOption {
                key: 'r',
                label: "retry with --ignore-immutable",
                action: FollowUpAction::Execute(
                    self.clone().with_flag(CommandFlags::IGNORE_IMMUTABLE),
                ),
            });
        } else if text.contains("stale") && text.contains("working copy") {
            options.push(FollowUpOption {
                key: 'r',
                label: "retry with --ignore-working-copy",
                action: FollowUpAction::Execute(
                    self.clone().with_flag(CommandFlags::IGNORE_WORKING_COPY),
                ),
            });
        }

        options
    }
}

impl PendingCommitSelect {
    pub fn into_jj_command(self, target: ChangeId, flags: CommandFlags) -> JJCommand {
        match self {
            PendingCommitSelect::WorkspaceAdd { path, name } => JJCommand {
                kind: JJCommandKind::WorkspaceAdd {
                    path,
                    name,
                    revision: target,
                },
                flags,
            },
        }
    }
}

impl PendingCommand {
    pub fn into_jj_command(self, text: String) -> Option<JJCommand> {
        match self {
            PendingCommand::Describe { change_ids, flags } => Some(JJCommand {
                kind: JJCommandKind::Describe {
                    change_ids,
                    message: text,
                },
                flags,
            }),
            PendingCommand::SquashWithMessage { builder, flags } => {
                Some(builder.build(text, flags))
            }
            PendingCommand::Revset
            | PendingCommand::WorkspaceAddPath { .. }
            | PendingCommand::WorkspaceAddName { .. } => None,
            PendingCommand::BookmarkCreate { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkCreate {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::BookmarkSet { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkSet {
                    name: BookmarkName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::BookmarkSetByName { name, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkSet {
                    name,
                    change_id: ChangeId::new(text),
                },
                flags,
            }),
            PendingCommand::BookmarkRename { old_name, flags } => Some(JJCommand {
                kind: JJCommandKind::BookmarkRename {
                    old_name,
                    new_name: BookmarkName::new(text),
                },
                flags,
            }),
            PendingCommand::TagSet { change_id, flags } => Some(JJCommand {
                kind: JJCommandKind::TagSet {
                    name: TagName::new(text),
                    change_id,
                },
                flags,
            }),
            PendingCommand::TagSetByName { name, flags } => Some(JJCommand {
                kind: JJCommandKind::TagSet {
                    name,
                    change_id: ChangeId::new(text),
                },
                flags,
            }),
            PendingCommand::Commit { flags, selection } => Some(JJCommand {
                kind: JJCommandKind::Commit {
                    message: Some(text),
                    selection,
                },
                flags,
            }),
            PendingCommand::WorkspaceRename { flags } => Some(JJCommand {
                kind: JJCommandKind::WorkspaceRename {
                    new_name: WorkspaceName::new(text),
                },
                flags,
            }),
            PendingCommand::RawCommand => {
                let args = match shlex::split(&text) {
                    Some(args) if !args.is_empty() => args,
                    _ => return None,
                };
                Some(JJCommand {
                    kind: JJCommandKind::Raw {
                        args: args.into_iter().map(Str::from).collect(),
                    },
                    flags: CommandFlags::empty(),
                })
            }
            PendingCommand::LuaResume => None,
        }
    }
}

impl TargetOperation {
    pub fn follow_up(
        self,
        source: ChangeId,
        targets: SmallVec1<ChangeId>,
        flags: CommandFlags,
        selection: ChangeSelection,
    ) -> Vec<FollowUpOption> {
        match self {
            TargetOperation::Squash(kind) => squash_follow_up(
                source,
                Some(SquashTarget {
                    target: targets.split_off_first().0,
                    kind,
                }),
                selection,
                flags,
            ),
            TargetOperation::Split(kind) => auto_follow_up(
                "split",
                JJCommand {
                    kind: JJCommandKind::Split {
                        change_id: source,
                        target: Some(SplitTarget {
                            target: targets.split_off_first().0,
                            kind,
                        }),
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::Rebase {
                source_mode,
                sources,
            } => rebase_follow_up(sources, targets.into_smallvec(), source_mode, flags),
            TargetOperation::RestoreFrom => auto_follow_up(
                "restore",
                JJCommand {
                    kind: JJCommandKind::Restore {
                        from: Some(targets.split_off_first().0),
                        into: None,
                        changes_in: None,
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::RestoreInto => auto_follow_up(
                "restore",
                JJCommand {
                    kind: JJCommandKind::Restore {
                        from: None,
                        into: Some(targets.split_off_first().0),
                        changes_in: None,
                        selection,
                    },
                    flags,
                },
            ),
            TargetOperation::BookmarkMove { bookmark_name } => auto_follow_up(
                "move",
                JJCommand {
                    kind: JJCommandKind::BookmarkMove {
                        name: bookmark_name.clone(),
                        target: targets.split_off_first().0,
                    },
                    flags,
                },
            ),
            TargetOperation::DuplicateOnto { sources } => auto_follow_up(
                "duplicate",
                JJCommand {
                    kind: JJCommandKind::Duplicate {
                        change_ids: sources,
                        onto: Some(targets.split_off_first().0),
                    },
                    flags,
                },
            ),
            TargetOperation::Interdiff => {
                let to = targets.split_off_first().0;
                vec![FollowUpOption {
                    key: ' ',
                    label: "interdiff",
                    action: FollowUpAction::EnterInterdiff {
                        from: CommitId::new(source.as_str()),
                        to: CommitId::new(to.as_str()),
                        from_label: Str::from(source.as_str()),
                        to_label: Str::from(to.as_str()),
                    },
                }]
            }
            TargetOperation::Revert { sources } => RebaseKind::iter()
                .map(|kind| FollowUpOption {
                    key: kind.key(),
                    label: kind.label(),
                    action: FollowUpAction::Execute(JJCommand {
                        kind: JJCommandKind::Revert {
                            change_ids: sources.clone(),
                            dest: RebaseTarget {
                                targets: targets.clone().into_smallvec(),
                                kind,
                            },
                        },
                        flags,
                    }),
                })
                .collect(),
        }
    }
}

impl ReadyCommand {
    pub fn build(self, message: String, flags: CommandFlags) -> JJCommand {
        match self {
            ReadyCommand::Squash {
                source,
                target,
                selection,
            } => JJCommand {
                kind: JJCommandKind::Squash {
                    change_id: source,
                    target,
                    message: MessageMode::Inline(message),
                    selection,
                },
                flags,
            },
        }
    }
}

fn auto_follow_up(label: &'static str, cmd: JJCommand) -> Vec<FollowUpOption> {
    vec![FollowUpOption {
        key: ' ',
        label,
        action: FollowUpAction::Execute(cmd),
    }]
}

fn squash_follow_up(
    source: ChangeId,
    target: Option<SquashTarget>,
    selection: ChangeSelection,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    let default_cmd = JJCommand {
        kind: JJCommandKind::Squash {
            change_id: source.clone(),
            target: target.clone(),
            message: MessageMode::Default,
            selection: selection.clone(),
        },
        flags,
    };
    let use_dest_cmd = JJCommand {
        kind: JJCommandKind::Squash {
            change_id: source.clone(),
            target: target.clone(),
            message: MessageMode::UseDestination,
            selection: selection.clone(),
        },
        flags,
    };
    let builder = ReadyCommand::Squash {
        source,
        target,
        selection,
    };

    vec![
        FollowUpOption {
            key: 's',
            label: "squash",
            action: FollowUpAction::Execute(default_cmd),
        },
        FollowUpOption {
            key: 'm',
            label: "with message",
            action: FollowUpAction::TextInput {
                prompt: "squash message: ".to_string(),
                pending: PendingCommand::SquashWithMessage { builder, flags },
            },
        },
        FollowUpOption {
            key: 'd',
            label: "use dest message",
            action: FollowUpAction::Execute(use_dest_cmd),
        },
    ]
}

fn rebase_follow_up(
    sources: SmallVec<ChangeId>,
    targets: SmallVec<ChangeId>,
    source_mode: RebaseSource,
    flags: CommandFlags,
) -> Vec<FollowUpOption> {
    RebaseKind::iter()
        .map(|kind| FollowUpOption {
            key: kind.key(),
            label: kind.label(),
            action: FollowUpAction::Execute(JJCommand {
                kind: JJCommandKind::Rebase {
                    change_ids: sources.clone(),
                    source_mode: source_mode.clone(),
                    dest: RebaseTarget {
                        targets: targets.clone(),
                        kind,
                    },
                },
                flags,
            }),
        })
        .collect()
}
