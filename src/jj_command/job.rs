use std::io::Write as _;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use super::{Captured, Stream};
use crate::terminal;

/// The main loop's hold on a running background command: cancel it, or hand
/// it the terminal when it stops to prompt there. Clones share one command;
/// the thread running it holds one too.
#[derive(Clone)]
pub struct JobHandle(Arc<Job>);

struct Job {
    state: Mutex<JobState>,
    /// Signalled when the phase becomes `Finished`.
    finished: Condvar,
    output: Mutex<JobOutput>,
}

struct JobState {
    /// The command's process group, cleared as soon as it is reaped: from
    /// then on the id is free to belong to anyone.
    pgid: Option<Pid>,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Running,
    /// Stopped for touching the terminal from the background, until the main
    /// loop hands the terminal over.
    WaitingForTerminal,
    /// In the terminal's foreground, until it exits.
    HandedOver,
    /// Reaped, with both pipes read to the end.
    Finished,
}

struct JobOutput {
    captured: Captured,
    /// Whether output is also copied to the terminal, which it is while the
    /// command owns the terminal and the TUI is out of the way.
    on_terminal: bool,
}

impl JobHandle {
    #[expect(
        clippy::new_without_default,
        reason = "each handle is made for one command, never as a placeholder"
    )]
    pub fn new() -> Self {
        Self(Arc::new(Job {
            state: Mutex::new(JobState {
                pgid: None,
                phase: Phase::Running,
            }),
            finished: Condvar::new(),
            output: Mutex::new(JobOutput {
                captured: Captured::default(),
                on_terminal: false,
            }),
        }))
    }

    pub fn kill(&self) {
        if let Some(pgid) = self.pgid() {
            let _ = killpg(pgid, Signal::SIGTERM);
            // A stopped process holds the SIGTERM until it is continued, and
            // one that stopped to prompt on the terminal would never be.
            let _ = killpg(pgid, Signal::SIGCONT);
        }
    }

    /// Whether the command is stopped, waiting for `hand_over_terminal`.
    pub fn wants_terminal(&self) -> bool {
        self.state().phase == Phase::WaitingForTerminal
    }

    /// Give the terminal to the stopped command until it exits, then take it
    /// back. The caller has already put the TUI away. Returns at once unless
    /// the command is waiting for the terminal, so a stop that was overtaken
    /// by a cancel or an exit does nothing.
    pub fn hand_over_terminal(&self) {
        let pgid = {
            let mut state = self.state();
            match (state.phase, state.pgid) {
                (Phase::WaitingForTerminal, Some(pgid)) => {
                    state.phase = Phase::HandedOver;
                    pgid
                }
                _ => return,
            }
        };
        tracing::info!(pgid = pgid.as_raw(), "handing the terminal to a command");

        // What the command printed so far is the context for its prompt. The
        // whole group is stopped, so nothing arrives between the two steps.
        {
            let mut output = lock(&self.0.output);
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(output.captured.bytes());
            let _ = stderr.flush();
            output.on_terminal = true;
        }

        if let Err(e) = terminal::give_terminal_to(pgid) {
            tracing::warn!(
                pgid = pgid.as_raw(),
                "could not hand the terminal over: {e}"
            );
            let _ = killpg(pgid, Signal::SIGTERM);
        }
        let _ = killpg(pgid, Signal::SIGCONT);

        let mut state = self.state();
        while state.phase != Phase::Finished {
            state = self
                .0
                .finished
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(state);

        if let Err(e) = terminal::take_terminal_back() {
            tracing::error!("could not take the terminal back: {e}");
        }
        lock(&self.0.output).on_terminal = false;
        tracing::info!(pgid = pgid.as_raw(), "took the terminal back");
    }

    pub(super) fn start(&self, pgid: Pid) {
        let mut state = self.state();
        state.pgid = Some(pgid);
        state.phase = Phase::Running;
    }

    pub(super) fn pgid(&self) -> Option<Pid> {
        self.state().pgid
    }

    pub(super) fn phase(&self) -> Phase {
        self.state().phase
    }

    pub(super) fn set_phase(&self, phase: Phase) {
        self.state().phase = phase;
    }

    pub(super) fn reaped(&self) {
        self.state().pgid = None;
    }

    /// Mark the command finished, waking a handoff waiting on it, and take
    /// what it printed.
    pub(super) fn finish(&self) -> Captured {
        self.state().phase = Phase::Finished;
        self.0.finished.notify_all();
        std::mem::take(&mut lock(&self.0.output).captured)
    }

    /// Record a chunk from one of the command's pipes, copying it to the
    /// terminal while the command owns it. `then` runs under the same lock,
    /// so whatever it feeds sees chunks in the order the capture has them.
    pub(super) fn record(&self, source: Stream, chunk: &[u8], then: impl FnOnce(&[u8])) {
        let mut output = lock(&self.0.output);
        output.captured.push(source, chunk);
        if output.on_terminal {
            let mut stderr = std::io::stderr();
            let _ = stderr.write_all(chunk);
            let _ = stderr.flush();
        }
        then(chunk);
    }

    pub(super) fn note(&self, text: &str) {
        lock(&self.0.output).captured.push_note(text);
    }

    fn state(&self) -> MutexGuard<'_, JobState> {
        lock(&self.0.state)
    }
}

/// The state behind these locks stays consistent across a panic elsewhere,
/// so a poisoned lock is used as it is.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
