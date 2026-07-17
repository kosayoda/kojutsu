//! First-class merge-conflict model: pure repo data as materialized from
//! jj (terms, per-hunk content), plus the user's resolution choices. All
//! interaction state (which term is picked, base-fold, gap-expansion)
//! lives App-side in `conflict_ui` — these types carry none of it.

use crate::dag::DiffToken;

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

/// Text content stored as display lines. `str::lines` drops the final
/// newline, so whether the content ended with one is kept separately for
/// faithful reassembly.
#[derive(Clone, PartialEq, Eq)]
pub struct ConflictText {
    pub lines: Vec<String>,
    pub trailing_newline: bool,
}

impl ConflictText {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            lines: String::from_utf8_lossy(bytes)
                .lines()
                .map(String::from)
                .collect(),
            trailing_newline: bytes.ends_with(b"\n"),
        }
    }

    /// Append the content to `out`, restoring line terminators.
    pub fn write_to(&self, out: &mut String) {
        for (i, line) in self.lines.iter().enumerate() {
            out.push_str(line);
            if i + 1 < self.lines.len() || self.trailing_newline {
                out.push('\n');
            }
        }
    }

    /// The content as an owned string, with line terminators restored.
    pub fn to_content(&self) -> String {
        let mut s = String::new();
        self.write_to(&mut s);
        s
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
    /// relative to its base. Empty for base and absent terms. Parallel to
    /// `text.lines` when present.
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
    pub content: String,
    pub complete: bool,
}

/// Pure conflict data as materialized from the repo. All user-facing UI
/// state (picks, base-fold, gap-expansion) lives App-side in `conflict_ui`,
/// keyed by `(CommitId, RepoPath, hunk_idx)`, so these values carry no
/// interaction state and can be freely reloaded.
#[derive(Clone)]
pub enum ConflictHunkKind {
    /// Auto-resolved section — just context lines.
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
        let n = text.lines.len();
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
        for content in ["a\nb\n", "a\nb", "", "\n", "a\n\n", "no newline"] {
            let text = ConflictText::from_bytes(content.as_bytes());
            let mut out = String::new();
            text.write_to(&mut out);
            assert_eq!(out, content, "round trip failed for {content:?}");
        }
    }
}
