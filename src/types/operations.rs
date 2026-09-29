use std::path::PathBuf;

use super::id::{RevisionArg, SmallVec, Str};

#[derive(Debug, Clone)]
pub struct SquashTarget {
    pub target: RevisionArg,
    pub kind: SquashKind,
}

#[derive(Debug, Clone)]
pub struct SplitTarget {
    pub target: RevisionArg,
    pub kind: SplitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SquashKind {
    Into,
    Onto,
    After,
    Before,
}

impl SquashKind {
    pub fn flag(&self) -> &'static str {
        match self {
            SquashKind::Into => "--into",
            SquashKind::Onto => "--onto",
            SquashKind::After => "--insert-after",
            SquashKind::Before => "--insert-before",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitKind {
    Onto,
    After,
    Before,
}

impl SplitKind {
    pub fn flag(&self) -> &'static str {
        match self {
            SplitKind::Onto => "--onto",
            SplitKind::After => "--insert-after",
            SplitKind::Before => "--insert-before",
        }
    }
}

/// Direction for arrange (commit reorder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrangeDirection {
    Up,
    Down,
}

/// Direction for row navigation (next/previous), wrapping around the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NavDirection {
    Forward,
    Backward,
}

/// Rebase source mode.
#[derive(Debug, Clone)]
pub enum RebaseSource {
    Revision,
    Source,
    Branch,
}

/// Rebase destination mode.
#[derive(Debug, Clone, strum::EnumIter)]
pub enum RebaseKind {
    Onto,
    After,
    Before,
}

impl RebaseKind {
    pub fn key(&self) -> char {
        match self {
            RebaseKind::Onto => 'o',
            RebaseKind::After => 'a',
            RebaseKind::Before => 'b',
        }
    }

    pub fn flag(&self) -> &'static str {
        match self {
            RebaseKind::Onto => "-d",
            RebaseKind::After => "-A",
            RebaseKind::Before => "-B",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RebaseKind::Onto => "onto",
            RebaseKind::After => "after",
            RebaseKind::Before => "before",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RebaseTarget {
    pub targets: SmallVec<RevisionArg>,
    pub kind: RebaseKind,
}

/// How to handle the commit message during squash.
#[derive(Debug, Clone)]
pub enum MessageMode {
    /// Let jj handle it (auto-merge, opens editor if needed).
    Default,
    /// Use -m "message".
    Inline(String),
    /// Use --use-destination-message.
    UseDestination,
}

/// How to filter changes for squash/commit operations.
#[derive(Debug, Clone)]
pub enum ChangeSelection {
    /// Include all changes (no filtering).
    All,
    /// Include only these files (maps to `[FILESETS]` positional args).
    Files(Vec<Str>),
    /// Line-level selection (maps to --interactive --tool with selection JSON).
    Lines(PathBuf),
}
