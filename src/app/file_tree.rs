use super::Loadable;
use crate::dag::{
    DiffFormat, DiffLine, DiffResult, DiffSummary, DiffTarget, FileChange, LineStats,
};
use crate::idx::FileIdx;
use crate::repo_service::{RepoError, RepoRequest};
use crate::types::RepoPath;

/// A diff target's changed files, each unfoldable to its lazily loaded
/// diff. Shared by every view that lists files: a DAG commit, an evolog
/// step, an interdiff.
pub struct FileTree {
    target: DiffTarget,
    summary: Loadable<DiffSummary>,
    /// Parallel to the summary's files.
    diffs: Vec<Loadable<DiffResult>>,
}

impl FileTree {
    pub fn new(target: DiffTarget) -> Self {
        Self {
            target,
            summary: Loadable::NotRequested,
            diffs: Vec::new(),
        }
    }

    pub fn target(&self) -> &DiffTarget {
        &self.target
    }

    pub fn summary(&self) -> &Loadable<DiffSummary> {
        &self.summary
    }

    pub fn files(&self) -> Option<&[FileChange]> {
        self.summary.loaded().map(|s| s.files.as_slice())
    }

    pub fn file(&self, file_idx: FileIdx) -> Option<&FileChange> {
        self.files()?.get(file_idx.raw())
    }

    pub fn file_idx(&self, path: &RepoPath) -> Option<FileIdx> {
        self.files()?
            .iter()
            .position(|f| f.path == *path)
            .map(FileIdx::new)
    }

    pub fn stats(&self) -> Option<LineStats> {
        self.summary.loaded().map(|s| s.stats)
    }

    /// Mark the file list as loading and return its request, unless it is
    /// already loaded or on its way.
    pub fn request_summary(&mut self) -> Option<RepoRequest> {
        self.summary.begin().then(|| RepoRequest::DiffSummary {
            target: self.target.clone(),
        })
    }

    pub fn set_summary(&mut self, result: Result<DiffSummary, RepoError>) {
        self.summary = result.into();
        self.diffs.clear();
    }

    pub fn diff(&self, file_idx: FileIdx) -> Option<&Loadable<DiffResult>> {
        self.diffs.get(file_idx.raw())
    }

    pub fn diff_lines(&self, file_idx: FileIdx, format: DiffFormat) -> Option<&Vec<DiffLine>> {
        Some(self.diff(file_idx)?.loaded()?.lines(format))
    }

    /// Mark a file's diff as loading and return its request, unless it is
    /// already loaded or on its way.
    pub fn request_diff(&mut self, file_idx: FileIdx) -> Option<RepoRequest> {
        let file = self.file(file_idx)?;
        let request = RepoRequest::FileDiff {
            target: self.target.clone(),
            path: file.path.clone(),
            old_path: file.old_path.clone(),
        };
        self.diff_slot(file_idx).begin().then_some(request)
    }

    /// Store a file's diff. Ignored if the file is no longer listed.
    pub fn set_diff(&mut self, path: &RepoPath, result: Result<DiffResult, RepoError>) {
        if let Some(file_idx) = self.file_idx(path) {
            *self.diff_slot(file_idx) = result.into();
        }
    }

    fn diff_slot(&mut self, file_idx: FileIdx) -> &mut Loadable<DiffResult> {
        let i = file_idx.raw();
        if self.diffs.len() <= i {
            self.diffs.resize_with(i + 1, || Loadable::NotRequested);
        }
        &mut self.diffs[i]
    }
}
