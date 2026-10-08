use std::path::Path;
use std::process::Output;

use crate::jj_version::InstalledJj;

use super::capture::joined;
use super::{Captured, CommandPart, JJCommand, JJCommandResult, KillHandle, Stream};

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

    /// The result standing in for this command when `jj` is too old to take
    /// it: reported, logged and handed on like any failure, without jj ever
    /// being asked to parse a flag it would only reject.
    pub fn refused_by(&self, jj: InstalledJj) -> Option<JJCommandResult> {
        let reason = jj.refusal(self.unsupported_by(jj)?)?;
        Some(JJCommandResult {
            display: self.display(),
            display_parts: self.display_parts(),
            output: Captured::from(format!("not run: {reason}\n").as_str()),
            success: false,
            cancelled: false,
            code: None,
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
        kill: &KillHandle,
        on_chunk: impl FnMut(&[u8]) + Send,
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

        kill.set_pgid(child.id() as i32);

        let child_stdout = child.stdout.take().unwrap();
        let child_stderr = child.stderr.take().unwrap();
        // Both pipes accumulate under one lock, so the callback observes
        // exactly the byte order the finished capture has.
        let sink = std::sync::Mutex::new((Captured::default(), on_chunk));
        let status = std::thread::scope(|scope| {
            scope.spawn(|| stream_pipe(child_stdout, Stream::Stdout, &sink));
            scope.spawn(|| stream_pipe(child_stderr, Stream::Stderr, &sink));
            child.wait()
        });
        let output = sink
            .into_inner()
            .map(|(captured, _)| captured)
            .unwrap_or_default();

        let status = match status {
            Ok(s) => s,
            Err(e) => return jj_error(cmd_str, display_parts, e),
        };

        let success = status.code().is_some() && status.success();
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            success,
            "jj command finished",
        );

        if status.code().is_none() {
            return JJCommandResult {
                display: cmd_str,
                display_parts,
                output: Captured::from("interrupted"),
                success: false,
                cancelled: true,
                code: None,
            };
        }

        JJCommandResult {
            display: cmd_str,
            display_parts,
            output,
            success: status.success(),
            cancelled: false,
            code: status.code(),
        }
    }
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

fn stream_pipe<F: FnMut(&[u8])>(
    pipe: impl std::io::Read,
    source: Stream,
    sink: &std::sync::Mutex<(Captured, F)>,
) {
    read_chunks(pipe, |chunk| {
        if let Ok(mut guard) = sink.lock() {
            let (captured, on_chunk) = &mut *guard;
            captured.push(source, chunk);
            on_chunk(chunk);
        }
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
