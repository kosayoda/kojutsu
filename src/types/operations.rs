use super::id::ChangeId;

#[derive(Debug, Clone)]
pub struct SquashTarget {
    pub target: ChangeId,
    pub kind: SquashKind,
}

#[derive(Debug, Clone)]
pub struct SplitTarget {
    pub target: ChangeId,
    pub kind: SplitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub target: ChangeId,
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
