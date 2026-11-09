use std::path::Path;
use std::process::Output;
use std::sync::Mutex;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::jj_version::InstalledJj;
use crate::terminal;

use super::capture::joined;
use super::job::Phase;
use super::{Captured, CommandPart, JJCommand, JJCommandResult, JobHandle, Stream};

impl JJCommand {
    pub fn run_interactive(&self, repo_path: &Path) -> JJCommandResult {
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            self.command(repo_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::piped())
                .output()
        });

        match result {
            Ok(Output { status, .. }) if was_interrupted || status.code().is_none() => {
                JJCommandResult {
                    display,
                    display_parts,
                    output: Captured::from("interrupted"),
                    success: false,
                    cancelled: true,
                    code: None,
                }
            }
            // stdout went straight to the terminal, so stderr is all there is.
            Ok(Output { stderr, status, .. }) => {
                let mut output = Captured::default();
                output.push(Stream::Stderr, &stderr);
                JJCommandResult {
                    display,
                    display_parts,
                    output,
                    success: status.success(),
                    cancelled: false,
                    code: status.code(),
                }
            }
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    pub fn run_suspend_captured(&self, repo_path: &Path) -> JJCommandResult {
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            let mut child = self
                .command(repo_path)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            let child_stdout = child.stdout.take().unwrap();
            let child_stderr = child.stderr.take().unwrap();

            let stdout_thread = std::thread::spawn(move || tee_pipe(child_stdout));
            let stderr_thread = std::thread::spawn(move || tee_pipe(child_stderr));

            let status = child.wait()?;
            let stdout = stdout_thread.join().unwrap_or_default();
            let stderr = stderr_thread.join().unwrap_or_default();

            Ok(Output {
                status,
                stdout,
                stderr,
            })
        });

        match result {
            Ok(Output { status, .. }) if was_interrupted || status.code().is_none() => {
                JJCommandResult {
                    display,
                    display_parts,
                    output: Captured::from("interrupted"),
                    success: false,
                    cancelled: true,
                    code: None,
                }
            }
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: joined(&stdout, &stderr),
                success: status.success(),
                cancelled: false,
                code: status.code(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    /// The result standing in for this command when it cannot be run as
    /// built: `jj` is too old to take it, or it names kojutsu as jj's tool
    /// and kojutsu does not know where its own executable is. Reported,
    /// logged and handed on like any failure, without jj ever being asked.
    pub fn refused_by(&self, jj: InstalledJj) -> Option<JJCommandResult> {
        self.refusal(jj, super::args::self_exe())
    }

    pub(super) fn refusal(&self, jj: InstalledJj, exe: Option<&Path>) -> Option<JJCommandResult> {
        let reason = match self.unsupported_by(jj) {
            Some(feature) => jj.refusal(feature)?,
            None if exe.is_none() && self.names_self_as_tool() => {
                "kojutsu cannot find its own executable to run as jj's tool".to_string()
            }
            None => return None,
        };
        Some(JJCommandResult {
            display: self.display(),
            display_parts: self.display_parts(),
            output: Captured::from(format!("not run: {reason}\n").as_str()),
            success: false,
            cancelled: false,
            code: None,
        })
    }

    /// Whether the command hands jj one of the tools that re-invoke this
    /// binary, read off the built args like `--interactive` is.
    fn names_self_as_tool(&self) -> bool {
        self.args().windows(2).any(|pair| {
            pair[0] == "--tool"
                && (pair[1] == super::args::SELECT_TOOL || pair[1] == super::args::APPLY_TOOL)
        })
    }

    pub fn run(&self, repo_path: &Path) -> JJCommandResult {
        let display = self.display();
        let display_parts = self.display_parts();

        let result = self.command(repo_path).env("JJ_EDITOR", ":").output();

        match result {
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: joined(&stdout, &stderr),
                success: status.success(),
                cancelled: false,
                code: status.code(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    pub fn run_cancellable(
        &self,
        repo_path: &Path,
        job: &JobHandle,
        on_event: impl FnMut(RunEvent<'_>) + Send,
    ) -> JJCommandResult {
        use std::os::unix::process::CommandExt as _;

        // The spawn itself is recorded by `command` below, along with every
        // other process kojutsu starts; this pairs the outcome with it.
        let start = std::time::Instant::now();
        let cmd_str = self.display();
        let display_parts = self.display_parts();

        let mut child = match self
            .command(repo_path)
            .env("JJ_EDITOR", ":")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return jj_error(cmd_str, display_parts, e),
        };

        let pid = Pid::from_raw(child.id() as i32);
        job.start(pid);

        let child_stdout = child.stdout.take().unwrap();
        let child_stderr = child.stderr.take().unwrap();
        let on_event = Mutex::new(on_event);
        let exit = std::thread::scope(|scope| {
            scope.spawn(|| stream_pipe(child_stdout, Stream::Stdout, job, &on_event));
            scope.spawn(|| stream_pipe(child_stderr, Stream::Stderr, job, &on_event));
            let exit = wait_for_exit(pid, job, &on_event);
            // Reaped, so the pid is free for reuse: a cancel from here on
            // must not name it.
            job.reaped();
            exit
        });
        let output = job.finish();

        let code = match exit {
            Exit::Code(code) => code,
            Exit::Signalled => {
                return JJCommandResult {
                    display: cmd_str,
                    display_parts,
                    output: Captured::from("interrupted"),
                    success: false,
                    cancelled: true,
                    code: None,
                };
            }
            Exit::Refused => {
                return JJCommandResult {
                    display: cmd_str,
                    display_parts,
                    output,
                    success: false,
                    cancelled: false,
                    code: None,
                };
            }
            Exit::Failed(e) => return jj_error(cmd_str, display_parts, e),
        };

        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            success = code == 0,
            "jj command finished",
        );

        JJCommandResult {
            display: cmd_str,
            display_parts,
            output,
            success: code == 0,
            cancelled: false,
            code: Some(code),
        }
    }
}

/// What a background command reports while it runs.
pub enum RunEvent<'a> {
    Output(&'a [u8]),
    /// It stopped for touching the terminal, and waits for
    /// `JobHandle::hand_over_terminal`.
    Stopped,
}

/// How a background command ended.
enum Exit {
    Code(i32),
    /// Killed by a signal: a cancel, or something outside kojutsu.
    Signalled,
    /// Ended because it stopped to use a terminal kojutsu could not give it.
    Refused,
    Failed(std::io::Error),
}

/// Wait for the command to exit, dealing with each stop on the way. A stop
/// means some process in its group touched the terminal: the kernel stops
/// the whole group, so it shows up here even when the process that wants
/// the terminal is a grandchild, such as ssh under git under jj.
fn wait_for_exit<F: FnMut(RunEvent<'_>)>(pid: Pid, job: &JobHandle, on_event: &Mutex<F>) -> Exit {
    let mut refused = false;
    loop {
        match waitpid(pid, Some(WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(..)) if refused => {
                return Exit::Refused;
            }
            Ok(WaitStatus::Exited(_, code)) => return Exit::Code(code),
            Ok(WaitStatus::Signaled(..)) => return Exit::Signalled,
            Ok(WaitStatus::Stopped(..)) => match job.phase() {
                Phase::Running if terminal::owns_terminal() => {
                    job.set_phase(Phase::WaitingForTerminal);
                    lock(on_event)(RunEvent::Stopped);
                }
                Phase::Running => {
                    tracing::warn!(
                        pid = pid.as_raw(),
                        "a command stopped to use the terminal, which kojutsu does not control",
                    );
                    job.note(
                        "\nkojutsu: the command stopped to use the terminal, and kojutsu cannot \
                         hand it over because kojutsu is not in the terminal's foreground\n",
                    );
                    refused = true;
                    job.kill();
                }
                // Ctrl-Z at the command's prompt. kojutsu cannot be suspended
                // from inside a handoff, so the command carries on.
                Phase::HandedOver => {
                    let _ = killpg(pid, Signal::SIGCONT);
                }
                // Stopped already, so it cannot stop again; and `Finished`
                // is only set once this returns.
                Phase::WaitingForTerminal | Phase::Finished => {}
            },
            Ok(_) => {}
            Err(Errno::EINTR) => {}
            Err(e) => return Exit::Failed(e.into()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Read a pipe to EOF in small chunks, invoking `on_chunk` for each.
fn read_chunks(mut pipe: impl std::io::Read, mut on_chunk: impl FnMut(&[u8])) {
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => on_chunk(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Read one of the command's pipes into the job. SIGTTOU is blocked because
/// the job copies output to the terminal while kojutsu is in the background,
/// and on a terminal set to stop background writes that would stop kojutsu.
fn stream_pipe<F: FnMut(RunEvent<'_>)>(
    pipe: impl std::io::Read,
    source: Stream,
    job: &JobHandle,
    on_event: &Mutex<F>,
) {
    terminal::with_sigttou_blocked(|| {
        read_chunks(pipe, |chunk| {
            job.record(source, chunk, |chunk| {
                lock(on_event)(RunEvent::Output(chunk))
            });
        });
    });
}

fn tee_pipe(pipe: impl std::io::Read + Send + 'static) -> Vec<u8> {
    use std::io::Write;
    let mut captured = Vec::new();
    let mut out = std::io::stderr();
    read_chunks(pipe, |chunk| {
        captured.extend_from_slice(chunk);
        let _ = out.write_all(chunk);
        let _ = out.flush();
    });
    captured
}

fn jj_error(
    display: String,
    display_parts: Vec<CommandPart>,
    err: std::io::Error,
) -> JJCommandResult {
    JJCommandResult {
        display,
        display_parts,
        output: Captured::from(format!("failed to run jj: {err}").as_str()),
        success: false,
        cancelled: false,
        code: None,
    }
}

fn with_sigint_suppressed<T>(body: impl FnOnce() -> T) -> (T, bool) {
    let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook = signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone()).ok();
    let result = body();
    if let Some(id) = hook {
        signal_hook::low_level::unregister(id);
    }
    (
        result,
        interrupted.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::jj_command::JJCommandKind;
    use crate::keymap::CommandFlags;

    fn sh(script: &str) -> JJCommand {
        JJCommand {
            kind: JJCommandKind::Exec {
                program: "sh".into(),
                args: vec!["-c".into(), script.into()],
            },
            flags: CommandFlags::empty(),
        }
    }

    /// Once the child is reaped its pid can belong to anyone, so the
    /// handle a cancel would signal through is cleared before the run
    /// returns.
    #[test]
    fn a_finished_run_leaves_nothing_to_kill() {
        let job = JobHandle::new();
        let result = sh("true").run_cancellable(Path::new("."), &job, |_| {});
        assert!(result.success);
        assert_eq!(job.pgid(), None);
    }

    /// The exit code and both pipes come through `waitpid` as they did
    /// through `Child::wait`.
    #[test]
    fn a_failing_run_keeps_its_code_and_output() {
        let job = JobHandle::new();
        let result =
            sh("echo out; echo err >&2; exit 3").run_cancellable(Path::new("."), &job, |_| {});
        assert!(!result.success);
        assert!(!result.cancelled);
        assert_eq!(result.code, Some(3));
        assert_eq!(result.output.stream(Stream::Stdout), b"out\n");
        assert_eq!(result.output.stream(Stream::Stderr), b"err\n");
    }

    /// A stopped command never leaves the run hanging. Under a terminal
    /// kojutsu owns, the stop is reported, and a cancel then ends it even
    /// though it is stopped. Without one, there is nobody to hand the
    /// terminal to, so the run ends it and says why.
    #[test]
    fn a_stopped_command_ends_either_way() {
        let job = JobHandle::new();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (stops_tx, stops_rx) = std::sync::mpsc::channel();
        let runner = job.clone();
        std::thread::spawn(move || {
            let on_event = {
                let job = runner.clone();
                move |event: RunEvent<'_>| match event {
                    RunEvent::Output(_) => {}
                    RunEvent::Stopped => {
                        let _ = stops_tx.send(());
                        job.kill();
                    }
                }
            };
            let result = sh("kill -STOP $$; echo resumed").run_cancellable(
                Path::new("."),
                &runner,
                on_event,
            );
            let _ = done_tx.send(result);
        });
        let result = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the stopped command ended");
        assert!(!result.success);
        assert!(!String::from_utf8_lossy(result.output.bytes()).contains("resumed"));
        if terminal::owns_terminal() {
            assert_eq!(stops_rx.try_iter().count(), 1);
            assert!(result.cancelled);
        } else {
            assert_eq!(stops_rx.try_iter().count(), 0);
            assert!(!result.cancelled);
            let output = String::from_utf8_lossy(result.output.bytes()).into_owned();
            assert!(
                output.contains("not in the terminal's foreground"),
                "{output}"
            );
        }
    }
}
