mod args;
mod capture;
pub mod completion;
mod follow_up;
mod run;

pub use capture::{Captured, Stream};
pub use completion::Completion;
pub use completion::{common_prefix, complete, replace_current_token, split_for_completion};
pub use follow_up::{FollowUpAction, FollowUpOption, offerable};

use std::sync::{Arc, atomic::AtomicI32, atomic::Ordering};

use crate::dag::{BookmarkRef, TagRef};
use crate::jj_version::{InstalledJj, JjFeature};
use crate::keymap::CommandFlags;
use crate::types::{
    BookmarkName, ChangeId, ChangeSelection, JumpTarget, MessageMode, OperationId, RebaseSource,
    RebaseTarget, RemoteName, RevisionArg, SmallVec, SplitTarget, SquashKind, SquashTarget, Str,
    TagName, WorkspaceName,
};

#[derive(Clone)]
pub struct KillHandle(Arc<AtomicI32>);

impl KillHandle {
    pub fn pair() -> (Self, Self) {
        let arc = Arc::new(AtomicI32::new(0));
        (Self(Arc::clone(&arc)), Self(arc))
    }

    pub(crate) fn set_pgid(&self, pgid: i32) {
        self.0.store(pgid, Ordering::Release);
    }

    pub fn kill(&self) {
        let pgid = self.0.load(Ordering::Acquire);
        if pgid > 0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InsertPosition {
    After,
    Before,
}

impl InsertPosition {
    fn flag(self) -> &'static str {
        match self {
            InsertPosition::After => "--insert-after",
            InsertPosition::Before => "--insert-before",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JJCommand {
    pub kind: JJCommandKind,
    pub flags: CommandFlags,
}

#[derive(Debug, Clone)]
pub enum JJCommandKind {
    Abandon {
        change_ids: SmallVec<RevisionArg>,
    },
    Describe {
        change_ids: SmallVec<RevisionArg>,
        message: String,
    },
    DescribeInEditor {
        change_id: RevisionArg,
    },
    Diffedit {
        change_id: RevisionArg,
        selection: ChangeSelection,
    },
    Edit {
        change_id: RevisionArg,
    },
    New {
        change_ids: SmallVec<RevisionArg>,
        insert: Option<InsertPosition>,
    },
    Squash {
        change_id: RevisionArg,
        target: Option<SquashTarget>,
        message: MessageMode,
        selection: ChangeSelection,
    },
    Rebase {
        change_ids: SmallVec<RevisionArg>,
        source_mode: RebaseSource,
        dest: RebaseTarget,
    },
    Restore {
        from: Option<RevisionArg>,
        into: Option<RevisionArg>,
        changes_in: Option<RevisionArg>,
        selection: ChangeSelection,
    },
    Split {
        change_id: RevisionArg,
        target: Option<SplitTarget>,
        selection: ChangeSelection,
    },
    BookmarkCreate {
        name: BookmarkName,
        change_id: RevisionArg,
    },
    BookmarkSet {
        name: BookmarkName,
        change_id: RevisionArg,
    },
    BookmarkDelete {
        names: SmallVec<BookmarkName>,
    },
    BookmarkForget {
        names: SmallVec<BookmarkName>,
    },
    BookmarkMove {
        name: BookmarkName,
        target: RevisionArg,
    },
    BookmarkRename {
        old_name: BookmarkName,
        new_name: BookmarkName,
    },
    BookmarkAdvance {
        change_id: Option<RevisionArg>,
    },
    BookmarkTrack {
        bookmarks: SmallVec<BookmarkRef>,
    },
    BookmarkUntrack {
        bookmarks: SmallVec<BookmarkRef>,
    },
    Undo,
    Redo,
    GitFetch {
        all_remotes: bool,
        remote: Option<RemoteName>,
    },
    GitPush {
        all: bool,
        remote: Option<RemoteName>,
    },
    GitPushChange {
        change_id: RevisionArg,
        remote: Option<RemoteName>,
    },
    GitPushBookmark {
        bookmarks: SmallVec<BookmarkName>,
        remote: Option<RemoteName>,
    },
    GitFetchBookmark {
        bookmark: BookmarkName,
        remote: RemoteName,
    },
    GitExport,
    GitImport,
    Absorb {
        from: Option<RevisionArg>,
        selection: ChangeSelection,
    },
    Commit {
        message: Option<String>,
        selection: ChangeSelection,
    },
    Duplicate {
        change_ids: SmallVec<RevisionArg>,
        onto: Option<RevisionArg>,
    },
    Parallelize {
        change_ids: SmallVec<RevisionArg>,
    },
    SimplifyParents {
        change_ids: SmallVec<RevisionArg>,
    },
    Revert {
        change_ids: SmallVec<RevisionArg>,
        dest: RebaseTarget,
    },
    WorkspaceAdd {
        path: String,
        name: Option<WorkspaceName>,
        revision: RevisionArg,
    },
    WorkspaceForget {
        names: SmallVec<WorkspaceName>,
    },
    WorkspaceList,
    WorkspaceRename {
        new_name: WorkspaceName,
    },
    TagSet {
        name: TagName,
        change_id: RevisionArg,
    },
    TagDelete {
        names: SmallVec<TagName>,
    },
    TagTrack {
        tags: SmallVec<TagRef>,
    },
    /// Replace each divergent change's copies with one commit. Empty:
    /// whichever changes jj's own `converge` revset finds.
    Converge {
        changes: SmallVec<ChangeId>,
    },
    TagUntrack {
        tags: SmallVec<TagRef>,
    },
    OpRestore {
        op_id: OperationId,
    },
    OpRevert {
        op_id: OperationId,
    },
    OpAbandon {
        op_id: OperationId,
    },
    Fix {
        change_ids: SmallVec<RevisionArg>,
        selection: ChangeSelection,
    },
    Run {
        change_ids: SmallVec<RevisionArg>,
        /// The command to run in each private working copy (already
        /// shlex-split; jj executes it without a shell).
        argv: Vec<Str>,
        /// Parallelism (`--jobs`); `None` lets jj resolve `run.jobs`.
        jobs: Option<usize>,
    },
    FileUntrack {
        paths: SmallVec<Str>,
    },
    Resolve {
        change_id: RevisionArg,
        path: Str,
        tool: ResolveTool,
    },
    Raw {
        args: Vec<Str>,
    },
    /// Not jj at all: a program a plugin asked for, spawned in the workspace
    /// root. It runs through the same pipeline so it is cancellable and shows
    /// up in the command log like everything else, but none of jj's global
    /// flags apply to it.
    Exec {
        program: Str,
        args: Vec<Str>,
    },
}

#[derive(Debug, Clone)]
pub enum ResolveTool {
    Ours,
    Theirs,
    Default,
    /// Apply pre-resolved content from a file (written by the per-hunk
    /// picker) via a merge tool that invokes kojutsu itself.
    Content(std::path::PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandPartKind {
    Prompt,
    Binary,
    Subcommand,
    Flag,
    Revision,
    String,
    Fileset,
}

#[derive(Clone)]
pub struct CommandPart {
    pub text: String,
    pub kind: CommandPartKind,
}

pub struct JJCommandResult {
    pub display: String,
    pub display_parts: Vec<CommandPart>,
    pub output: Captured,
    pub success: bool,
    /// The command was killed (Esc/^C) rather than running to completion.
    pub cancelled: bool,
    /// Exit code, when the command ran to completion.
    pub code: Option<i32>,
}

/// How a command uses the terminal while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalUse {
    /// Output captured on a background thread while the TUI stays up.
    Background,
    /// Output captured, but the TUI steps aside so the child has the real
    /// stdin.
    Foreground,
    /// The child takes the whole terminal: an editor or a diff editor.
    Interactive,
    /// The child writes straight to the terminal, as for `Interactive`, and
    /// the screen is held once it exits so what it wrote can be read.
    Passthrough,
}

impl JJCommand {
    pub fn with_flag(mut self, flag: CommandFlags) -> Self {
        self.flags |= flag;
        self
    }

    /// The global toggles this command takes; the rest are dropped from its
    /// command line however they were set. A foreign program takes none of
    /// jj's. `--ignore-immutable` is for rewriting what the user aimed a
    /// command at, but all a fetch or import aims at is the remote: there it
    /// only lets jj rebase other people's pushed commits onto new versions of
    /// their changes, leaving local, bookmarkless copies of them behind.
    pub fn global_flags(&self) -> CommandFlags {
        let all = crate::types::GLOBAL_TOGGLES
            .iter()
            .fold(CommandFlags::empty(), |all, toggle| all | toggle.flag);
        match &self.kind {
            JJCommandKind::Exec { .. } => CommandFlags::empty(),
            _ if self.imports_from_git() => all.difference(CommandFlags::IGNORE_IMMUTABLE),
            _ => all,
        }
    }

    /// Whether the command brings commits in from git, typed or not.
    fn imports_from_git(&self) -> bool {
        match &self.kind {
            JJCommandKind::GitFetch { .. }
            | JJCommandKind::GitFetchBookmark { .. }
            | JJCommandKind::GitImport => true,
            JJCommandKind::Raw { args } => matches!(
                args.as_slice(),
                [git, sub, ..] if git == "git" && (sub == "fetch" || sub == "import")
            ),
            _ => false,
        }
    }

    /// Where the cursor should go once the command has run, when the
    /// command itself decides that. Commands that move `@` take the cursor
    /// with them, and squashing into a named commit follows the content
    /// there. Everything else rewrites commits in place or out of sight, and
    /// leaves the cursor to find the revision it was on (or `@`, if it was
    /// on `@`).
    pub fn jump_target(&self) -> Option<JumpTarget> {
        match &self.kind {
            JJCommandKind::New { .. }
            | JJCommandKind::Edit { .. }
            | JJCommandKind::Commit { .. } => Some(JumpTarget::WorkingCopy),
            JJCommandKind::Squash {
                target:
                    Some(SquashTarget {
                        target,
                        kind: SquashKind::Into,
                    }),
                ..
            } => Some(JumpTarget::Revision(target.clone())),
            JJCommandKind::BookmarkTrack { bookmarks, .. } => bookmarks
                .first()
                .map(|br| JumpTarget::Bookmark(br.name.clone())),
            JJCommandKind::BookmarkUntrack { bookmarks, .. } => bookmarks
                .first()
                .map(|br| JumpTarget::Bookmark(br.name.clone())),
            JJCommandKind::BookmarkCreate { name, .. }
            | JJCommandKind::BookmarkSet { name, .. }
            | JJCommandKind::BookmarkMove { name, .. } => Some(JumpTarget::Bookmark(name.clone())),
            JJCommandKind::BookmarkRename { new_name, .. } => {
                Some(JumpTarget::Bookmark(new_name.clone()))
            }
            _ => None,
        }
    }

    /// The program this spawns, and the arguments to spawn it with. jj gets the
    /// globals every kojutsu invocation carries; an `Exec` gets nothing added,
    /// since `-R` and `--color` mean nothing to it.
    fn spawn_parts(&self, repo_path: &std::path::Path) -> (Str, Vec<Str>) {
        if let JJCommandKind::Exec { program, args } = &self.kind {
            return (program.clone(), args.clone());
        }
        let mut args: Vec<Str> = vec![
            "-R".into(),
            repo_path.display().to_string().into(),
            "--color=always".into(),
        ];
        args.extend(self.args());
        ("jj".into(), args)
    }

    /// The child process to run, configured but not yet spawned. Callers set
    /// their own stdio and then spawn it.
    ///
    /// Every process kojutsu starts is built here, and recorded here, so the
    /// debug log is a complete account of them. That matters for the ones the
    /// UI never mentions: a plugin's `quiet` call deliberately leaves no
    /// running overlay and no command-log row, and "invisible on screen" should
    /// not also mean "no way to find out what ran".
    fn command(&self, repo_path: &std::path::Path) -> std::process::Command {
        let (program, args) = self.spawn_parts(repo_path);
        tracing::info!(command = self.display().as_str(), "spawning");
        let mut command = std::process::Command::new(program.as_str());
        command.args(args.iter().map(|a| a.as_str()));
        command.current_dir(repo_path);
        command
    }

    /// The binary shown in the command log. Foreign programs name themselves.
    fn binary(&self) -> &str {
        match &self.kind {
            JJCommandKind::Exec { program, .. } => program.as_str(),
            _ => "jj",
        }
    }

    pub fn display(&self) -> String {
        self.display_parts()
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn display_parts(&self) -> Vec<CommandPart> {
        let mut parts = vec![
            CommandPart {
                text: "$".into(),
                kind: CommandPartKind::Prompt,
            },
            CommandPart {
                text: self.binary().to_string(),
                kind: CommandPartKind::Binary,
            },
        ];
        parts.extend(self.tagged_args().into_iter().map(
            |args::TaggedArg { text, kind, .. }| {
                // Quote whatever a shell would need quoted, whichever kind it is:
                // the log line is there to be read and pasted, and an argument that
                // arrived as one word has to leave as one word. jj's own flags and
                // subcommands never need it; a program's arguments can.
                let text = if text
                    .contains(|c: char| c.is_whitespace() || "\"'\\$`!#&|;(){}".contains(c))
                {
                    shlex::try_quote(&text)
                        .map(|q| q.into_owned())
                        .unwrap_or_else(|_| text.to_string())
                } else {
                    text.to_string()
                };
                CommandPart { text, kind }
            },
        ));
        parts
    }

    /// The jj features this command's line uses, read off the built args
    /// for the same reason as `--interactive`: the builder is the one place
    /// that knows every flag it added.
    pub fn features(&self) -> impl Iterator<Item = JjFeature> {
        self.tagged_args().into_iter().filter_map(|arg| arg.needs)
    }

    /// The newest feature this command uses that `jj` predates, if any.
    pub fn unsupported_by(&self, jj: InstalledJj) -> Option<JjFeature> {
        self.features()
            .filter(|&feature| !jj.supports(feature))
            .max_by_key(|feature| feature.since())
    }

    /// How the command uses the terminal while it runs: the one place that
    /// decides whether the TUI steps aside for it.
    pub fn terminal_use(&self) -> TerminalUse {
        // Read off the args for the same reason as `--interactive`.
        if self.args().iter().any(|a| a.as_str() == "--passthrough") {
            return TerminalUse::Passthrough;
        }
        if self.is_interactive() {
            return TerminalUse::Interactive;
        }
        match &self.kind {
            // Talking to a remote may prompt for an SSH passphrase or
            // credentials on stdin.
            JJCommandKind::GitFetch { .. }
            | JJCommandKind::GitFetchBookmark { .. }
            | JJCommandKind::GitPush { .. }
            | JJCommandKind::GitPushChange { .. }
            | JJCommandKind::GitPushBookmark { .. } => TerminalUse::Foreground,
            _ => TerminalUse::Background,
        }
    }

    /// Whether the command needs the terminal handed to it, rather than being
    /// run with its output captured.
    ///
    /// Two reasons, handled differently on purpose. A `--interactive` diff
    /// editor is read back off the built args: several kinds pick that flag up
    /// from `CommandFlags::INTERACTIVE` or from a line-level selection, and
    /// restating those rules here is how the two copies drift: a kind that
    /// gained the flag without gaining a case would launch a diff editor
    /// against a closed stdin. Opening `$EDITOR` can't be read off the args,
    /// since it's the *absence* of `-m` or a tool default, so those stay
    /// listed; a miss there is milder, because captured runs set
    /// `JJ_EDITOR=:` and the edit is simply skipped.
    fn is_interactive(&self) -> bool {
        if self.args().iter().any(|a| a.as_str() == "--interactive") {
            return true;
        }
        match &self.kind {
            JJCommandKind::DescribeInEditor { .. }
            | JJCommandKind::Diffedit { .. }
            | JJCommandKind::Split { .. } => true,
            JJCommandKind::Squash { message, .. } => matches!(message, MessageMode::Default),
            // Asks on the terminal when its heuristics can't decide, and
            // may open $EDITOR to merge descriptions.
            JJCommandKind::Converge { .. } => !self.flags.contains(CommandFlags::NO_INTERACTIVE),
            JJCommandKind::Commit { message, .. } => message.is_none(),
            JJCommandKind::Resolve { tool, .. } => matches!(tool, ResolveTool::Default),
            // A command line the user typed; assume it may want the terminal.
            JJCommandKind::Raw { .. } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    fn exec(argv: &[&str]) -> JJCommand {
        let (program, args) = argv.split_first().expect("a program");
        JJCommand {
            kind: JJCommandKind::Exec {
                program: (*program).into(),
                args: args.iter().map(|a| (*a).into()).collect(),
            },
            flags: CommandFlags::empty(),
        }
    }

    /// jj's globals are jj's. Handing `-R` and `--color=always` to a foreign
    /// program would at best confuse it and at worst mean something else.
    #[test]
    fn an_exec_gets_none_of_jjs_global_flags() {
        let repo = std::path::Path::new("/repo");
        let (program, args) = exec(&["git", "status", "--porcelain"]).spawn_parts(repo);
        assert_eq!(program, "git");
        assert_eq!(args, ["status", "--porcelain"]);

        // For contrast, a jj command line carries them.
        let raw = JJCommand {
            kind: JJCommandKind::Raw {
                args: vec!["log".into()],
            },
            flags: CommandFlags::empty(),
        };
        let (program, args) = raw.spawn_parts(repo);
        assert_eq!(program, "jj");
        assert_eq!(args, ["-R", "/repo", "--color=always", "log"]);
    }

    /// The command log is a record of what ran, so it has to name the program
    /// that ran rather than claiming everything was jj.
    #[test]
    fn an_exec_names_itself_in_the_log() {
        assert_eq!(
            exec(&["git", "submodule", "status"]).display(),
            "$ git submodule status"
        );
        // An argument that would need quoting to be pasted back gets it.
        assert_eq!(
            exec(&["git", "log", "--format=a b"]).display(),
            "$ git log '--format=a b'"
        );
    }

    /// Every retry offered is a jj flag, so there is nothing to offer here even
    /// when the output happens to contain a word one of them keys off.
    #[test]
    fn an_exec_is_never_offered_a_jj_retry_flag() {
        let cmd = exec(&["git", "push"]);
        assert!(
            cmd.retry_options(
                b"refusing to rewrite immutable commit",
                InstalledJj::default()
            )
            .is_empty()
        );

        let raw = JJCommand {
            kind: JJCommandKind::Raw {
                args: vec!["describe".into()],
            },
            flags: CommandFlags::empty(),
        };
        assert!(
            !raw.retry_options(b"immutable", InstalledJj::default())
                .is_empty()
        );
    }

    /// Nothing foreign should be assumed to want the terminal, and nothing
    /// about it tells the DAG where to move the cursor.
    #[test]
    fn an_exec_neither_takes_the_terminal_nor_moves_the_cursor() {
        let cmd = exec(&["git", "status"]);
        assert!(!cmd.is_interactive());
        assert!(cmd.jump_target().is_none());
    }
}

#[cfg(test)]
mod is_interactive_tests {
    use super::*;
    use crate::types::{ChangeSelection, MessageMode, RevisionArg};

    fn squash(message: MessageMode, selection: ChangeSelection, flags: CommandFlags) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Squash {
                change_id: RevisionArg::new("qpvuntsm"),
                target: None,
                message,
                selection,
            },
            flags,
        }
    }

    #[test]
    fn a_captured_squash_needs_no_terminal() {
        let cmd = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::All,
            CommandFlags::empty(),
        );
        assert!(!cmd.args().iter().any(|a| a.as_str() == "--interactive"));
        assert!(!cmd.is_interactive());
    }

    /// Git talking to a remote runs captured but in the foreground, where an
    /// SSH passphrase prompt can reach the user; local git plumbing doesn't.
    #[test]
    fn remote_git_commands_take_the_foreground() {
        let cmd = |kind| JJCommand {
            kind,
            flags: CommandFlags::empty(),
        };
        let push = cmd(JJCommandKind::GitPush {
            all: false,
            remote: None,
        });
        assert_eq!(push.terminal_use(), TerminalUse::Foreground);
        assert_eq!(
            cmd(JJCommandKind::GitExport).terminal_use(),
            TerminalUse::Background
        );
        let editor_commit = cmd(JJCommandKind::Commit {
            message: None,
            selection: ChangeSelection::All,
        });
        assert_eq!(editor_commit.terminal_use(), TerminalUse::Interactive);
    }

    /// The reason to read the args rather than restate the rules: every way a
    /// command can acquire --interactive is covered by construction.
    #[test]
    fn anything_passing_interactive_is_interactive() {
        let by_flag = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::All,
            CommandFlags::INTERACTIVE,
        );
        assert!(by_flag.is_interactive());

        let by_selection = squash(
            MessageMode::Inline("m".into()),
            ChangeSelection::Lines(std::path::PathBuf::from("/tmp/sel.json")),
            CommandFlags::empty(),
        );
        assert!(by_selection.is_interactive());
    }

    /// Opening $EDITOR isn't visible in the args: it's the absence of -m.
    #[test]
    fn an_editor_message_still_needs_the_terminal() {
        let cmd = squash(
            MessageMode::Default,
            ChangeSelection::All,
            CommandFlags::empty(),
        );
        assert!(!cmd.args().iter().any(|a| a.as_str() == "--interactive"));
        assert!(cmd.is_interactive());
    }
}

#[cfg(test)]
mod jump_target_tests {
    use super::*;
    use crate::types::{ChangeSelection, MessageMode, RevisionArg};

    fn cmd(kind: JJCommandKind) -> JJCommand {
        JJCommand {
            kind,
            flags: CommandFlags::empty(),
        }
    }

    fn squash(target: Option<SquashTarget>) -> JJCommand {
        cmd(JJCommandKind::Squash {
            change_id: RevisionArg::new("qpv"),
            target,
            message: MessageMode::Default,
            selection: ChangeSelection::All,
        })
    }

    #[test]
    fn moving_at_takes_the_cursor_to_at() {
        let edit = cmd(JJCommandKind::Edit {
            change_id: RevisionArg::new("qpv"),
        });
        assert!(matches!(edit.jump_target(), Some(JumpTarget::WorkingCopy)));
    }

    /// The content lands in the named commit, so the cursor follows it.
    #[test]
    fn squashing_into_a_commit_follows_the_content() {
        let into = squash(Some(SquashTarget {
            target: RevisionArg::new("zzz"),
            kind: SquashKind::Into,
        }));
        assert!(matches!(into.jump_target(), Some(JumpTarget::Revision(r)) if r.as_str() == "zzz"));
    }

    /// Rewrites leave the cursor to find the revision it was on.
    #[test]
    fn other_rewrites_set_no_jump() {
        let onto = squash(Some(SquashTarget {
            target: RevisionArg::new("zzz"),
            kind: SquashKind::Onto,
        }));
        let abandon = cmd(JJCommandKind::Abandon {
            change_ids: SmallVec::from_elem(RevisionArg::new("qpv"), 1),
        });
        for command in [squash(None), onto, abandon] {
            assert!(command.jump_target().is_none(), "{}", command.display());
        }
    }
}

#[cfg(test)]
mod remote_ref_args_tests {
    use super::*;
    use crate::types::BookmarkName;

    /// Each remote bookmark goes to jj as one exact symbol. As separate names
    /// and `--remote` values, jj would track every name on every remote.
    #[test]
    fn tracking_names_each_remote_bookmark_exactly() {
        let cmd = JJCommand {
            kind: JJCommandKind::BookmarkTrack {
                bookmarks: [("main", "origin"), ("feat x", "upstream")]
                    .into_iter()
                    .map(|(name, remote)| BookmarkRef {
                        name: BookmarkName::new(name),
                        remote: RemoteName::new(remote),
                    })
                    .collect(),
            },
            flags: CommandFlags::empty(),
        };
        let args: Vec<String> = cmd.args().iter().map(|a| a.to_string()).collect();
        assert_eq!(
            args,
            ["bookmark", "track", "main@origin", "\"feat x\"@upstream"]
        );
    }

    #[test]
    fn tracking_a_tag_names_it_exactly_too() {
        let cmd = JJCommand {
            kind: JJCommandKind::TagTrack {
                tags: smallvec::smallvec![TagRef {
                    name: crate::types::TagName::new("v1.0"),
                    remote: RemoteName::new("origin"),
                }],
            },
            flags: CommandFlags::empty(),
        };
        let args: Vec<String> = cmd.args().iter().map(|a| a.to_string()).collect();
        assert_eq!(args, ["tag", "track", "v1.0@origin"]);
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;

    fn run(flags: CommandFlags, jobs: Option<usize>) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Run {
                change_ids: smallvec::smallvec![RevisionArg::new("qpv"), RevisionArg::new("xyz")],
                argv: vec!["cargo".into(), "test".into()],
                jobs,
            },
            flags,
        }
    }

    fn args(cmd: &JJCommand) -> Vec<String> {
        cmd.args().iter().map(|a| a.to_string()).collect()
    }

    /// The new options are jj's, so they go before the `--` that hands the
    /// rest to the command being run.
    #[test]
    fn run_options_go_to_jj_not_to_the_command() {
        let cmd = run(
            CommandFlags::IGNORE_CHANGES | CommandFlags::IGNORE_ERRORS,
            None,
        );
        let args = args(&cmd);
        let dashes = args.iter().position(|a| a == "--").expect("a --");
        for option in ["--ignore-changes", "--ignore-errors"] {
            let at = args.iter().position(|a| a == option).expect(option);
            assert!(at < dashes, "{option} after --: {args:?}");
        }
        assert_eq!(cmd.terminal_use(), TerminalUse::Background);
    }

    /// Passthrough output goes to the terminal, so the TUI steps aside for
    /// it and holds the screen afterwards.
    #[test]
    fn a_passthrough_run_takes_the_terminal() {
        let cmd = run(CommandFlags::PASSTHROUGH, Some(1));
        assert!(args(&cmd).windows(2).any(|w| w == ["--jobs", "1"]));
        assert_eq!(cmd.terminal_use(), TerminalUse::Passthrough);
    }
}

#[cfg(test)]
mod absorb_tests {
    use super::*;
    use crate::types::ChangeSelection;

    /// A line selection reaches `jj absorb` the way it reaches squash: as
    /// kojutsu's own diff tool, which keeps only the selected lines.
    #[test]
    fn absorbing_lines_goes_through_the_diff_tool() {
        let cmd = JJCommand {
            kind: JJCommandKind::Absorb {
                from: Some(RevisionArg::new("qpv")),
                selection: ChangeSelection::Lines(std::path::PathBuf::from("/tmp/sel.json")),
            },
            flags: CommandFlags::empty(),
        };
        let args: Vec<String> = cmd.args().iter().map(|a| a.to_string()).collect();
        assert!(args.contains(&"--interactive".to_string()), "{args:?}");
        assert!(
            args.windows(2).any(|w| w == ["--tool", "kojutsu-select"]),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a.contains("/tmp/sel.json")), "{args:?}");
    }
}

#[cfg(test)]
mod converge_tests {
    use super::*;

    fn converge(changes: &[&str], flags: CommandFlags) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Converge {
                changes: changes.iter().map(|c| ChangeId::new(*c)).collect(),
            },
            flags,
        }
    }

    fn args(cmd: &JJCommand) -> Vec<String> {
        cmd.args().iter().map(|a| a.to_string()).collect()
    }

    /// Each change is named by `change_id()`, which takes in all its copies
    /// wherever they sit, rather than one copy's revision.
    #[test]
    fn converge_is_scoped_to_whole_changes() {
        let cmd = converge(&["twznmtor", "tnklypyx"], CommandFlags::empty());
        assert_eq!(
            args(&cmd),
            [
                "converge",
                "-r",
                "change_id(twznmtor)",
                "-r",
                "change_id(tnklypyx)"
            ]
        );
    }

    /// Converge prompts on the terminal when it can't decide, so it gets
    /// the terminal; told not to prompt, it can run in the background.
    #[test]
    fn converge_takes_the_terminal_unless_told_not_to_prompt() {
        assert_eq!(
            converge(&["x"], CommandFlags::empty()).terminal_use(),
            TerminalUse::Interactive
        );
        let quiet = converge(&["x"], CommandFlags::NO_INTERACTIVE);
        assert!(args(&quiet).contains(&"--no-interactive".to_string()));
        assert_eq!(quiet.terminal_use(), TerminalUse::Background);
    }
}

#[cfg(test)]
mod push_retry_tests {
    use super::*;

    fn push() -> JJCommand {
        JJCommand {
            kind: JJCommandKind::GitPush {
                all: false,
                remote: None,
            },
            flags: CommandFlags::empty(),
        }
    }

    fn retried_flags(cmd: &JJCommand, output: &str) -> Option<CommandFlags> {
        let options = cmd.retry_options(output.as_bytes(), InstalledJj::default());
        match options.as_slice() {
            [] => None,
            [only] => match &only.action {
                FollowUpAction::Execute(retry) => Some(retry.flags),
                _ => panic!("expected a retry command"),
            },
            _ => panic!("expected at most one retry"),
        }
    }

    #[test]
    fn a_push_refused_for_conflicts_offers_to_allow_them() {
        let flags = retried_flags(
            &push(),
            "Error: Won't push commit 1234abcd since it has conflicts\n",
        );
        assert_eq!(flags, Some(CommandFlags::ALLOW_CONFLICTS));
        let retry = push().with_flag(CommandFlags::ALLOW_CONFLICTS);
        assert!(
            retry
                .args()
                .iter()
                .any(|a| a.as_str() == "--allow-conflicts")
        );
    }

    /// Allowing only one of two reasons would just be refused again.
    #[test]
    fn every_reason_given_is_allowed_together() {
        let flags = retried_flags(
            &push(),
            "Error: Won't push bookmark main: commit 1234abcd has no description and has conflicts\n",
        );
        assert_eq!(
            flags,
            Some(CommandFlags::ALLOW_CONFLICTS | CommandFlags::ALLOW_EMPTY_DESCRIPTION)
        );
    }

    /// The wording is matched as text, so only a push is offered a push flag.
    #[test]
    fn only_a_push_is_offered_the_push_override() {
        let new = JJCommand {
            kind: JJCommandKind::Raw {
                args: vec!["log".into()],
            },
            flags: CommandFlags::empty(),
        };
        assert_eq!(
            retried_flags(&new, "Won't push commit x since it has conflicts"),
            None
        );
    }

    /// A private commit is set in config; no flag gets it pushed.
    #[test]
    fn a_private_commit_offers_nothing() {
        let flags = retried_flags(
            &push(),
            "Error: Won't push commit 1234abcd since it is private\n",
        );
        assert_eq!(flags, None);
    }
}

#[cfg(test)]
mod ref_move_tests {
    use super::*;

    fn tag_set(flags: CommandFlags) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::TagSet {
                name: TagName::new("v1"),
                change_id: RevisionArg::new("x"),
            },
            flags,
        }
    }

    fn bookmark_set() -> JJCommand {
        JJCommand {
            kind: JJCommandKind::BookmarkSet {
                name: BookmarkName::new("main"),
                change_id: RevisionArg::new("x"),
            },
            flags: CommandFlags::empty(),
        }
    }

    fn retry(cmd: &JJCommand, output: &str) -> Option<JJCommand> {
        cmd.retry_options(output.as_bytes(), InstalledJj::default())
            .into_iter()
            .find_map(|option| match option.action {
                FollowUpAction::Execute(retry) => Some(retry),
                _ => None,
            })
    }

    /// jj's flag for tags is `--allow-move`; `--allow-backwards` is a
    /// bookmark flag, and `tag set` rejects it as an unexpected argument.
    #[test]
    fn tag_set_allows_a_move_with_its_own_flag() {
        let args = tag_set(CommandFlags::ALLOW_MOVE).args();
        assert!(args.contains(&"--allow-move".into()), "{args:?}");
        let args = tag_set(CommandFlags::ALLOW_BACKWARDS).args();
        assert!(!args.contains(&"--allow-backwards".into()), "{args:?}");
    }

    #[test]
    fn a_refused_tag_move_offers_to_allow_it() {
        let output =
            "Error: Refusing to move tag: v1\nHint: Use --allow-move to update existing tags.\n";
        let retried = retry(&tag_set(CommandFlags::empty()), output).expect("a retry");
        assert!(retried.args().contains(&"--allow-move".into()));
        // Already allowed: the same command would only fail again.
        assert!(retry(&tag_set(CommandFlags::ALLOW_MOVE), output).is_none());
    }

    #[test]
    fn a_refused_bookmark_move_offers_to_allow_backwards() {
        let output = "Error: Refusing to move bookmark backwards or sideways: main\n";
        let retried = retry(&bookmark_set(), output).expect("a retry");
        assert!(retried.args().contains(&"--allow-backwards".into()));
        // Each kind answers only its own refusal.
        assert!(retry(&tag_set(CommandFlags::empty()), output).is_none());
    }
}

#[cfg(test)]
mod global_flag_tests {
    use super::*;

    const TOGGLES: CommandFlags = CommandFlags::IGNORE_IMMUTABLE
        .union(CommandFlags::IGNORE_WORKING_COPY)
        .union(CommandFlags::DEBUG);

    fn cmd(kind: JJCommandKind, flags: CommandFlags) -> JJCommand {
        JJCommand { kind, flags }
    }

    fn raw(args: &[&str], flags: CommandFlags) -> JJCommand {
        cmd(
            JJCommandKind::Raw {
                args: args.iter().map(|a| (*a).into()).collect(),
            },
            flags,
        )
    }

    fn args(cmd: &JJCommand) -> Vec<String> {
        cmd.args().iter().map(|a| a.to_string()).collect()
    }

    fn fetches() -> [JJCommand; 4] {
        [
            cmd(
                JJCommandKind::GitFetch {
                    all_remotes: false,
                    remote: None,
                },
                TOGGLES,
            ),
            cmd(
                JJCommandKind::GitFetchBookmark {
                    bookmark: BookmarkName::new("b"),
                    remote: RemoteName::new("origin"),
                },
                TOGGLES,
            ),
            cmd(JJCommandKind::GitImport, TOGGLES),
            raw(&["git", "fetch"], TOGGLES),
        ]
    }

    /// With the toggle left on, a fetch would rebase other people's pushed
    /// commits onto new versions of their changes. The other toggles still
    /// apply.
    #[test]
    fn a_fetch_never_ignores_immutability() {
        for fetch in fetches() {
            let args = args(&fetch);
            assert!(!args.contains(&"--ignore-immutable".into()), "{args:?}");
            assert!(args.contains(&"--ignore-working-copy".into()), "{args:?}");
            assert!(args.contains(&"--debug".into()), "{args:?}");
        }
    }

    #[test]
    fn a_fetch_refused_over_immutable_commits_offers_no_retry() {
        for fetch in fetches() {
            assert!(
                fetch
                    .retry_options(
                        b"Error: Commit 1234abcd is immutable",
                        InstalledJj::default()
                    )
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_rewrite_keeps_the_toggle() {
        let describe = cmd(
            JJCommandKind::Describe {
                change_ids: SmallVec::from_elem(RevisionArg::new("x"), 1),
                message: "m".into(),
            },
            CommandFlags::IGNORE_IMMUTABLE,
        );
        assert!(args(&describe).contains(&"--ignore-immutable".into()));
    }

    /// The retry offered for a typed command has to change its command line,
    /// or it is the same command run again.
    #[test]
    fn a_typed_commands_retry_carries_the_flag_ahead_of_passthrough() {
        let typed = raw(
            &["run", "-r", "x", "--", "echo", "hi"],
            CommandFlags::empty(),
        );
        let [retry] = typed
            .retry_options(
                b"Error: Commit 1234abcd is immutable",
                InstalledJj::default(),
            )
            .try_into()
            .unwrap_or_else(|_| panic!("expected one retry"));
        let FollowUpAction::Execute(retry) = retry.action else {
            panic!("expected a retry command");
        };
        assert_eq!(
            args(&retry),
            ["run", "-r", "x", "--ignore-immutable", "--", "echo", "hi"]
        );
    }
}

#[cfg(test)]
mod jj_feature_tests {
    use super::*;
    use crate::jj_version::JjVersion;
    use strum::IntoEnumIterator;

    fn cmd(kind: JJCommandKind, flags: CommandFlags) -> JJCommand {
        JJCommand { kind, flags }
    }

    fn run(flags: CommandFlags) -> JJCommand {
        cmd(
            JJCommandKind::Run {
                change_ids: smallvec::smallvec![RevisionArg::new("x")],
                argv: vec!["make".into()],
                jobs: None,
            },
            flags,
        )
    }

    fn absorb(selection: ChangeSelection) -> JJCommand {
        cmd(
            JJCommandKind::Absorb {
                from: None,
                selection,
            },
            CommandFlags::empty(),
        )
    }

    fn push(flags: CommandFlags) -> JJCommand {
        cmd(
            JJCommandKind::GitPush {
                all: false,
                remote: None,
            },
            flags,
        )
    }

    /// A command using `feature`. One arm per feature, so dating a new one
    /// means showing where kojutsu emits it.
    fn example(feature: JjFeature) -> JJCommand {
        match feature {
            JjFeature::BookmarkAdvance => cmd(
                JJCommandKind::BookmarkAdvance { change_id: None },
                CommandFlags::empty(),
            ),
            JjFeature::Run => run(CommandFlags::empty()),
            JjFeature::RunPassthrough => run(CommandFlags::PASSTHROUGH),
            JjFeature::RunIgnoreChanges => run(CommandFlags::IGNORE_CHANGES),
            JjFeature::RunIgnoreErrors => run(CommandFlags::IGNORE_ERRORS),
            JjFeature::PushAllowConflicts => push(CommandFlags::ALLOW_CONFLICTS),
            JjFeature::AbsorbLines => absorb(ChangeSelection::Lines("/tmp/sel.json".into())),
            JjFeature::TagTracking => cmd(
                JJCommandKind::TagUntrack {
                    tags: smallvec::smallvec![TagRef {
                        name: TagName::new("v1"),
                        remote: RemoteName::new("origin"),
                    }],
                },
                CommandFlags::empty(),
            ),
            JjFeature::Converge => cmd(
                JJCommandKind::Converge {
                    changes: SmallVec::new(),
                },
                CommandFlags::empty(),
            ),
            JjFeature::UndoCrossWorkspace => {
                cmd(JJCommandKind::Redo, CommandFlags::ALLOW_CROSS_WORKSPACE)
            }
        }
    }

    /// A release of the series before `version`'s.
    fn just_before(version: JjVersion) -> InstalledJj {
        InstalledJj::known(JjVersion::new(version.major, version.minor - 1, 0))
    }

    /// Each dated feature is tagged where kojutsu spells it: the jj that
    /// introduced it runs the command, the release before refuses it.
    #[test]
    fn every_feature_is_tagged_where_it_is_emitted() {
        for feature in JjFeature::iter() {
            let command = example(feature);
            assert!(
                command.features().any(|f| f == feature),
                "{feature:?} not tagged in {:?}",
                command.args()
            );
            let introduced = InstalledJj::known(feature.since());
            assert_eq!(command.unsupported_by(introduced), None, "{feature:?}");
            assert_eq!(
                command.unsupported_by(just_before(feature.since())),
                Some(feature)
            );
        }
    }

    /// Only a line selection goes through absorb's diff editor.
    #[test]
    fn absorbing_files_needs_nothing_new() {
        let files = absorb(ChangeSelection::Files(vec!["src/lib.rs".into()]));
        assert_eq!(files.features().count(), 0);
        // Elsewhere `--interactive` is old: split has taken it since 0.10.
        let split = cmd(
            JJCommandKind::Split {
                change_id: RevisionArg::new("x"),
                target: None,
                selection: ChangeSelection::Lines("/tmp/sel.json".into()),
            },
            CommandFlags::empty(),
        );
        assert_eq!(split.features().count(), 0);
    }

    /// A command that needs several features is held to the newest.
    #[test]
    fn the_newest_missing_feature_is_the_one_named() {
        let both = run(CommandFlags::PASSTHROUGH);
        let jj = InstalledJj::known(JjVersion::new(0, 42, 0));
        assert_eq!(both.unsupported_by(jj), Some(JjFeature::RunPassthrough));
    }

    #[test]
    fn a_refused_command_reports_why_without_running() {
        let jj = InstalledJj::known(JjVersion::new(0, 44, 0));
        let converge = example(JjFeature::Converge);
        let refused = converge.refused_by(jj).expect("refused");
        assert!(!refused.success);
        assert_eq!(refused.code, None);
        assert_eq!(refused.display, converge.display());
        let output = String::from_utf8_lossy(refused.output.bytes()).into_owned();
        assert_eq!(
            output,
            "not run: jj converge needs jj 0.45.0 or newer; the installed jj is 0.44.0\n"
        );

        assert!(run(CommandFlags::PASSTHROUGH).refused_by(jj).is_none());
        assert!(converge.refused_by(InstalledJj::default()).is_none());
    }

    /// A typed command line is the user's own: jj judges it.
    #[test]
    fn a_raw_command_is_never_refused() {
        let raw = cmd(
            JJCommandKind::Raw {
                args: vec!["converge".into()],
            },
            CommandFlags::empty(),
        );
        let ancient = InstalledJj::known(JjVersion::new(0, 1, 0));
        assert!(raw.refused_by(ancient).is_none());
    }

    #[test]
    fn a_retry_jj_would_refuse_is_not_offered() {
        let output = b"Error: Won't push commit 1234abcd since it has conflicts\n";
        let old = InstalledJj::known(JjVersion::new(0, 43, 0));
        assert!(
            push(CommandFlags::empty())
                .retry_options(output, old)
                .is_empty()
        );
        let current = InstalledJj::known(JjFeature::PushAllowConflicts.since());
        assert_eq!(
            push(CommandFlags::empty())
                .retry_options(output, current)
                .len(),
            1
        );
    }

    fn option(key: char, command: JJCommand) -> FollowUpOption {
        FollowUpOption {
            key,
            label: "option",
            action: FollowUpAction::Execute(command),
        }
    }

    #[test]
    fn only_what_jj_can_carry_out_is_offered() {
        let jj = InstalledJj::known(JjVersion::new(0, 44, 0));
        let offered = offerable(
            vec![
                option('c', example(JjFeature::Converge)),
                option('p', push(CommandFlags::empty())),
            ],
            jj,
        )
        .expect("one left");
        assert_eq!(offered.iter().map(|o| o.key).collect::<String>(), "p");

        // Nothing left: say why, rather than offer an empty choice.
        let refused = offerable(vec![option('c', example(JjFeature::Converge))], jj);
        assert_eq!(
            refused.err().as_deref(),
            Some("jj converge needs jj 0.45.0 or newer; the installed jj is 0.44.0")
        );
        assert!(
            offerable(Vec::new(), jj)
                .expect("nothing to drop")
                .is_empty()
        );
    }
}

#[cfg(test)]
mod cross_workspace_tests {
    use super::*;
    use crate::jj_version::JjVersion;

    const REFUSED: &[u8] = b"Error: Refusing to undo operation ada452fd2933 because it was performed in workspace second\nHint: Use `--allow-cross-workspace` to undo it anyway, or use `jj op revert` to revert a specific operation\n";

    fn undo(flags: CommandFlags) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Undo,
            flags,
        }
    }

    #[test]
    fn a_refused_undo_offers_to_undo_anyway() {
        let jj = InstalledJj::known(JjVersion::new(0, 46, 0));
        let options = undo(CommandFlags::empty()).retry_options(REFUSED, jj);
        let [option] = options.as_slice() else {
            panic!("expected one retry");
        };
        let FollowUpAction::Execute(retry) = &option.action else {
            panic!("expected a command");
        };
        assert_eq!(retry.args(), ["undo", "--allow-cross-workspace"]);
        // Already allowed: the same command would only be refused again.
        assert!(
            undo(CommandFlags::ALLOW_CROSS_WORKSPACE)
                .retry_options(REFUSED, jj)
                .is_empty()
        );
    }

    /// Only an undo or redo takes the flag.
    #[test]
    fn nothing_else_is_offered_it() {
        let jj = InstalledJj::default();
        let new = JJCommand {
            kind: JJCommandKind::New {
                change_ids: SmallVec::new(),
                insert: None,
            },
            flags: CommandFlags::empty(),
        };
        assert!(new.retry_options(REFUSED, jj).is_empty());
    }
}
