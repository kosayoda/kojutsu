use std::path::Path;
use std::process::{Command, Output};

use super::{CommandPart, JJCommand, JJCommandResult, KillHandle};

impl JJCommand {
    pub fn run_interactive(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            Command::new("jj")
                .arg("-R")
                .arg(repo_path)
                .arg("--color=always")
                .args(&args)
                .current_dir(repo_path)
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
                    output: b"interrupted".to_vec(),
                    success: false,
                }
            }
            Ok(Output { stderr, status, .. }) => JJCommandResult {
                display,
                display_parts,
                output: stderr,
                success: status.success(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    pub fn run_suspend_captured(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let (result, was_interrupted) = with_sigint_suppressed(|| {
            let mut child = Command::new("jj")
                .arg("-R")
                .arg(repo_path)
                .arg("--color=always")
                .args(&args)
                .current_dir(repo_path)
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
                    output: b"interrupted".to_vec(),
                    success: false,
                }
            }
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
            },
            Err(e) => jj_error(display, display_parts, e),
        }
    }

    pub fn run(&self, repo_path: &Path) -> JJCommandResult {
        let args = self.args();
        let display = self.display();
        let display_parts = self.display_parts();

        let result = Command::new("jj")
            .arg("-R")
            .arg(repo_path)
            .arg("--color=always")
            .args(&args)
            .current_dir(repo_path)
            .env("JJ_EDITOR", ":")
            .output();

        match result {
            Ok(Output {
                stdout,
                stderr,
                status,
            }) => JJCommandResult {
                display,
                display_parts,
                output: merge_captured_output(stdout, stderr),
                success: status.success(),
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

        let start = std::time::Instant::now();
        let args = self.args();
        let cmd_str = self.display();
        tracing::info!(command = cmd_str.as_str(), "running jj command");
        let display_parts = self.display_parts();

        let mut child = match Command::new("jj")
            .arg("-R")
            .arg(repo_path)
            .arg("--color=always")
            .args(&args)
            .current_dir(repo_path)
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
        // Accumulate stdout and stderr into one chronologically interleaved
        // buffer; the callback observes exactly the same byte order because
        // both happen under the same lock.
        let sink = std::sync::Mutex::new((Vec::new(), on_chunk));
        let status = std::thread::scope(|scope| {
            scope.spawn(|| stream_pipe(child_stdout, &sink));
            scope.spawn(|| stream_pipe(child_stderr, &sink));
            child.wait()
        });
        let output = sink.into_inner().map(|(buf, _)| buf).unwrap_or_default();

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
                output: b"interrupted".to_vec(),
                success: false,
            };
        }

        JJCommandResult {
            display: cmd_str,
            display_parts,
            output,
            success: status.success(),
        }
    }
}

fn stream_pipe<F: FnMut(&[u8])>(
    mut pipe: impl std::io::Read,
    sink: &std::sync::Mutex<(Vec<u8>, F)>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let mut guard = match sink.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                let (captured, on_chunk) = &mut *guard;
                captured.extend_from_slice(chunk);
                on_chunk(chunk);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn tee_pipe(mut pipe: impl std::io::Read + Send + 'static) -> Vec<u8> {
    use std::io::Write;
    let mut buf = [0u8; 4096];
    let mut captured = Vec::new();
    let mut out = std::io::stderr();
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                captured.extend_from_slice(chunk);
                let _ = out.write_all(chunk);
                let _ = out.flush();
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    captured
}

fn merge_captured_output(mut stdout: Vec<u8>, stderr: Vec<u8>) -> Vec<u8> {
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with(b"\n") {
            stdout.push(b'\n');
        }
        stdout.extend_from_slice(&stderr);
    }
    stdout
}

fn jj_error(
    display: String,
    display_parts: Vec<CommandPart>,
    err: std::io::Error,
) -> JJCommandResult {
    JJCommandResult {
        display,
        display_parts,
        output: format!("failed to run jj: {err}").into_bytes(),
        success: false,
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
