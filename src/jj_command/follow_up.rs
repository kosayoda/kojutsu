use strum::IntoEnumIterator;

use crate::keymap::CommandFlags;
use crate::types::{
    ChangeSelection, CommandPrompt, CommitId, MessageMode, PendingCommitSelect, ReadyCommand,
    RebaseKind, RebaseSource, RebaseTarget, RepoPath, RevisionArg, SmallVec, SmallVec1,
    SplitTarget, SquashTarget, Str, TargetOperation, TextPrompt,
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
        pending: TextPrompt,
    },
    WidenRevset {
        change_id: String,
    },
    /// Stage the resolution content and run `jj resolve`. The content is
    /// carried (not a temp file) so declining the prompt leaves nothing to
    /// clean up; the file is written only when this executes.
    ResolveConflict {
        change_id: RevisionArg,
        path: RepoPath,
        content: String,
        flags: CommandFlags,
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
        // Every option here retries with a jj flag, which a foreign program
        // would reject. It is matched on output text, so without this a `git`
        // command that happened to say "immutable" would be offered one.
        if matches!(self.kind, super::JJCommandKind::Exec { .. }) {
            return Vec::new();
        }
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
    pub fn into_jj_command(self, target: RevisionArg, flags: CommandFlags) -> JJCommand {
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

impl TargetOperation {
    /// The commands on offer once `targets` are picked for `sources`. An
    /// operation that doesn't [take many sources](Self::takes_many_sources)
    /// is given exactly one.
    pub fn follow_up(
        self,
        sources: SmallVec1<RevisionArg>,
        targets: SmallVec1<RevisionArg>,
        flags: CommandFlags,
        selection: ChangeSelection,
    ) -> Vec<FollowUpOption> {
        let source = sources.first().clone();
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
            TargetOperation::Rebase { source_mode } => rebase_follow_up(
                sources.into_smallvec(),
                targets.into_smallvec(),
                source_mode,
                flags,
            ),
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
            TargetOperation::DuplicateOnto => auto_follow_up(
                "duplicate",
                JJCommand {
                    kind: JJCommandKind::Duplicate {
                        change_ids: sources.into_smallvec(),
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
            TargetOperation::Revert => RebaseKind::iter()
                .map(|kind| FollowUpOption {
                    key: kind.key(),
                    label: kind.label(),
                    action: FollowUpAction::Execute(JJCommand {
                        kind: JJCommandKind::Revert {
                            change_ids: sources.clone().into_smallvec(),
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
    source: RevisionArg,
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
                pending: CommandPrompt::SquashWithMessage { builder, flags }.into(),
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
    sources: SmallVec<RevisionArg>,
    targets: SmallVec<RevisionArg>,
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
