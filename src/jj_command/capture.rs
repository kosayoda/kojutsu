//! What a command wrote, kept so that either pipe can still be read on its own.

/// Which of a command's pipes some bytes arrived on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Everything a command wrote, in the order it arrived, remembering which pipe
/// each run of bytes came from.
///
/// One buffer serves both readers. The interleaved whole is what the UI shows,
/// and what it streams live, so the order bytes arrived in is the order they
/// have to stay in. A caller parsing an answer wants one pipe on its own
/// instead: jj writes warnings and hints to stderr, and reading an object name
/// out of the interleaved buffer can come back with the "a" from "Warning".
///
/// `runs` indexes `bytes`, so both are private and only [`Self::push`] and
/// [`Self::push_note`] can extend them: there is no way to leave an index
/// pointing at bytes that moved, or outside the buffer entirely.
#[derive(Default)]
pub struct Captured {
    bytes: Vec<u8>,
    /// Runs of `bytes` in arrival order. Gaps between them are notes, which
    /// belong to no pipe.
    runs: Vec<(Stream, std::ops::Range<usize>)>,
}

impl Captured {
    /// Record bytes that arrived on one of the command's pipes.
    pub fn push(&mut self, source: Stream, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        let start = self.bytes.len();
        self.bytes.extend_from_slice(chunk);
        let end = self.bytes.len();
        match self.runs.last_mut() {
            // Back-to-back arrivals from one pipe are a single run.
            Some((last, run)) if *last == source && run.end == start => run.end = end,
            _ => self.runs.push((source, start..end)),
        }
    }

    /// Record bytes kojutsu wrote itself: a separator keeping stderr off the
    /// end of an unterminated stdout line, or a stand-in for a command that
    /// produced nothing to capture. Shown along with the rest, and part of
    /// neither pipe, so a caller reading a pipe never sees kojutsu's words
    /// mixed in with the command's.
    pub fn push_note(&mut self, text: &str) {
        self.bytes.extend_from_slice(text.as_bytes());
    }

    /// Both pipes as they arrived. What the UI shows.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// What arrived on one pipe, in order, with nothing else in it.
    pub fn stream(&self, want: Stream) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (source, run) in &self.runs {
            if *source == want {
                bytes.extend_from_slice(&self.bytes[run.clone()]);
            }
        }
        bytes
    }
}

impl From<&str> for Captured {
    /// A result that is only a note, like the stand-in for a killed command.
    fn from(note: &str) -> Self {
        let mut captured = Self::default();
        captured.push_note(note);
        captured
    }
}

/// Both pipes of a command that ran to completion before either was read, so
/// there is no arrival order left to preserve: stdout leads, stderr follows.
pub fn joined(stdout: &[u8], stderr: &[u8]) -> Captured {
    let mut captured = Captured::default();
    captured.push(Stream::Stdout, stdout);
    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with(b"\n") {
            captured.push_note("\n");
        }
        captured.push(Stream::Stderr, stderr);
    }
    captured
}

#[cfg(test)]
mod tests {
    use super::{Captured, Stream, joined};

    fn split(captured: &Captured) -> (String, String) {
        (
            String::from_utf8(captured.stream(Stream::Stdout)).unwrap(),
            String::from_utf8(captured.stream(Stream::Stderr)).unwrap(),
        )
    }

    /// The separator keeps the two pipes off one line in the UI. It is kojutsu's
    /// byte, not the command's, so neither pipe comes back carrying it.
    #[test]
    fn a_separator_belongs_to_the_whole_but_to_neither_pipe() {
        let captured = joined(b"a4f2c1", b"Warning: no name\n");
        assert_eq!(
            String::from_utf8_lossy(captured.bytes()),
            "a4f2c1\nWarning: no name\n"
        );
        assert_eq!(
            split(&captured),
            ("a4f2c1".into(), "Warning: no name\n".into())
        );
    }

    /// Nothing is inserted when stdout already ends a line.
    #[test]
    fn no_separator_is_added_when_none_is_needed() {
        let captured = joined(b"a4f2c1\n", b"oops\n");
        assert_eq!(String::from_utf8_lossy(captured.bytes()), "a4f2c1\noops\n");
        assert_eq!(split(&captured), ("a4f2c1\n".into(), "oops\n".into()));
    }

    #[test]
    fn a_pipe_that_wrote_nothing_reads_back_empty() {
        assert_eq!(
            split(&joined(b"only stdout\n", b"")),
            ("only stdout\n".into(), String::new())
        );
        assert_eq!(
            split(&joined(b"", b"only stderr\n")),
            (String::new(), "only stderr\n".into())
        );
        assert_eq!(split(&joined(b"", b"")), (String::new(), String::new()));
    }

    /// A note is all kojutsu wrote, so both pipes are empty and the caller has
    /// `ok` and `status` to tell it why.
    #[test]
    fn a_result_that_is_only_a_note_has_neither_pipe() {
        let captured = Captured::from("interrupted");
        assert_eq!(String::from_utf8_lossy(captured.bytes()), "interrupted");
        assert!(!captured.is_empty());
        assert_eq!(split(&captured), (String::new(), String::new()));
    }

    /// The live overlay is fed the same bytes in the same order as the final
    /// buffer, so interleaved arrivals must stay interleaved, while each pipe
    /// still reads back as the command wrote it.
    #[test]
    fn interleaved_arrivals_keep_their_order_and_still_separate() {
        let mut captured = Captured::default();
        captured.push(Stream::Stderr, b"hint: ");
        captured.push(Stream::Stderr, b"snapshotting\n");
        captured.push(Stream::Stdout, b"first\n");
        captured.push(Stream::Stderr, b"warning: slow\n");
        captured.push(Stream::Stdout, b"second\n");

        assert_eq!(
            String::from_utf8_lossy(captured.bytes()),
            "hint: snapshotting\nfirst\nwarning: slow\nsecond\n"
        );
        assert_eq!(
            split(&captured),
            (
                "first\nsecond\n".into(),
                "hint: snapshotting\nwarning: slow\n".into()
            )
        );
    }

    /// Chunk boundaries are an artefact of how much a read returned, so runs
    /// from one pipe coalesce rather than accumulating one entry per read.
    #[test]
    fn consecutive_chunks_from_one_pipe_are_one_run() {
        let mut captured = Captured::default();
        for _ in 0..100 {
            captured.push(Stream::Stdout, b"x");
        }
        captured.push(Stream::Stderr, b"y");
        assert_eq!(captured.runs.len(), 2);
    }

    /// An empty read is not a run: a pipe that only ever returned nothing has
    /// written nothing.
    #[test]
    fn an_empty_chunk_is_not_a_run() {
        let mut captured = Captured::default();
        captured.push(Stream::Stdout, b"");
        assert!(captured.runs.is_empty());
        assert!(captured.is_empty());
    }
}
