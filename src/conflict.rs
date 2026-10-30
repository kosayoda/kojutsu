//! First-class merge-conflict model: pure repo data as materialized from
//! jj (terms, per-hunk content), plus the user's resolution choices. All
//! interaction state (which term is picked, base-fold, gap-expansion)
//! lives App-side in `conflict_ui`: these types carry none of it.

use crate::dag::DiffToken;

use bstr::{BString, ByteSlice as _};

use jj_lib::conflicts::ConflictMarkerStyle;

/// The marker style kojutsu materializes conflicts with, and the name jj
/// spells it by in configuration.
///
/// Diff line numbers (and so the line selections built from them) are
/// computed against this materialization. Any jj command that re-materializes
/// a conflict on its own side has to be pinned to the same style, or the two
/// disagree about how many marker lines precede a given line and the wrong
/// lines get applied.
///
/// jj-lib only converts a name into a style, never the reverse, so the two
/// are stated separately and tied together by a test that parses the name
/// with jj-lib's own deserializer.
pub const MARKER_STYLE: ConflictMarkerStyle = ConflictMarkerStyle::Git;
pub const MARKER_STYLE_CONFIG: &str = "git";

/// Identifies one term of a conflict hunk. jj represents a conflict as
/// alternating positive terms ("sides", the contents to merge) and negative
/// terms ("bases", the common ancestors diffed away); ordinals are 0-based.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictTermKind {
    Side(usize),
    Base(usize),
}

impl ConflictTermKind {
    pub fn is_side(self) -> bool {
        matches!(self, Self::Side(_))
    }

    /// Human-readable label. Two-sided conflicts use the familiar
    /// ours/theirs/base; n-way conflicts fall back to numbered terms.
    pub fn label(self, num_sides: usize) -> String {
        if num_sides <= 2 {
            match self {
                Self::Side(0) => "ours".to_string(),
                Self::Side(_) => "theirs".to_string(),
                Self::Base(_) => "base".to_string(),
            }
        } else {
            match self {
                Self::Side(n) => format!("side {}", n + 1),
                Self::Base(n) => format!("base {}", n + 1),
            }
        }
    }
}

/// File content split into lines. Each line keeps its own terminator
/// (`\n`, `\r\n`, or none on an unterminated last line) and its bytes
/// as stored, so writing the lines back reproduces the content exactly:
/// a resolution must not change line endings or encoding of the parts
/// the user didn't touch.
#[derive(Clone, PartialEq, Eq)]
pub struct ConflictText {
    lines: Vec<BString>,
}

impl ConflictText {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            lines: bytes
                .split_inclusive(|&b| b == b'\n')
                .map(BString::from)
                .collect(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Line `idx` for display: terminator dropped, invalid UTF-8 replaced.
    pub fn display_line(&self, idx: usize) -> Option<String> {
        let line = self.lines.get(idx)?;
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        Some(line.to_str_lossy().into_owned())
    }

    /// Append the content to `out`.
    pub fn write_to(&self, out: &mut BString) {
        for line in &self.lines {
            out.extend_from_slice(line);
        }
    }

    pub fn to_bytes(&self) -> BString {
        let mut out = BString::default();
        self.write_to(&mut out);
        out
    }
}

/// One term of a conflict hunk: a side's or base's content.
#[derive(Clone)]
pub struct ConflictTerm {
    pub kind: ConflictTermKind,
    /// The file does not exist on this term (deleted, or never created).
    /// Absent terms materialize as empty content, so this flag is the only
    /// way to distinguish deletion from an empty file.
    pub absent: bool,
    pub text: ConflictText,
    /// Word-level tokens per line, highlighting what this side changed
    /// relative to its base. Empty for base and absent terms; otherwise one
    /// list per line of `text`.
    pub token_lines: Vec<Vec<DiffToken>>,
}

/// Context lines kept visible on each conflict-adjacent edge of a
/// resolved hunk; the rest hides behind an expandable gap row.
pub const CONFLICT_CONTEXT_LINES: usize = 3;

/// How a resolved hunk's lines split when trimmed: `head` leading and
/// `tail` trailing lines stay visible next to adjacent conflicts, and
/// `hidden` lines collapse behind a gap row between them.
#[derive(Clone, Copy)]
pub struct TrimmedContext {
    pub head: usize,
    pub tail: usize,
    pub hidden: usize,
}

/// A hunk's resolution choice.
#[derive(Clone, PartialEq, Eq)]
pub enum ConflictPick {
    /// One of the hunk's terms (a side or the base).
    Term(ConflictTermKind),
    /// A hand-edited resolution.
    Edited(ConflictText),
}

impl ConflictPick {
    /// The picked term's kind, if the pick is a term.
    pub fn term(&self) -> Option<ConflictTermKind> {
        match self {
            Self::Term(kind) => Some(*kind),
            Self::Edited(_) => None,
        }
    }
}

/// A conflicted file assembled under the user's picks. `complete` is true
/// when every hunk was resolved or picked, so the content is conflict-free;
/// when false the content still carries markers for the unpicked hunks (jj
/// parses those back into a conflicted state).
pub struct Resolution {
    pub content: BString,
    pub complete: bool,
}

/// Pure conflict data as materialized from the repo. All user-facing UI
/// state (picks, base-fold, gap-expansion) lives App-side in `conflict_ui`,
/// keyed by `(CommitId, RepoPath, hunk_idx)`, so these values carry no
/// interaction state and can be freely reloaded.
#[derive(Clone)]
pub enum ConflictHunkKind {
    /// Auto-resolved section: just context lines.
    Resolved { text: ConflictText },
    /// Conflicted section with multiple terms to choose from.
    Conflict {
        /// Terms in jj's materialized order: side 1, base 1, side 2, ...
        terms: Vec<ConflictTerm>,
    },
}

impl ConflictHunkKind {
    /// Number of positive terms (sides) in a conflict hunk; 0 for resolved.
    pub fn num_sides(&self) -> usize {
        match self {
            Self::Resolved { .. } => 0,
            Self::Conflict { terms, .. } => terms.len().div_ceil(2),
        }
    }

    /// Context trimming for a resolved hunk. `first`/`last` say whether the
    /// hunk starts/ends the file (edges with no adjacent conflict keep no
    /// context). `expanded` (App-side UI state) shows every line. `None` =
    /// show every line (expanded, small, or not a resolved hunk).
    pub fn trimmed_context(
        &self,
        expanded: bool,
        first: bool,
        last: bool,
    ) -> Option<TrimmedContext> {
        let Self::Resolved { text } = self else {
            return None;
        };
        if expanded {
            return None;
        }
        let n = text.line_count();
        let head = if first { 0 } else { CONFLICT_CONTEXT_LINES };
        let tail = if last { 0 } else { CONFLICT_CONTEXT_LINES };
        let hidden = n.saturating_sub(head + tail);
        // A one-line gap saves nothing over showing the line.
        if hidden < 2 {
            return None;
        }
        Some(TrimmedContext { head, tail, hidden })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_text_round_trips_content() {
        let contents: [&[u8]; 10] = [
            b"a\nb\n",
            b"a\nb",
            b"",
            b"\n",
            b"a\n\n",
            b"no newline",
            b"a\r\nb\r\n",
            b"a\r\nb",
            b"mixed\r\nendings\n",
            b"latin-1 \xe9\n",
        ];
        for content in contents {
            let text = ConflictText::from_bytes(content);
            assert_eq!(
                text.to_bytes(),
                content,
                "round trip failed for {content:?}"
            );
        }
    }

    #[test]
    fn a_line_displays_without_its_terminator() {
        let text = ConflictText::from_bytes(b"crlf\r\nlf\n\nlast \xe9");
        let shown: Vec<_> = (0..text.line_count())
            .map(|i| text.display_line(i).unwrap())
            .collect();
        assert_eq!(shown, ["crlf", "lf", "", "last \u{fffd}"]);
    }
}

#[cfg(test)]
mod marker_style_tests {
    use super::{MARKER_STYLE, MARKER_STYLE_CONFIG};

    /// Parse the config name with jj-lib's own `Deserialize` (the same impl
    /// that reads the `--config` we emit), and check it lands on the style we
    /// actually materialize with. Fails if the two constants drift apart, and
    /// equally if jj-lib renames the variant out from under the name.
    #[test]
    fn the_config_name_parses_to_the_style_we_materialize_with() {
        let parsed: jj_lib::conflicts::ConflictMarkerStyle =
            serde_json::from_str(&format!("\"{MARKER_STYLE_CONFIG}\"")).unwrap_or_else(|e| {
                panic!("jj-lib rejects {MARKER_STYLE_CONFIG:?} as a marker style: {e}")
            });
        assert_eq!(parsed, MARKER_STYLE);
    }
}
