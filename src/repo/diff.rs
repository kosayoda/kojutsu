use std::collections::HashSet;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{
    ConflictMaterializeOptions, MaterializedTreeValue, materialize_tree_value,
    try_materialize_file_conflict_value,
};
use jj_lib::diff_presentation::DiffTokenType;
use jj_lib::diff_presentation::unified::{self, DiffLineType, git_diff_part};
use jj_lib::matchers::EverythingMatcher;
use jj_lib::merge::{Diff, Merge, MergedTreeValue};
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::Repo;
use jj_lib::repo_path::{RepoPath as JjRepoPath, RepoPathBuf};
use jj_lib::store::Store;
use pollster::FutureExt as _;

use super::JjRepo;
use crate::dag::{
    CommitDetails, DiffLine, DiffLineKind, DiffResult, FileChange, FileStatus, LineStats,
};
use crate::types::{CommitId as UiCommitId, RepoPath};

/// Max bytes of file content (per side) materialized for a diff. This is a
/// memory guard, not a latency cap — diff work runs on background workers.
/// Larger files get a placeholder instead of being loaded into memory.
const MAX_DIFF_FILE_SIZE: usize = 64 * 1024 * 1024;

/// Bytes examined for the binary heuristic (git's null-byte scan).
const BINARY_SNIFF_SIZE: usize = 8000;

/// One side of a file diff, extracted with a size cap.
enum DiffSideContent {
    Text(bstr::BString),
    Binary,
    /// Regular file larger than [`MAX_DIFF_FILE_SIZE`]; content not loaded.
    TooLarge,
}

/// Materialize one side of a file diff. Regular files are read through a
/// size cap so oversized blobs are never fully loaded (jj's `git_diff_part`
/// reads whole files before its binary check and has no cap); other tree
/// values (symlinks, conflicts, submodules) still go through `git_diff_part`.
fn materialize_diff_side(
    store: &Arc<Store>,
    path: &JjRepoPath,
    value: MergedTreeValue,
    labels: &ConflictLabels,
    materialize_options: &ConflictMaterializeOptions,
) -> Result<DiffSideContent> {
    let materialized = materialize_tree_value(store, path, value, labels).block_on()?;
    if let MaterializedTreeValue::File(mut file) = materialized {
        let read_err = |e: futures::io::Error| {
            color_eyre::eyre::eyre!("failed to read {}: {e}", path.as_internal_file_string())
        };
        // Sniff the first 8k for a null byte (git's binary heuristic) so
        // binary blobs are detected without reading their full content.
        let mut contents = Vec::new();
        (&mut file.reader)
            .take(BINARY_SNIFF_SIZE as u64)
            .read_to_end(&mut contents)
            .block_on()
            .map_err(read_err)?;
        if contents.contains(&0) {
            return Ok(DiffSideContent::Binary);
        }
        (&mut file.reader)
            .take((MAX_DIFF_FILE_SIZE + 1 - contents.len()) as u64)
            .read_to_end(&mut contents)
            .block_on()
            .map_err(read_err)?;
        if contents.len() > MAX_DIFF_FILE_SIZE {
            return Ok(DiffSideContent::TooLarge);
        }
        return Ok(DiffSideContent::Text(contents.into()));
    }
    let part = git_diff_part(path, materialized, materialize_options)
        .block_on()
        .map_err(|e| color_eyre::eyre::eyre!("diff error: {e}"))?;
    Ok(if part.content.is_binary {
        DiffSideContent::Binary
    } else {
        DiffSideContent::Text(part.content.contents)
    })
}

/// A single-line placeholder shown instead of a real diff (binary or
/// oversized files).
fn placeholder_diff(text: impl Into<String>) -> DiffResult {
    let line = DiffLine {
        kind: DiffLineKind::Header,
        content: text.into(),
        tokens: vec![],
        old_line: None,
        new_line: None,
    };
    DiffResult {
        git: vec![line.clone()],
        color_words: vec![line],
    }
}

fn too_large_placeholder() -> DiffResult {
    placeholder_diff(format!(
        "(file larger than {} MiB — diff skipped)",
        MAX_DIFF_FILE_SIZE / (1024 * 1024)
    ))
}

impl JjRepo {
    /// Compute the file-level changes and line totals for a commit.
    pub fn commit_details(&self, commit_id: &UiCommitId) -> Result<CommitDetails> {
        let repo = self.repo.as_ref();
        let commit_hex_id = commit_id.as_str();
        let commit_id = BackendCommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit
            .parent_tree(repo)
            .block_on()
            .wrap_err("failed to get parent tree")?;
        let commit_tree = commit.tree();

        let mut changes = Vec::new();
        let mut stats = LineStats::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        // Build copy records for rename/copy detection.
        let mut copy_records = jj_lib::copies::CopyRecords::default();
        for parent_id in commit.parent_ids() {
            match repo.store().get_copy_records(None, parent_id, commit.id()) {
                Ok(stream) => {
                    use futures::TryStreamExt as _;
                    let records: Vec<_> = stream
                        .try_collect()
                        .block_on()
                        .inspect_err(|e| tracing::warn!("failed to collect copy records: {e}"))
                        .unwrap_or_default();
                    copy_records.add_records(records);
                }
                Err(e) => {
                    tracing::warn!("failed to get copy records: {e}");
                }
            }
        }

        let mut diff_stream =
            parent_tree.diff_stream_with_copies(&commit_tree, &EverythingMatcher, &copy_records);

        while let Some(entry) = diff_stream.next().block_on() {
            let target_path = entry.path.target().as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("diff entry error for {target_path}: {e}");
                    changes.push(FileChange {
                        path: RepoPath::new(&target_path),
                        old_path: None,
                        status: FileStatus::Error,
                        has_conflict: false,
                        stats: LineStats::default(),
                    });
                    continue;
                }
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();

            let (status, old_path) = if let Some(copy_op) = entry.path.copy_operation() {
                match copy_op {
                    jj_lib::copies::CopyOperation::Rename => (
                        FileStatus::Renamed,
                        entry
                            .path
                            .source
                            .as_ref()
                            .map(|(p, _)| RepoPath::new(p.as_internal_file_string())),
                    ),
                    jj_lib::copies::CopyOperation::Copy => (
                        FileStatus::Copied,
                        entry
                            .path
                            .source
                            .as_ref()
                            .map(|(p, _)| RepoPath::new(p.as_internal_file_string())),
                    ),
                }
            } else {
                match (before_present, after_present) {
                    (false, true) => (FileStatus::Added, None),
                    (true, false) => (FileStatus::Deleted, None),
                    (true, true) => (FileStatus::Modified, None),
                    (false, false) => continue,
                }
            };

            let has_conflict = !values.after.is_resolved();
            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: old_path.clone(),
                status,
                has_conflict,
                stats: LineStats::default(),
            });

            // For stats: materialize before content from old path if renamed/copied.
            let before_repo_path = old_path
                .as_ref()
                .and_then(|p| RepoPathBuf::from_internal_string(p.as_str()).ok())
                .unwrap_or_else(|| {
                    RepoPathBuf::from_internal_string(&target_path).expect("target path is valid")
                });
            let after_repo_path =
                RepoPathBuf::from_internal_string(&target_path).expect("target path is valid");

            let before = materialize_diff_side(
                repo.store(),
                &before_repo_path,
                values.before,
                &labels,
                &materialize_options,
            )?;
            let after = materialize_diff_side(
                repo.store(),
                &after_repo_path,
                values.after,
                &labels,
                &materialize_options,
            )?;
            let (DiffSideContent::Text(before), DiffSideContent::Text(after)) = (before, after)
            else {
                continue;
            };

            let contents = Diff::new(before.as_ref(), after.as_ref());
            let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
            let file_stats = count_line_stats(&hunks);
            stats.added = stats.added.saturating_add(file_stats.added);
            stats.removed = stats.removed.saturating_add(file_stats.removed);
            if let Some(fc) = changes.last_mut() {
                fc.stats = file_stats;
            }
        }

        // Add any conflicted files not already found by the diff pass.
        if commit.has_conflict() {
            let existing: HashSet<RepoPath> = changes.iter().map(|c| c.path.clone()).collect();
            for (path, _) in commit_tree.conflicts() {
                let repo_path = RepoPath::new(path.as_internal_file_string());
                if !existing.contains(&repo_path) {
                    changes.push(FileChange {
                        path: repo_path,
                        old_path: None,
                        status: FileStatus::Modified,
                        has_conflict: true,
                        stats: LineStats::default(),
                    });
                }
            }
        }

        let is_empty = changes.is_empty();
        Ok(CommitDetails {
            files: changes,
            stats,
            is_empty,
        })
    }

    /// Compute the line-level diff for a single file in a commit.
    /// For renamed/copied files, `old_path` provides the source path to diff against.
    /// Returns (git_diff_lines, color_words_lines).
    pub fn file_diff(
        &self,
        commit_id: &UiCommitId,
        path: &RepoPath,
        old_path: Option<&RepoPath>,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let commit_hex_id = commit_id.as_str();
        let commit_id = BackendCommitId::try_from_hex(commit_hex_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex: {commit_hex_id}"))?;
        let commit = repo
            .store()
            .get_commit(&commit_id)
            .wrap_err("failed to load commit for diff")?;

        let parent_tree = commit.parent_tree(repo).block_on()?;
        let commit_tree = commit.tree();
        self.trees_file_diff(&parent_tree, &commit_tree, path, old_path)
    }

    /// Get the conflict hunks for a conflicted file, broken down by hunk.
    /// Returns a list of resolved (context) and conflicted hunks.
    pub fn conflict_hunks(
        &self,
        commit_id: &UiCommitId,
        path: &RepoPath,
    ) -> Result<Vec<crate::dag::ConflictHunkKind>> {
        use crate::dag::ConflictHunkKind;

        let repo = self.repo.as_ref();
        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let commit = repo
            .store()
            .get_commit(&backend_id)
            .wrap_err("failed to load commit")?;
        let tree = commit.tree();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let tree_value = tree.path_value(&repo_path).block_on()?;
        let labels = ConflictLabels::unlabeled();

        let Some(materialized) =
            try_materialize_file_conflict_value(repo.store(), &repo_path, &tree_value, &labels)
                .block_on()
                .wrap_err("failed to materialize conflict")?
        else {
            return Ok(vec![]);
        };

        let merge_options = jj_lib::tree_merge::MergeOptions {
            hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
            same_change: jj_lib::merge::SameChange::Accept,
        };
        let merge_result = jj_lib::files::merge_hunks(&materialized.contents, &merge_options);

        let hunks = match merge_result {
            jj_lib::files::MergeResult::Resolved(content) => {
                let lines = String::from_utf8_lossy(&content)
                    .lines()
                    .map(String::from)
                    .collect();
                vec![ConflictHunkKind::Resolved { lines }]
            }
            jj_lib::files::MergeResult::Conflict(merge_hunks) => {
                merge_hunks
                    .into_iter()
                    .map(|hunk| {
                        if let Some(resolved) = hunk.as_resolved() {
                            let lines = String::from_utf8_lossy(resolved.as_ref())
                                .lines()
                                .map(String::from)
                                .collect();
                            ConflictHunkKind::Resolved { lines }
                        } else {
                            // Collect each side's content as lines.
                            let sides: Vec<Vec<String>> = hunk
                                .iter()
                                .map(|side| {
                                    String::from_utf8_lossy(side.as_ref())
                                        .lines()
                                        .map(String::from)
                                        .collect()
                                })
                                .collect();
                            ConflictHunkKind::Conflict {
                                sides,
                                selected: None,
                            }
                        }
                    })
                    .collect()
            }
        };

        Ok(hunks)
    }

    /// Compute file-level changes between two commits (for evolog level-1 unfold).
    pub fn inter_commit_details(&self, from_id: &str, to_id: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_tree = from_commit.tree();
        let to_tree = to_commit.tree();
        let copy_records = jj_lib::copies::CopyRecords::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let mut changes = Vec::new();
        let mut diff_stream =
            from_tree.diff_stream_with_copies(&to_tree, &EverythingMatcher, &copy_records);

        while let Some(entry) = diff_stream.next().block_on() {
            let path = entry.path.target();
            let target_path = path.as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("inter-commit diff entry error for {target_path}: {e}");
                    changes.push(FileChange {
                        path: RepoPath::new(&target_path),
                        old_path: None,
                        status: FileStatus::Error,
                        has_conflict: false,
                        stats: LineStats::default(),
                    });
                    continue;
                }
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();
            let status = match (before_present, after_present) {
                (false, true) => FileStatus::Added,
                (true, false) => FileStatus::Deleted,
                (true, true) => FileStatus::Modified,
                (false, false) => continue,
            };
            let has_conflict = !values.after.is_resolved();

            // Compute line stats by materializing + diffing.
            let mut file_stats = LineStats::default();
            let before = materialize_diff_side(
                repo.store(),
                path,
                values.before,
                &labels,
                &materialize_options,
            )?;
            let after = materialize_diff_side(
                repo.store(),
                path,
                values.after,
                &labels,
                &materialize_options,
            )?;
            if let (DiffSideContent::Text(before), DiffSideContent::Text(after)) = (before, after) {
                let contents = Diff::new(before.as_ref(), after.as_ref());
                let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
                file_stats = count_line_stats(&hunks);
            }

            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: None,
                status,
                has_conflict,
                stats: file_stats,
            });
        }

        Ok(changes)
    }

    /// Compute the diff for a single file between two commits (for evolog level-2 unfold).
    pub fn inter_commit_file_diff(
        &self,
        from_id: &str,
        to_id: &str,
        path: &RepoPath,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_tree = from_commit.tree();
        let to_tree = to_commit.tree();
        self.trees_file_diff(&from_tree, &to_tree, path, None)
    }

    /// Compute the diff for a single file between two trees.
    /// For renamed/copied files, `old_path` provides the source path in the
    /// before tree to diff against.
    fn trees_file_diff(
        &self,
        before_tree: &MergedTree,
        after_tree: &MergedTree,
        path: &RepoPath,
        old_path: Option<&RepoPath>,
    ) -> Result<DiffResult> {
        let repo = self.repo.as_ref();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let before_repo_path = old_path
            .and_then(|p| RepoPathBuf::from_internal_string(p.as_str()).ok())
            .unwrap_or_else(|| repo_path.clone());
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let before_value = before_tree.path_value(&before_repo_path).block_on()?;
        let after_value = after_tree.path_value(&repo_path).block_on()?;

        let before = materialize_diff_side(
            repo.store(),
            &before_repo_path,
            before_value,
            &labels,
            &materialize_options,
        )?;
        let after = materialize_diff_side(
            repo.store(),
            &repo_path,
            after_value,
            &labels,
            &materialize_options,
        )?;

        let (before, after) = match (before, after) {
            (DiffSideContent::Text(b), DiffSideContent::Text(a)) => (b, a),
            (DiffSideContent::TooLarge, _) | (_, DiffSideContent::TooLarge) => {
                return Ok(too_large_placeholder());
            }
            _ => return Ok(placeholder_diff("(binary file)")),
        };

        let contents = Diff::new(before.as_ref(), after.as_ref());
        let hunks = unified::unified_diff_hunks(
            contents,
            3, // context lines
            Default::default(),
        );

        let mut git_lines = Vec::new();
        hunks_to_diff_lines(&hunks, &mut git_lines);
        let mut cw_lines = Vec::new();
        hunks_to_color_words_lines(&hunks, &mut cw_lines);
        Ok(DiffResult {
            git: git_lines,
            color_words: cw_lines,
        })
    }

    /// Compute the rebased tree pair for an interdiff.
    /// Returns (rebased_from_tree, to_tree) where rebased_from_tree has from's
    /// changes applied onto to's parent base via 3-way merge.
    fn compute_interdiff_trees(
        &self,
        from_id: &str,
        to_id: &str,
    ) -> Result<(MergedTree, MergedTree)> {
        let repo = self.repo.as_ref();
        let from_commit_id = BackendCommitId::try_from_hex(from_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let to_commit_id = BackendCommitId::try_from_hex(to_id)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit ID hex"))?;
        let from_commit = repo.store().get_commit(&from_commit_id)?;
        let to_commit = repo.store().get_commit(&to_commit_id)?;

        let from_parent_tree = from_commit.parent_tree(repo).block_on()?;
        let from_tree = from_commit.tree();
        let to_parent_tree = to_commit.parent_tree(repo).block_on()?;
        let to_tree = to_commit.tree();

        let merge_input = Merge::from_removes_adds(
            [(from_parent_tree, String::new())],
            [(to_parent_tree, String::new()), (from_tree, String::new())],
        );
        let rebased_tree = MergedTree::merge(merge_input)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("tree merge failed: {e}"))?;

        Ok((rebased_tree, to_tree))
    }

    /// Compute file-level changes for an interdiff between two commits.
    /// Rebases `from` onto `to`'s parents, then diffs the result against `to`.
    pub fn interdiff_details(&self, from_id: &str, to_id: &str) -> Result<Vec<FileChange>> {
        let repo = self.repo.as_ref();
        let (rebased_tree, to_tree) = self.compute_interdiff_trees(from_id, to_id)?;

        let copy_records = jj_lib::copies::CopyRecords::default();
        let labels = ConflictLabels::unlabeled();
        let materialize_options = default_materialize_options();

        let mut changes = Vec::new();
        let mut diff_stream =
            rebased_tree.diff_stream_with_copies(&to_tree, &EverythingMatcher, &copy_records);

        while let Some(entry) = diff_stream.next().block_on() {
            let path = entry.path.target();
            let target_path = path.as_internal_file_string().to_string();
            let values = match entry.values {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("interdiff entry error for {target_path}: {e}");
                    changes.push(FileChange {
                        path: RepoPath::new(&target_path),
                        old_path: None,
                        status: FileStatus::Error,
                        has_conflict: false,
                        stats: LineStats::default(),
                    });
                    continue;
                }
            };

            let before_present = values.before.is_present();
            let after_present = values.after.is_present();
            let status = match (before_present, after_present) {
                (false, true) => FileStatus::Added,
                (true, false) => FileStatus::Deleted,
                (true, true) => FileStatus::Modified,
                (false, false) => continue,
            };
            let has_conflict = !values.after.is_resolved();

            let mut file_stats = LineStats::default();
            let before = materialize_diff_side(
                repo.store(),
                path,
                values.before,
                &labels,
                &materialize_options,
            )?;
            let after = materialize_diff_side(
                repo.store(),
                path,
                values.after,
                &labels,
                &materialize_options,
            )?;
            if let (DiffSideContent::Text(before), DiffSideContent::Text(after)) = (before, after) {
                let contents = Diff::new(before.as_ref(), after.as_ref());
                let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
                file_stats = count_line_stats(&hunks);
            }

            changes.push(FileChange {
                path: RepoPath::new(&target_path),
                old_path: None,
                status,
                has_conflict,
                stats: file_stats,
            });
        }

        Ok(changes)
    }

    /// Compute per-file diff for an interdiff between two commits.
    pub fn interdiff_file_diff(
        &self,
        from_id: &str,
        to_id: &str,
        path: &RepoPath,
    ) -> Result<DiffResult> {
        let (rebased_tree, to_tree) = self.compute_interdiff_trees(from_id, to_id)?;
        self.trees_file_diff(&rebased_tree, &to_tree, path, None)
    }

    /// Get the raw content of a file at a specific commit.
    pub fn get_file_at_commit(
        &self,
        commit_id: &UiCommitId,
        file_path: &RepoPath,
    ) -> Result<Vec<u8>> {
        let repo = self.repo.as_ref();
        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;
        let commit = repo.store().get_commit(&backend_id)?;
        let tree = commit.tree();
        let repo_path = RepoPathBuf::from_internal_string(file_path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let value = tree.path_value(&repo_path).block_on()?;
        let labels = ConflictLabels::unlabeled();
        let materialized =
            materialize_tree_value(repo.store(), &repo_path, value, &labels).block_on()?;
        match materialized {
            jj_lib::conflicts::MaterializedTreeValue::File(mut file) => {
                let buf = file.read_all(&repo_path).block_on()?;
                Ok(buf)
            }
            jj_lib::conflicts::MaterializedTreeValue::FileConflict(conflict) => {
                let options = default_materialize_options();
                let result = jj_lib::conflicts::materialize_merge_result_to_bytes(
                    &conflict.contents,
                    &conflict.labels,
                    &options,
                );
                Ok(result.into())
            }
            _ => color_eyre::eyre::bail!("path is not a regular file"),
        }
    }
}

fn default_materialize_options() -> ConflictMaterializeOptions {
    ConflictMaterializeOptions {
        marker_style: jj_lib::conflicts::ConflictMarkerStyle::Git,
        marker_len: None,
        merge: jj_lib::tree_merge::MergeOptions {
            hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
            same_change: jj_lib::merge::SameChange::Accept,
        },
    }
}

/// Count added/removed lines from unified diff hunks.
fn count_line_stats(hunks: &[unified::UnifiedDiffHunk<'_>]) -> LineStats {
    let mut stats = LineStats::default();
    for hunk in hunks {
        for (line_type, _) in &hunk.lines {
            match line_type {
                DiffLineType::Added => stats.added = stats.added.saturating_add(1),
                DiffLineType::Removed => stats.removed = stats.removed.saturating_add(1),
                DiffLineType::Context => {}
            }
        }
    }
    stats
}

fn hunks_to_diff_lines(hunks: &[unified::UnifiedDiffHunk<'_>], out: &mut Vec<DiffLine>) {
    for hunk in hunks {
        out.push(DiffLine {
            kind: DiffLineKind::Header,
            content: format!(
                "@@ -{},{} +{},{} @@",
                hunk.left_line_range.start + 1,
                hunk.left_line_range.len(),
                hunk.right_line_range.start + 1,
                hunk.right_line_range.len(),
            ),
            tokens: vec![],
            old_line: None,
            new_line: None,
        });

        let mut old_line = hunk.left_line_range.start as u32 + 1;
        let mut new_line = hunk.right_line_range.start as u32 + 1;

        for (line_type, tokens) in &hunk.lines {
            let mut diff_tokens = Vec::new();
            let mut full_text = String::new();
            for (tag, bytes) in tokens {
                let text = String::from_utf8_lossy(bytes);
                full_text.push_str(&text);
                let text = text.into_owned();
                let kind = match tag {
                    DiffTokenType::Matching => crate::dag::DiffTokenKind::Unchanged,
                    DiffTokenType::Different => match line_type {
                        DiffLineType::Removed => crate::dag::DiffTokenKind::Removed,
                        _ => crate::dag::DiffTokenKind::Added,
                    },
                };
                diff_tokens.push(crate::dag::DiffToken { text, kind });
            }
            if let Some(last) = diff_tokens.last_mut() {
                last.text = last.text.trim_end_matches('\n').to_string();
            }
            let text = full_text.trim_end_matches('\n').to_string();

            let (kind, ol, nl) = match line_type {
                DiffLineType::Context => {
                    let r = (DiffLineKind::Context, Some(old_line), Some(new_line));
                    old_line += 1;
                    new_line += 1;
                    r
                }
                DiffLineType::Removed => {
                    let r = (DiffLineKind::Removed, Some(old_line), None);
                    old_line += 1;
                    r
                }
                DiffLineType::Added => {
                    let r = (DiffLineKind::Added, None, Some(new_line));
                    new_line += 1;
                    r
                }
            };
            out.push(DiffLine {
                kind,
                content: text,
                tokens: diff_tokens,
                old_line: ol,
                new_line: nl,
            });
        }
    }
}

/// Count how many times a word diff alternates between removed and added content.
/// Used to decide whether to inline color-words or fall back to separate lines.
fn count_alternations(diff: &jj_lib::diff::ContentDiff<'_>) -> usize {
    let mut count: usize = 0;
    let mut last_side: Option<u8> = None;
    for h in diff.hunks() {
        if h.kind == jj_lib::diff::DiffHunkKind::Matching {
            continue;
        }
        if !h.contents[0].is_empty() && last_side != Some(0) {
            count += 1;
            last_side = Some(0);
        }
        if !h.contents[1].is_empty() && last_side != Some(1) {
            count += 1;
            last_side = Some(1);
        }
    }
    count
}

/// Accumulator for building color-words diff lines from consecutive
/// removed/added blocks.
struct ColorWordBuilder {
    removed_lines: Vec<String>,
    added_lines: Vec<String>,
    removed_count: u32,
    added_count: u32,
    old_line: u32,
    new_line: u32,
}

impl ColorWordBuilder {
    fn new(old_start: u32, new_start: u32) -> Self {
        Self {
            removed_lines: Vec::new(),
            added_lines: Vec::new(),
            removed_count: 0,
            added_count: 0,
            old_line: old_start,
            new_line: new_start,
        }
    }

    fn push_removed(&mut self, text: String) {
        self.removed_lines.push(text);
        self.removed_count += 1;
    }

    fn push_added(&mut self, text: String) {
        self.added_lines.push(text);
        self.added_count += 1;
    }

    fn has_pending_added_only(&self) -> bool {
        !self.added_lines.is_empty() && self.removed_lines.is_empty()
    }

    /// Flush a collected removed+added block into output DiffLines.
    fn flush(&mut self, out: &mut Vec<DiffLine>) {
        use crate::dag::{DiffToken, DiffTokenKind};
        const MAX_ALTERNATION: usize = 3;

        if self.removed_lines.is_empty() && self.added_lines.is_empty() {
            return;
        }

        let removed_block = self.removed_lines.join("\n");
        let added_block = self.added_lines.join("\n");

        // Word-diff once, use for both alternation check and rendering.
        let can_inline = !self.removed_lines.is_empty() && !self.added_lines.is_empty();
        let diff = if can_inline {
            Some(jj_lib::diff::ContentDiff::by_word([
                removed_block.as_bytes(),
                added_block.as_bytes(),
            ]))
        } else {
            None
        };

        let should_inline = diff
            .as_ref()
            .is_some_and(|d| count_alternations(d) <= MAX_ALTERNATION);

        if should_inline {
            let diff = diff.unwrap();
            let mut tokens: Vec<DiffToken> = Vec::new();
            let mut content = String::new();
            let base_old = self.old_line;
            let base_new = self.new_line;

            for h in diff.hunks() {
                match h.kind {
                    jj_lib::diff::DiffHunkKind::Matching => {
                        let text = String::from_utf8_lossy(h.contents[0]);
                        for (i, part) in text.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: Some(self.old_line),
                                    new_line: Some(self.new_line),
                                });
                                self.old_line += 1;
                                self.new_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Unchanged,
                                });
                            }
                        }
                    }
                    jj_lib::diff::DiffHunkKind::Different => {
                        let removed = String::from_utf8_lossy(h.contents[0]);
                        let added = String::from_utf8_lossy(h.contents[1]);

                        for (i, part) in removed.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: Some(self.old_line),
                                    new_line: None,
                                });
                                self.old_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Removed,
                                });
                            }
                        }

                        for (i, part) in added.split('\n').enumerate() {
                            if i > 0 {
                                out.push(DiffLine {
                                    kind: DiffLineKind::Context,
                                    content: std::mem::take(&mut content),
                                    tokens: std::mem::take(&mut tokens),
                                    old_line: None,
                                    new_line: Some(self.new_line),
                                });
                                self.new_line += 1;
                            }
                            if !part.is_empty() {
                                content.push_str(part);
                                tokens.push(DiffToken {
                                    text: part.to_string(),
                                    kind: DiffTokenKind::Added,
                                });
                            }
                        }
                    }
                }
            }

            if !content.is_empty() || !tokens.is_empty() {
                let has_removed = tokens.iter().any(|t| t.kind == DiffTokenKind::Removed);
                let has_added = tokens.iter().any(|t| t.kind == DiffTokenKind::Added);
                out.push(DiffLine {
                    kind: DiffLineKind::Context,
                    content,
                    tokens,
                    old_line: if self.old_line > base_old || has_removed || !has_added {
                        Some(self.old_line)
                    } else {
                        None
                    },
                    new_line: if self.new_line > base_new || has_added || !has_removed {
                        Some(self.new_line)
                    } else {
                        None
                    },
                });
            }

            self.old_line = base_old + self.removed_count;
            self.new_line = base_new + self.added_count;
        } else {
            for line in self.removed_lines.drain(..) {
                out.push(DiffLine {
                    kind: DiffLineKind::Removed,
                    content: line.clone(),
                    tokens: vec![DiffToken {
                        text: line,
                        kind: DiffTokenKind::Removed,
                    }],
                    old_line: Some(self.old_line),
                    new_line: None,
                });
                self.old_line += 1;
            }
            for line in self.added_lines.drain(..) {
                out.push(DiffLine {
                    kind: DiffLineKind::Added,
                    content: line.clone(),
                    tokens: vec![DiffToken {
                        text: line,
                        kind: DiffTokenKind::Added,
                    }],
                    old_line: None,
                    new_line: Some(self.new_line),
                });
                self.new_line += 1;
            }
        }

        self.removed_lines.clear();
        self.added_lines.clear();
        self.removed_count = 0;
        self.added_count = 0;
    }
}

/// Convert unified diff hunks into color-words `DiffLine` structs.
fn hunks_to_color_words_lines(hunks: &[unified::UnifiedDiffHunk<'_>], out: &mut Vec<DiffLine>) {
    use crate::dag::{DiffToken, DiffTokenKind};

    for hunk in hunks {
        out.push(DiffLine {
            kind: DiffLineKind::Header,
            content: format!(
                "@@ -{},{} +{},{} @@",
                hunk.left_line_range.start + 1,
                hunk.left_line_range.len(),
                hunk.right_line_range.start + 1,
                hunk.right_line_range.len(),
            ),
            tokens: vec![],
            old_line: None,
            new_line: None,
        });

        let mut builder = ColorWordBuilder::new(
            hunk.left_line_range.start as u32 + 1,
            hunk.right_line_range.start as u32 + 1,
        );

        for (line_type, tokens) in &hunk.lines {
            let mut text = String::new();
            for (_, bytes) in tokens {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
            let text = text.trim_end_matches('\n').to_string();

            match line_type {
                DiffLineType::Removed => {
                    if builder.has_pending_added_only() {
                        builder.flush(out);
                    }
                    builder.push_removed(text);
                }
                DiffLineType::Added => {
                    builder.push_added(text);
                }
                DiffLineType::Context => {
                    builder.flush(out);
                    out.push(DiffLine {
                        kind: DiffLineKind::Context,
                        content: text.clone(),
                        tokens: vec![DiffToken {
                            text,
                            kind: DiffTokenKind::Unchanged,
                        }],
                        old_line: Some(builder.old_line),
                        new_line: Some(builder.new_line),
                    });
                    builder.old_line += 1;
                    builder.new_line += 1;
                }
            }
        }
        builder.flush(out);
    }
}
