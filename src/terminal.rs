use std::io::{self, stdout, Stdout};
use std::os::unix::io::AsRawFd;
use std::panic;
use std::sync::{mpsc, Arc};
use std::thread;

use crossterm::event::{self, Event};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Enter the alternate screen, enable raw mode and mouse capture, and return
/// a [`Terminal`] handle ready for drawing.
pub fn init() -> io::Result<Term> {
    install_panic_hook();
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Leave the alternate screen, disable raw mode and mouse capture.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture, Show)?;
    Ok(())
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
            if poll.poll(&mut events, None).is_err() {
                break;
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
                        loop {
                            match event::read() {
                                Ok(ev) => {
                                    if event_tx.send(wrap(ev)).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => return,
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
