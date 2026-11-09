use std::io::{self, IsTerminal as _, Stdout, stdout};
use std::os::fd::AsFd as _;
use std::os::unix::io::AsRawFd;
use std::panic;
use std::sync::{Arc, mpsc};
use std::thread;

use crossterm::event::{self, Event};
use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use nix::unistd::{Pid, getpgrp, tcgetpgrp, tcsetpgrp};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Mouse reporting for presses, releases and the wheel, without motion.
/// crossterm's `EnableMouseCapture` also turns on drag (1002) and any-motion
/// (1003) tracking, and nothing here uses motion: every move would arrive as
/// an event and redraw the screen. Turned off by `DisableMouseCapture`.
struct EnableClickCapture;

impl crossterm::Command for EnableClickCapture {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        // 1000: presses, releases and the wheel. 1015 and 1006: coordinates
        // past column 223, in RXVT and the preferred SGR encoding.
        f.write_str("\x1b[?1000h\x1b[?1015h\x1b[?1006h")
    }
}

/// Enter the alternate screen, enable raw mode and mouse capture, and return
/// a [`Terminal`] handle ready for drawing.
pub fn init() -> io::Result<Term> {
    install_panic_hook();
    enable_raw_mode()?;
    if io::stdin().is_terminal() {
        stop_background_writes()?;
    }
    execute!(stdout(), EnterAlternateScreen, EnableClickCapture, Hide)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Have a background process group that writes to the terminal stopped, as
/// one that reads from it already is, so a command about to prompt stops
/// before drawing over the TUI rather than after. Raw mode saved the
/// terminal's settings from before this, so `restore` turns it off again.
fn stop_background_writes() -> io::Result<()> {
    let stdin = io::stdin();
    let mut attrs = tcgetattr(stdin.as_fd())?;
    attrs.local_flags.insert(LocalFlags::TOSTOP);
    tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &attrs)?;
    Ok(())
}

/// Whether kojutsu's process group is the terminal's foreground, the only
/// place it can hand the terminal to another group from.
pub fn owns_terminal() -> bool {
    tcgetpgrp(io::stdin().as_fd()).is_ok_and(|pgid| pgid == getpgrp())
}

/// Make `pgid` the terminal's foreground process group.
pub fn give_terminal_to(pgid: Pid) -> nix::Result<()> {
    tcsetpgrp(io::stdin().as_fd(), pgid)
}

/// Make kojutsu's process group the terminal's foreground again. kojutsu is
/// in the background at this point, where changing the foreground raises
/// SIGTTOU, and an unblocked SIGTTOU would stop kojutsu itself.
pub fn take_terminal_back() -> nix::Result<()> {
    with_sigttou_blocked(|| tcsetpgrp(io::stdin().as_fd(), getpgrp()))
}

/// Run `f` with SIGTTOU blocked on this thread, so that writing to or
/// configuring the terminal while kojutsu is in the background goes through
/// instead of stopping kojutsu.
pub fn with_sigttou_blocked<T>(f: impl FnOnce() -> T) -> T {
    let mut sigttou = SigSet::empty();
    sigttou.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    let blocked = pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&sigttou), Some(&mut previous));
    let out = f();
    if blocked.is_ok() {
        let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
    }
    out
}

/// Leave the alternate screen, disable raw mode and mouse capture.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    Ok(())
}

/// With the terminal handed back to the shell, hold it until Enter, so what a
/// command wrote there can be read before the TUI draws over it.
pub fn wait_for_enter(prompt: &str) {
    use std::io::{BufRead, Write};
    let mut out = stdout();
    let _ = write!(out, "\n[{prompt}]");
    let _ = out.flush();
    let _ = io::stdin().lock().read_line(&mut String::new());
}

/// Ensure the terminal is restored even on panic so the user doesn't get
/// stuck in raw mode.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}

const STDIN_TOKEN: mio::Token = mio::Token(0);
const WAKE_TOKEN: mio::Token = mio::Token(1);
const SIGNAL_TOKEN: mio::Token = mio::Token(2);

pub struct TerminalEvents {
    waker: Arc<mio::Waker>,
    join: thread::JoinHandle<()>,
}

impl TerminalEvents {
    pub fn stop(self) {
        let _ = self.waker.wake();
        let _ = self.join.join();
    }
}

pub fn spawn_terminal_events<T: Send + 'static>(
    event_tx: mpsc::Sender<T>,
    wrap: impl Fn(Event) -> T + Send + 'static,
) -> TerminalEvents {
    let poll = mio::Poll::new().expect("failed to create mio Poll");
    let waker =
        Arc::new(mio::Waker::new(poll.registry(), WAKE_TOKEN).expect("failed to create Waker"));

    let waker_clone = Arc::clone(&waker);
    let join = thread::spawn(move || {
        let mut poll = poll;
        let stdin_fd = io::stdin().as_raw_fd();
        let mut source = mio::unix::SourceFd(&stdin_fd);
        poll.registry()
            .register(&mut source, STDIN_TOKEN, mio::Interest::READABLE)
            .expect("failed to register stdin");

        // Register SIGWINCH so terminal resize wakes the poll.
        let mut signals = signal_hook_mio::v1_0::Signals::new([signal_hook::consts::SIGWINCH])
            .expect("failed to register SIGWINCH");
        poll.registry()
            .register(&mut signals, SIGNAL_TOKEN, mio::Interest::READABLE)
            .expect("failed to register signal source");

        let mut events = mio::Events::with_capacity(4);
        loop {
            match poll.poll(&mut events, None) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
            for ev in &events {
                match ev.token() {
                    WAKE_TOKEN => return,
                    SIGNAL_TOKEN => {
                        // Drain pending signals and send a Resize event.
                        for _sig in signals.pending() {}
                        // Query the actual terminal size.
                        if let Ok((cols, rows)) = crossterm::terminal::size() {
                            let _ = event_tx.send(wrap(Event::Resize(cols, rows)));
                        }
                        // Drain any stdin events triggered by the resize (e.g.
                        // mouse position reports) so they don't delay the next
                        // real keypress.
                        while event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                            match event::read() {
                                Ok(ev) => {
                                    let _ = event_tx.send(wrap(ev));
                                }
                                Err(_) => break,
                            }
                        }
                    }
                    STDIN_TOKEN => {
                        // Read the first event, then drain any events
                        // crossterm buffered internally since mio won't
                        // re-trigger for bytes already consumed from stdin.
                        while let Ok(ev) = event::read() {
                            if event_tx.send(wrap(ev)).is_err() {
                                return;
                            }
                            if !event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    TerminalEvents {
        waker: waker_clone,
        join,
    }
}
