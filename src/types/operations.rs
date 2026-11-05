use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    Lines(Arc<StagedFile>),
}

/// A file written for jj's tool to read, owned by the command that names
/// it: it lives as long as any clone of the command does and is removed
/// with the last one, so a dismissed prompt, a refusal or a failed run
/// leaves nothing behind. The tool removes it earlier when it runs.
#[derive(Debug)]
pub struct StagedFile(PathBuf);

impl StagedFile {
    /// A new private temp file holding `content`.
    pub fn write(content: &[u8]) -> std::io::Result<Self> {
        use std::io::Write as _;
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(content)?;
        file.flush()?;
        let (_, path) = file.keep().map_err(|e| e.error)?;
        Ok(Self(path))
    }

    /// Stand in for a file at `path` without writing one. Dropping it
    /// still removes whatever is at `path`.
    #[cfg(test)]
    pub fn adopt(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// A real, empty staged file, shared the way a command holds it.
    #[cfg(test)]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::write(b"").expect("a temp file"))
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
