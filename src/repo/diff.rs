use std::collections::HashSet;
use std::sync::Arc;

use bstr::ByteSlice as _;
use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use jj_lib::backend::{FileId, TreeValue};
use jj_lib::backend::{MergedTreeValue, MergedTreeValueExt as _};
use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{
    ConflictMaterializeOptions, MaterializedTreeValue, materialize_tree_value,
};
use jj_lib::diff_presentation::DiffTokenType;
use jj_lib::diff_presentation::unified::{self, DiffLineType};
use jj_lib::matchers::EverythingMatcher;
use jj_lib::merge::{Diff, Merge};
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo::Repo;
use jj_lib::repo_path::{RepoPath as JjRepoPath, RepoPathBuf};
use jj_lib::rewrite::rebase_to_dest_parent;
use jj_lib::store::Store;
use pollster::FutureExt as _;

use super::JjRepo;
use crate::conflict::{ConflictTerm, ConflictTermKind, ConflictText};
use crate::dag::{
    DiffLine, DiffLineKind, DiffResult, DiffSummary, DiffTarget, DiffToken, DiffTokenKind,
    FileChange, FileStatus, LineStats,
};
use crate::types::{CommitId as UiCommitId, RepoPath};

/// Bytes examined for the binary heuristic (git's null-byte scan).
const BINARY_SNIFF_SIZE: usize = 8000;

/// One side of a file diff, extracted with a size cap.
enum DiffSideContent {
    Text {
        content: bstr::BString,
        /// 1-based line ranges that are materialized conflict regions
        /// (markers plus the term content between them). Empty for
        /// unconflicted sides.
        conflict_regions: Vec<std::ops::Range<u32>>,
    },
    Binary,
    /// Regular file larger than the diff size limit; content not loaded.
    TooLarge,
}

impl DiffSideContent {
    fn text(content: impl Into<bstr::BString>) -> Self {
        Self::Text {
            content: content.into(),
            conflict_regions: Vec::new(),
        }
    }
}

/// Materialize one side of a file diff. Regular files are read through a
/// size cap (`limit` bytes, a memory guard rather than a latency cap; diff work
/// runs on background workers) so oversized blobs are never fully loaded.
/// Conflicted files materialize hunk by hunk so the conflict regions' line
/// ranges are tracked exactly. Values that have no content of their own
/// (symlinks, submodules, non-file conflicts) stand in for it with the
/// same one-line descriptions jj's own diffs use, so a change to one is
/// visible rather than rendering as an empty diff.
fn materialize_diff_side(
    store: &Arc<Store>,
    path: &JjRepoPath,
    value: MergedTreeValue,
    labels: &ConflictLabels,
    limit: usize,
) -> Result<DiffSideContent> {
    if !value.is_resolved() {
        match read_file_conflict(store, path, &value, labels, limit)? {
            Some(FileConflict::Terms(terms)) => {
                return Ok(materialize_conflict_with_regions(&terms.contents));
            }
            Some(FileConflict::TooLarge) => return Ok(DiffSideContent::TooLarge),
            None => {}
        }
    }
    let materialized = materialize_tree_value(store, path, value, labels).block_on()?;
    match materialized {
        MaterializedTreeValue::File(mut file) => {
            read_diff_file(&mut file.reader, limit).map_err(|e| {
                color_eyre::eyre::eyre!("failed to read `{}`: {e}", path.as_internal_file_string())
            })
        }
        MaterializedTreeValue::FileConflict(_) => {
            unreachable!("a conflict between files is read through the cap above")
        }
        MaterializedTreeValue::Absent => Ok(DiffSideContent::text("")),
        MaterializedTreeValue::Symlink { target, .. } => Ok(DiffSideContent::text(target)),
        MaterializedTreeValue::GitSubmodule(id) => Ok(DiffSideContent::text(format!(
            "Git submodule checked out at {id}"
        ))),
        MaterializedTreeValue::OtherConflict { id, labels } => {
            Ok(DiffSideContent::text(id.describe(&labels)))
        }
        MaterializedTreeValue::AccessDenied(err) => {
            Ok(DiffSideContent::text(format!("Access denied: {err}")))
        }
        MaterializedTreeValue::Tree(id) => Err(color_eyre::eyre::eyre!(
            "unexpected tree {id:?} in diff at {}",
            path.as_internal_file_string()
        )),
    }
}

/// Read a regular file's side of a diff, reading no more than the binary
/// sniff and `limit + 1` bytes: the first 8k are sniffed for a null byte
/// (git's binary heuristic) so binary blobs are detected without reading
/// their full content, and one byte past `limit` tells an oversized file
/// from one that fits exactly.
fn read_diff_file(
    mut reader: impl futures::AsyncRead + Unpin,
    limit: usize,
) -> futures::io::Result<DiffSideContent> {
    let mut contents = Vec::new();
    (&mut reader)
        .take(BINARY_SNIFF_SIZE as u64)
        .read_to_end(&mut contents)
        .block_on()?;
    if contents.contains(&0) {
        return Ok(DiffSideContent::Binary);
    }
    Ok(match read_capped_into(reader, contents, limit)? {
        Some(contents) => DiffSideContent::text(contents),
        None => DiffSideContent::TooLarge,
    })
}

/// The rest of `reader` appended to `contents`, reading one byte past
/// `limit` at most; `None` when the whole is larger than `limit`.
fn read_capped_into(
    mut reader: impl futures::AsyncRead + Unpin,
    mut contents: Vec<u8>,
    limit: usize,
) -> futures::io::Result<Option<Vec<u8>>> {
    (&mut reader)
        .take(limit.saturating_add(1).saturating_sub(contents.len()) as u64)
        .read_to_end(&mut contents)
        .block_on()?;
    Ok((contents.len() <= limit).then_some(contents))
}

/// A conflict between files, every term read through the size cap.
struct FileConflictTerms {
    /// Simplified file ids, in the terms' order; `None` where the file is
    /// absent on that term.
    ids: Merge<Option<FileId>>,
    labels: ConflictLabels,
    contents: Merge<bstr::BString>,
}

enum FileConflict {
    Terms(FileConflictTerms),
    /// Some term is larger than the limit; nothing was kept.
    TooLarge,
}

/// The conflicted file `value` names at `path`, or `None` when the
/// conflict is not between files (and absence). Each term is read on its
/// own through the cap, so one oversized term stops the read without the
/// others being loaded.
fn read_file_conflict(
    store: &Arc<Store>,
    path: &JjRepoPath,
    value: &MergedTreeValue,
    labels: &ConflictLabels,
    limit: usize,
) -> Result<Option<FileConflict>> {
    let Some(unsimplified) = value.to_file_merge() else {
        return Ok(None);
    };
    let (labels, ids) = labels.simplify_with(&unsimplified);
    let readers = ids.try_map(|id| {
        id.as_ref()
            .map(|id| store.read_file(path, id).block_on())
            .transpose()
    })?;
    let contents = read_terms(readers, limit).map_err(|e| {
        color_eyre::eyre::eyre!("failed to read `{}`: {e}", path.as_internal_file_string())
    })?;
    Ok(Some(match contents {
        Some(contents) => FileConflict::Terms(FileConflictTerms {
            ids,
            labels,
            contents,
        }),
        None => FileConflict::TooLarge,
    }))
}

/// Every term's content, an absent term reading as empty; `None` as soon
/// as a term is larger than `limit`. The limit is per term, not in sum.
fn read_terms<R: futures::AsyncRead + Unpin>(
    readers: Merge<Option<R>>,
    limit: usize,
) -> futures::io::Result<Option<Merge<bstr::BString>>> {
    let mut contents = Vec::with_capacity(readers.iter().count());
    for reader in readers.into_iter() {
        let content = match reader {
            Some(reader) => read_capped_into(reader, Vec::new(), limit)?,
            None => Some(Vec::new()),
        };
        match content {
            Some(bytes) => contents.push(bstr::BString::from(bytes)),
            None => return Ok(None),
        }
    }
    Ok(Some(Merge::from_vec(contents)))
}

/// The error for a file the limit keeps kojutsu from loading.
fn too_large_error(path: &JjRepoPath, limit: usize) -> color_eyre::Report {
    color_eyre::eyre::eyre!(
        "`{}` is larger than {} MiB (diff.max_file_size_mib)",
        path.as_internal_file_string(),
        limit / (1024 * 1024)
    )
}

/// Materialize a conflicted file the same way jj does (per-hunk markers),
/// while recording which 1-based output line ranges are conflict regions.
fn materialize_conflict_with_regions(contents: &Merge<bstr::BString>) -> DiffSideContent {
    let options = default_materialize_options();
    let mut out: Vec<u8> = Vec::new();
    let mut regions = Vec::new();
    let mut line: u32 = 1;
    let append = |bytes: &[u8], out: &mut Vec<u8>| -> u32 {
        let mut n = bytes.iter().filter(|b| **b == b'\n').count() as u32;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            n += 1;
        }
        out.extend_from_slice(bytes);
        n
    };
    match jj_lib::files::merge_hunks(contents, &options.merge) {
        jj_lib::files::MergeResult::Resolved(content) => {
            append(content.as_ref(), &mut out);
        }
        jj_lib::files::MergeResult::Conflict(merge_hunks) => {
            for hunk in merge_hunks {
                if let Some(resolved) = hunk.as_resolved() {
                    line += append(resolved.as_ref(), &mut out);
                } else {
                    let materialized = jj_lib::conflicts::materialize_merge_result_to_bytes(
                        &hunk,
                        &ConflictLabels::unlabeled(),
                        &options,
                    );
                    let n = append(materialized.as_ref(), &mut out);
                    regions.push(line..line + n);
                    line += n;
                }
            }
        }
    }
    if out.contains(&0) {
        return DiffSideContent::Binary;
    }
    DiffSideContent::Text {
        content: out.into(),
        conflict_regions: regions,
    }
}

/// Whether a tree value is a Git submodule pointer on any of its terms.
/// Submodules carry no file content, so their diffs are descriptions
/// rather than bytes (see `materialize_diff_side`) and can't be edited.
fn is_submodule(value: &MergedTreeValue) -> bool {
    value
        .iter()
        .flatten()
        .any(|v| matches!(v, TreeValue::GitSubmodule(_)))
}

/// Whether either side of a change is a Git submodule pointer: a submodule
/// being added or removed is as unpatchable as one being bumped.
fn is_submodule_change(values: &Diff<MergedTreeValue>) -> bool {
    is_submodule(&values.before) || is_submodule(&values.after)
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
        conflict_region: false,
    };
    DiffResult {
        git: vec![line.clone()],
        color_words: vec![line],
    }
}

fn too_large_placeholder(limit: usize) -> DiffResult {
    placeholder_diff(format!(
        "(file larger than {} MiB: diff skipped)",
        limit / (1024 * 1024)
    ))
}

/// The trees a `DiffTarget` compares, resolved against the repo.
struct TreePair {
    before: MergedTree,
    after: MergedTree,
}

impl JjRepo {
    /// Resolve a diff target to the pair of trees it compares.
    fn tree_pair(&self, target: &DiffTarget) -> Result<TreePair> {
        let repo = self.repo.as_ref();
        match target {
            DiffTarget::Commit(commit_id) => {
                let commit = self.load_commit(commit_id)?;
                let before = commit
                    .parent_tree(repo)
                    .block_on()
                    .wrap_err("failed to get parent tree")?;
                Ok(TreePair {
                    before,
                    after: commit.tree(),
                })
            }
            DiffTarget::Evolution {
                predecessors,
                commit,
            } => self.rebased_pair(predecessors, commit),
            DiffTarget::Interdiff { from, to } => self.rebased_pair(std::slice::from_ref(from), to),
        }
    }

    /// `sources` rebased onto `destination`'s parents, against `destination`:
    /// the comparison `jj evolog -p` and `jj interdiff` both make, so what
    /// the parents brought in doesn't show as a change.
    fn rebased_pair(&self, sources: &[UiCommitId], destination: &UiCommitId) -> Result<TreePair> {
        let sources = sources
            .iter()
            .map(|id| self.load_commit(id))
            .collect::<Result<Vec<_>>>()?;
        let destination = self.load_commit(destination)?;
        let before = rebase_to_dest_parent(self.repo.as_ref(), &sources, &destination)
            .block_on()
            .wrap_err("failed to rebase onto the destination's parents")?;
        Ok(TreePair {
            before,
            after: destination.tree(),
        })
    }

    /// Rename and copy records for a target. Only a commit's own changes
    /// carry them; like jj, the rebased comparisons list their files as
    /// plain adds and deletes.
    fn copy_records(&self, target: &DiffTarget) -> Result<jj_lib::copies::CopyRecords> {
        let mut copy_records = jj_lib::copies::CopyRecords::default();
        let DiffTarget::Commit(commit_id) = target else {
            return Ok(copy_records);
        };
        let commit = self.load_commit(commit_id)?;
        let store = self.repo.store();
        for parent_id in commit.parent_ids() {
            match store.get_copy_records(None, parent_id, commit.id()) {
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
        Ok(copy_records)
    }

    /// Compute the changed files and line totals for a diff target.
    pub fn diff_summary(&self, target: &DiffTarget) -> Result<DiffSummary> {
        let TreePair { before, after } = self.tree_pair(target)?;
        let copy_records = self.copy_records(target)?;
        let mut summary = self.tree_changes(&before, &after, &copy_records)?;
        // A commit lists every file it leaves conflicted, including ones its
        // parents already had conflicted and it didn't touch: they still
        // need resolving there, as `jj status` reports.
        if matches!(target, DiffTarget::Commit(_)) && after.has_conflict() {
            let existing: HashSet<RepoPath> =
                summary.files.iter().map(|c| c.path.clone()).collect();
            for (path, value) in after.conflicts() {
                let repo_path = RepoPath::new(path.as_internal_file_string());
                if !existing.contains(&repo_path) {
                    summary.files.push(FileChange {
                        path: repo_path,
                        old_path: None,
                        status: FileStatus::Modified,
                        has_conflict: true,
                        baseline_conflicted: true,
                        is_submodule: value.as_ref().is_ok_and(is_submodule),
                        stats: LineStats::default(),
                    });
                }
            }
        }
        Ok(summary)
    }

    /// List the files that differ between two trees, with per-file and total
    /// line stats.
    fn tree_changes(
        &self,
        before_tree: &MergedTree,
        after_tree: &MergedTree,
        copy_records: &jj_lib::copies::CopyRecords,
    ) -> Result<DiffSummary> {
        let store = self.repo.store();
        let labels = ConflictLabels::unlabeled();
        let mut files = Vec::new();
        let mut stats = LineStats::default();

        let mut diff_stream =
            before_tree.diff_stream_with_copies(after_tree, &EverythingMatcher, copy_records);
        while let Some(entry) = diff_stream.next().block_on() {
            let after_path = entry.path.target();
            let path = RepoPath::new(after_path.as_internal_file_string());
            let values = match entry.values {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("diff entry error for {path}: {e}");
                    files.push(FileChange {
                        path,
                        old_path: None,
                        status: FileStatus::Error,
                        has_conflict: false,
                        baseline_conflicted: false,
                        is_submodule: false,
                        stats: LineStats::default(),
                    });
                    continue;
                }
            };

            let source_path = entry.path.source.as_ref().map(|(p, _)| p);
            let status = match entry.path.copy_operation() {
                Some(jj_lib::copies::CopyOperation::Rename) => FileStatus::Renamed,
                Some(jj_lib::copies::CopyOperation::Copy) => FileStatus::Copied,
                None => match (values.before.is_present(), values.after.is_present()) {
                    (false, true) => FileStatus::Added,
                    (true, false) => FileStatus::Deleted,
                    (true, true) => FileStatus::Modified,
                    (false, false) => continue,
                },
            };
            let old_path = source_path.map(|p| RepoPath::new(p.as_internal_file_string()));
            let has_conflict = !values.after.is_resolved();
            let baseline_conflicted = !values.before.is_resolved();
            let is_submodule = is_submodule_change(&values);

            // Stats diff the renamed or copied file against its source.
            let before_path = source_path.map_or(after_path, |p| p.as_ref());
            let before = materialize_diff_side(
                store,
                before_path,
                values.before,
                &labels,
                self.diff_size_limit,
            )?;
            let after = materialize_diff_side(
                store,
                after_path,
                values.after,
                &labels,
                self.diff_size_limit,
            )?;
            let file_stats = match (before, after) {
                (
                    DiffSideContent::Text {
                        content: before, ..
                    },
                    DiffSideContent::Text { content: after, .. },
                ) => {
                    let contents = Diff::new(before.as_ref(), after.as_ref());
                    let hunks = unified::unified_diff_hunks(contents, 0, Default::default());
                    count_line_stats(&hunks)
                }
                _ => LineStats::default(),
            };
            stats.added = stats.added.saturating_add(file_stats.added);
            stats.removed = stats.removed.saturating_add(file_stats.removed);

            files.push(FileChange {
                path,
                old_path,
                status,
                has_conflict,
                baseline_conflicted,
                is_submodule,
                stats: file_stats,
            });
        }
        Ok(DiffSummary { files, stats })
    }

    /// Compute the line-level diff of one file in a diff target. For renamed
    /// or copied files, `old_path` is the source path in the before tree.
    pub fn file_diff(
        &self,
        target: &DiffTarget,
        path: &RepoPath,
        old_path: Option<&RepoPath>,
    ) -> Result<DiffResult> {
        let TreePair { before, after } = self.tree_pair(target)?;
        self.trees_file_diff(&before, &after, path, old_path)
    }

    /// Get the conflict hunks for a conflicted file, broken down by hunk.
    /// Returns a list of resolved (context) and conflicted hunks.
    pub fn conflict_hunks(
        &self,
        commit_id: &UiCommitId,
        path: &RepoPath,
    ) -> Result<Vec<crate::conflict::ConflictHunkKind>> {
        use crate::conflict::ConflictHunkKind;

        let repo = self.repo.as_ref();
        let tree = self.load_commit(commit_id)?.tree();
        let repo_path = RepoPathBuf::from_internal_string(path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let tree_value = tree.path_value(&repo_path).block_on()?;
        let labels = ConflictLabels::unlabeled();

        let terms = match read_file_conflict(
            repo.store(),
            &repo_path,
            &tree_value,
            &labels,
            self.diff_size_limit,
        )? {
            Some(FileConflict::Terms(terms)) => terms,
            Some(FileConflict::TooLarge) => {
                return Err(too_large_error(&repo_path, self.diff_size_limit));
            }
            None => return Ok(vec![]),
        };

        // `ids` parallels `contents` term-for-term; a `None` id means the
        // file is absent on that term (materialized as empty content).
        let absent: Vec<bool> = terms.ids.iter().map(|id| id.is_none()).collect();
        let merge_result = jj_lib::files::merge_hunks(&terms.contents, &merge_options());

        let hunks = match merge_result {
            jj_lib::files::MergeResult::Resolved(content) => {
                vec![ConflictHunkKind::Resolved {
                    text: ConflictText::from_bytes(&content),
                }]
            }
            jj_lib::files::MergeResult::Conflict(merge_hunks) => merge_hunks
                .into_iter()
                .map(|hunk| {
                    if let Some(resolved) = hunk.as_resolved() {
                        ConflictHunkKind::Resolved {
                            text: ConflictText::from_bytes(resolved.as_ref()),
                        }
                    } else {
                        ConflictHunkKind::Conflict {
                            terms: conflict_terms(&hunk, &absent),
                        }
                    }
                })
                .collect(),
        };

        Ok(hunks)
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

        let before_value = before_tree.path_value(&before_repo_path).block_on()?;
        let after_value = after_tree.path_value(&repo_path).block_on()?;

        let before = materialize_diff_side(
            repo.store(),
            &before_repo_path,
            before_value,
            &labels,
            self.diff_size_limit,
        )?;
        let after = materialize_diff_side(
            repo.store(),
            &repo_path,
            after_value,
            &labels,
            self.diff_size_limit,
        )?;

        let (before, before_regions, after, after_regions) = match (before, after) {
            (
                DiffSideContent::Text {
                    content: b,
                    conflict_regions: br,
                },
                DiffSideContent::Text {
                    content: a,
                    conflict_regions: ar,
                },
            ) => (b, br, a, ar),
            (DiffSideContent::TooLarge, _) | (_, DiffSideContent::TooLarge) => {
                return Ok(too_large_placeholder(self.diff_size_limit));
            }
            _ => return Ok(placeholder_diff("(binary file)")),
        };

        let (mut git_lines, mut cw_lines) = diff_lines(
            before.as_ref(),
            after.as_ref(),
            self.diff_context(crate::dag::DiffFormat::Git),
            self.diff_context(crate::dag::DiffFormat::ColorWords),
        );
        mark_conflict_regions(&mut git_lines, &before_regions, &after_regions);
        mark_conflict_regions(&mut cw_lines, &before_regions, &after_regions);
        Ok(DiffResult {
            git: git_lines,
            color_words: cw_lines,
        })
    }

    /// Get the raw content of a file at a specific commit.
    pub fn get_file_at_commit(
        &self,
        commit_id: &UiCommitId,
        file_path: &RepoPath,
    ) -> Result<Vec<u8>> {
        let repo = self.repo.as_ref();
        let tree = self.load_commit(commit_id)?.tree();
        let repo_path = RepoPathBuf::from_internal_string(file_path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;
        let value = tree.path_value(&repo_path).block_on()?;
        let labels = ConflictLabels::unlabeled();
        let limit = self.diff_size_limit;
        if !value.is_resolved() {
            return match read_file_conflict(repo.store(), &repo_path, &value, &labels, limit)? {
                Some(FileConflict::Terms(terms)) => {
                    let options = default_materialize_options();
                    let result = jj_lib::conflicts::materialize_merge_result_to_bytes(
                        &terms.contents,
                        &terms.labels,
                        &options,
                    );
                    Ok(result.into())
                }
                Some(FileConflict::TooLarge) => Err(too_large_error(&repo_path, limit)),
                None => Err(color_eyre::eyre::eyre!("path is not a regular file")),
            };
        }
        let materialized =
            materialize_tree_value(repo.store(), &repo_path, value, &labels).block_on()?;
        match materialized {
            MaterializedTreeValue::File(mut file) => {
                read_capped_into(&mut file.reader, Vec::new(), limit)?
                    .ok_or_else(|| too_large_error(&repo_path, limit))
            }
            _ => Err(color_eyre::eyre::eyre!("path is not a regular file")),
        }
    }

    /// Refuse a file the limit keeps kojutsu from loading, before work that
    /// would load it whole. jj-lib's annotate reads the file at every
    /// ancestor with no cap of its own, so this bounds only the file as it
    /// is at `commit_id`.
    pub(super) fn ensure_within_limit(
        &self,
        commit_id: &UiCommitId,
        file_path: &RepoPath,
    ) -> Result<()> {
        self.get_file_at_commit(commit_id, file_path).map(drop)
    }
}

/// Tag each term of a conflicted merge hunk with its kind. A `Merge`
/// interleaves positive and negative terms, starting positive: side 1,
/// base 1, side 2, base 2, and so on. This is the only place that may depend
/// on that ordering. `absent` flags file absence per term, in the same
/// interleaved order.
fn conflict_terms<T: AsRef<[u8]>>(hunk: &Merge<T>, absent: &[bool]) -> Vec<ConflictTerm> {
    // `absent` is built from `materialized.ids`, which jj keeps at the same
    // arity as `contents` (the source of these terms). A mismatch means
    // that invariant broke; catch it in tests/debug rather than silently
    // treating a missing flag as "present".
    debug_assert_eq!(
        hunk.iter().count(),
        absent.len(),
        "absent flags must parallel the merge terms"
    );
    let bases: Vec<&[u8]> = hunk.removes().map(|t| t.as_ref()).collect();
    hunk.iter()
        .enumerate()
        .map(|(i, term)| {
            let kind = if i % 2 == 0 {
                ConflictTermKind::Side(i / 2)
            } else {
                ConflictTermKind::Base(i / 2)
            };
            let is_absent = absent.get(i).copied().unwrap_or(false);
            // Highlight each side's changes against its base (jj pairs
            // side n+1 with base n; both sides of a 2-sided conflict pair
            // with the single base). Bases get no highlighting.
            let token_lines = match kind {
                ConflictTermKind::Side(n) if !is_absent => bases
                    .get(n.saturating_sub(1))
                    .map(|base| side_token_lines(base, term.as_ref()))
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            ConflictTerm {
                kind,
                absent: is_absent,
                text: ConflictText::from_bytes(term.as_ref()),
                token_lines,
            }
        })
        .collect()
}

/// Word-level tokens for each line of a conflict side, marking what the
/// side changed relative to its base. Returns one token list per side
/// line (removed-only lines belong to the base and are skipped).
/// Map a word-diff token tag to its display kind. A `Different` token is
/// Removed on a removed line and Added otherwise, so the line's type
/// disambiguates.
fn diff_token_kind(tag: &DiffTokenType, line_type: &DiffLineType) -> DiffTokenKind {
    match tag {
        DiffTokenType::Matching => DiffTokenKind::Unchanged,
        DiffTokenType::Different => match line_type {
            DiffLineType::Removed => DiffTokenKind::Removed,
            _ => DiffTokenKind::Added,
        },
    }
}

fn side_token_lines(base: &[u8], side: &[u8]) -> Vec<Vec<DiffToken>> {
    let contents = Diff::new(bstr::BStr::new(base), bstr::BStr::new(side));
    // Full context so every unchanged side line is present too.
    let hunks = unified::unified_diff_hunks(contents, u32::MAX as usize, Default::default());
    let mut lines = Vec::new();
    for hunk in &hunks {
        for (line_type, tokens) in &hunk.lines {
            if matches!(line_type, DiffLineType::Removed) {
                continue;
            }
            let mut diff_tokens: Vec<DiffToken> = tokens
                .iter()
                .filter(|(_, bytes)| !bytes.is_empty())
                .map(|(tag, bytes)| DiffToken {
                    text: String::from_utf8_lossy(bytes).into_owned(),
                    kind: diff_token_kind(tag, line_type),
                })
                .collect();
            if let Some(last) = diff_tokens.last_mut() {
                let text = last.text.strip_suffix('\n').unwrap_or(&last.text);
                last.text = text.strip_suffix('\r').unwrap_or(text).to_string();
            }
            lines.push(diff_tokens);
        }
    }
    lines
}

/// Line-level hunk splitting with same-change acceptance. Must be the same
/// options everywhere so the picker's hunk boundaries, the diff view's
/// conflict regions, and re-materialized markers all agree.
fn merge_options() -> jj_lib::tree_merge::MergeOptions {
    jj_lib::tree_merge::MergeOptions {
        hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
        same_change: jj_lib::merge::SameChange::Accept,
    }
}

fn default_materialize_options() -> ConflictMaterializeOptions {
    ConflictMaterializeOptions {
        marker_style: crate::conflict::MARKER_STYLE,
        marker_len: None,
        merge: merge_options(),
    }
}

/// Materialize a conflict hunk's terms as git-style markers (the same
/// text jj would produce). The `Merge` is rebuilt by term kind: sides as
/// adds, bases as removes, each ordered by ordinal, rather than trusting
/// the Vec's position, so markers are correct regardless of storage order.
pub fn hunk_markers(terms: &[crate::conflict::ConflictTerm]) -> bstr::BString {
    let term_bytes = |t: &crate::conflict::ConflictTerm| t.text.to_bytes();
    let mut sides: Vec<(usize, bstr::BString)> = Vec::new();
    let mut bases: Vec<(usize, bstr::BString)> = Vec::new();
    for t in terms {
        match t.kind {
            ConflictTermKind::Side(n) => sides.push((n, term_bytes(t))),
            ConflictTermKind::Base(n) => bases.push((n, term_bytes(t))),
        }
    }
    sides.sort_by_key(|(n, _)| *n);
    bases.sort_by_key(|(n, _)| *n);
    let merge = Merge::from_removes_adds(
        bases.into_iter().map(|(_, b)| b),
        sides.into_iter().map(|(_, s)| s),
    );
    jj_lib::conflicts::materialize_merge_result_to_bytes(
        &merge,
        &ConflictLabels::unlabeled(),
        &default_materialize_options(),
    )
}

/// Whether any line looks like a git-style conflict marker (7+ repeats of
/// a marker character at line start). Used to reject hand-edited hunk
/// resolutions that still contain markers.
pub fn has_conflict_markers(content: &[u8]) -> bool {
    content.lines().any(|l| {
        let Some((&first, rest)) = l.split_first() else {
            return false;
        };
        matches!(first, b'<' | b'>' | b'=' | b'|')
            && rest.iter().take_while(|&&b| b == first).count() >= 6
    })
}

/// Assemble file content from conflict hunks and the per-hunk picks
/// (keyed by hunk index). Resolved hunks and picked hunks contribute
/// their text; unpicked hunks are re-materialized as conflict markers,
/// which jj parses back into a conflicted state
/// (`merge-tool-edits-conflict-markers`). Returns the assembled content and
/// whether every hunk was resolved or picked.
pub fn assemble_resolution(
    hunks: &[crate::conflict::ConflictHunkKind],
    picks: &std::collections::HashMap<crate::idx::ConflictHunkIdx, crate::conflict::ConflictPick>,
) -> crate::conflict::Resolution {
    let mut content = bstr::BString::default();
    let mut complete = true;
    for (i, hunk) in hunks.iter().enumerate() {
        match hunk {
            crate::conflict::ConflictHunkKind::Resolved { text } => text.write_to(&mut content),
            crate::conflict::ConflictHunkKind::Conflict { terms } => match picks
                .get(&crate::idx::ConflictHunkIdx::new(i))
            {
                Some(crate::conflict::ConflictPick::Term(kind)) => {
                    match terms.iter().find(|t| t.kind == *kind) {
                        Some(term) => term.text.write_to(&mut content),
                        // The pick names no present term (shouldn't happen).
                        // Fall back to markers so the hunk stays conflicted
                        // rather than silently vanishing from the file.
                        None => {
                            complete = false;
                            content.extend_from_slice(&hunk_markers(terms));
                        }
                    }
                }
                Some(crate::conflict::ConflictPick::Edited(text)) => text.write_to(&mut content),
                None => {
                    complete = false;
                    content.extend_from_slice(&hunk_markers(terms));
                }
            },
        }
    }
    crate::conflict::Resolution { content, complete }
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

/// Whether a 1-based line number falls in any of the given ranges.
fn in_regions(regions: &[std::ops::Range<u32>], line: Option<u32>) -> bool {
    line.is_some_and(|l| regions.iter().any(|r| r.contains(&l)))
}

/// Mark diff lines lying in a conflict region on either side.
fn mark_conflict_regions(
    lines: &mut [DiffLine],
    before_regions: &[std::ops::Range<u32>],
    after_regions: &[std::ops::Range<u32>],
) {
    if before_regions.is_empty() && after_regions.is_empty() {
        return;
    }
    for l in lines {
        l.conflict_region = l.kind != DiffLineKind::Header
            && (in_regions(before_regions, l.old_line) || in_regions(after_regions, l.new_line));
    }
}

/// A file's diff lines in both formats, each with its own lines of context
/// around the changes. One diff serves both when the two agree.
fn diff_lines(
    before: &bstr::BStr,
    after: &bstr::BStr,
    git_context: usize,
    color_words_context: usize,
) -> (Vec<DiffLine>, Vec<DiffLine>) {
    let hunks_with = |context| {
        unified::unified_diff_hunks(Diff::new(before, after), context, Default::default())
    };
    let git_hunks = hunks_with(git_context);
    let cw_hunks_own;
    let cw_hunks = if color_words_context == git_context {
        &git_hunks
    } else {
        cw_hunks_own = hunks_with(color_words_context);
        &cw_hunks_own
    };
    let mut git_lines = Vec::new();
    hunks_to_diff_lines(&git_hunks, &mut git_lines);
    let mut cw_lines = Vec::new();
    hunks_to_color_words_lines(cw_hunks, &mut cw_lines);
    (git_lines, cw_lines)
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
            conflict_region: false,
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
                let kind = diff_token_kind(tag, line_type);
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
                conflict_region: false,
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
        let inline_diff = can_inline
            .then(|| {
                jj_lib::diff::ContentDiff::by_word([
                    removed_block.as_bytes(),
                    added_block.as_bytes(),
                ])
            })
            .filter(|d| count_alternations(d) <= MAX_ALTERNATION);

        if let Some(diff) = inline_diff {
            let mut lines = InlineLines::new(out, self.old_line, self.new_line);
            for h in diff.hunks() {
                match h.kind {
                    jj_lib::diff::DiffHunkKind::Matching => {
                        lines.push(h.contents[0], DiffTokenKind::Unchanged, true, true);
                    }
                    jj_lib::diff::DiffHunkKind::Different => {
                        lines.push(h.contents[0], DiffTokenKind::Removed, true, false);
                        lines.push(h.contents[1], DiffTokenKind::Added, false, true);
                    }
                }
            }
            lines.finish();
            self.old_line += self.removed_count;
            self.new_line += self.added_count;
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
                    conflict_region: false,
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
                    conflict_region: false,
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

/// The display lines of an inline colour-words block. Each old and each
/// new line is numbered once: on the first display line holding some of
/// its content or, when it has none, on the line where it ends.
struct InlineLines<'a> {
    out: &'a mut Vec<DiffLine>,
    content: String,
    tokens: Vec<DiffToken>,
    old_line: u32,
    new_line: u32,
    old_numbered: bool,
    new_numbered: bool,
}

impl<'a> InlineLines<'a> {
    fn new(out: &'a mut Vec<DiffLine>, old_line: u32, new_line: u32) -> Self {
        Self {
            out,
            content: String::new(),
            tokens: Vec::new(),
            old_line,
            new_line,
            old_numbered: false,
            new_numbered: false,
        }
    }

    /// Add a word-diff span of `kind`. Its newlines end the display line
    /// and, as `ends_old`/`ends_new` say, the old line, the new line, or
    /// both.
    fn push(&mut self, bytes: &[u8], kind: DiffTokenKind, ends_old: bool, ends_new: bool) {
        for (i, part) in String::from_utf8_lossy(bytes).split('\n').enumerate() {
            if i > 0 {
                self.end_line(ends_old, ends_new);
            }
            if !part.is_empty() {
                self.content.push_str(part);
                self.tokens.push(DiffToken {
                    text: part.to_string(),
                    kind,
                });
            }
        }
    }

    fn end_line(&mut self, ends_old: bool, ends_new: bool) {
        let number_old = !self.old_numbered
            && (ends_old || self.tokens.iter().any(|t| t.kind != DiffTokenKind::Added));
        let number_new = !self.new_numbered
            && (ends_new || self.tokens.iter().any(|t| t.kind != DiffTokenKind::Removed));
        self.out.push(DiffLine {
            kind: DiffLineKind::Context,
            content: std::mem::take(&mut self.content),
            tokens: std::mem::take(&mut self.tokens),
            old_line: number_old.then_some(self.old_line),
            new_line: number_new.then_some(self.new_line),
            conflict_region: false,
        });
        if ends_old {
            self.old_line += 1;
        }
        self.old_numbered = !ends_old && (self.old_numbered || number_old);
        if ends_new {
            self.new_line += 1;
        }
        self.new_numbered = !ends_new && (self.new_numbered || number_new);
    }

    /// End the block, where its last old line and last new line both end.
    /// Nothing is left to show when the last display line is empty and
    /// both lines were already numbered.
    fn finish(mut self) {
        if !self.tokens.is_empty() || !self.old_numbered || !self.new_numbered {
            self.end_line(true, true);
        }
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
            conflict_region: false,
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
                        conflict_region: false,
                    });
                    builder.old_line += 1;
                    builder.new_line += 1;
                }
            }
        }
        builder.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each term's kind, as a 0-based `side n` or `base n`, and its first
    /// line, in the order `conflict_terms` returns them.
    fn tagged(terms: &[ConflictTerm]) -> Vec<(String, String)> {
        terms
            .iter()
            .map(|t| {
                let kind = match t.kind {
                    ConflictTermKind::Side(n) => format!("side {n}"),
                    ConflictTermKind::Base(n) => format!("base {n}"),
                };
                (kind, t.text.display_line(0).unwrap())
            })
            .collect()
    }

    /// `Merge::from_removes_adds` is jj's canonical constructor: adds are
    /// the sides, removes are the bases. Pin that our tagging recovers
    /// them from the interleaved iteration order, for two sides and more.
    #[test]
    fn conflict_terms_tags_sides_and_bases_in_merge_order() {
        let two_way = Merge::from_removes_adds(vec!["base\n"], vec!["ours\n", "theirs\n"]);
        assert_eq!(
            tagged(&conflict_terms(&two_way, &[false; 3])),
            [
                ("side 0".to_string(), "ours".to_string()),
                ("base 0".into(), "base".into()),
                ("side 1".into(), "theirs".into()),
            ]
        );

        let three_way = Merge::from_removes_adds(
            vec!["base1\n", "base2\n"],
            vec!["side1\n", "side2\n", "side3\n"],
        );
        assert_eq!(
            tagged(&conflict_terms(&three_way, &[false; 5])),
            [
                ("side 0".to_string(), "side1".to_string()),
                ("base 0".into(), "base1".into()),
                ("side 1".into(), "side2".into()),
                ("base 1".into(), "base2".into()),
                ("side 2".into(), "side3".into()),
            ]
        );
    }

    /// A modify/delete conflict: the deleting side materializes as empty
    /// content, distinguished from a genuinely empty file only by the
    /// absent flag (interleaved order: side 1, base 1, side 2).
    #[test]
    fn conflict_terms_tags_absent_terms() {
        let hunk = Merge::from_removes_adds(vec!["base\n"], vec!["", "theirs\n"]);
        let terms = conflict_terms(&hunk, &[true, false, false]);
        for t in &terms {
            match t.kind {
                ConflictTermKind::Side(0) => {
                    assert!(t.absent);
                    assert_eq!(t.text.line_count(), 0);
                    assert!(t.token_lines.is_empty());
                }
                _ => assert!(!t.absent),
            }
        }
    }
    /// Partial assembly: picked hunks contribute their term text; unpicked
    /// hunks are re-materialized as git-style conflict markers that jj can
    /// parse back into a conflicted state.
    #[test]
    fn assemble_resolution_partial_keeps_markers() {
        use crate::conflict::{ConflictHunkKind, ConflictPick, ConflictText};
        use crate::idx::ConflictHunkIdx;
        use std::collections::HashMap;
        let conflict = || ConflictHunkKind::Conflict {
            terms: conflict_terms(
                &Merge::from_removes_adds(vec!["base\n"], vec!["ours\n", "theirs\n"]),
                &[false; 3],
            ),
        };
        let ctx = |s: &[u8]| ConflictHunkKind::Resolved {
            text: ConflictText::from_bytes(s),
        };

        // Hunk 1 picked (theirs), hunk 3 unpicked → keeps markers.
        let hunks = vec![ctx(b"ctx1\n"), conflict(), ctx(b"ctx2\n"), conflict()];
        let picks = HashMap::from([(
            ConflictHunkIdx::new(1),
            ConflictPick::Term(ConflictTermKind::Side(1)),
        )]);
        let res = assemble_resolution(&hunks, &picks);
        assert!(!res.complete);
        assert!(res.content.starts_with(b"ctx1\ntheirs\nctx2\n<<<<<<<"));
        assert!(res.content.contains_str("|||||||"));
        assert!(res.content.contains_str("======="));
        assert!(res.content.contains_str(">>>>>>>"));
        assert!(has_conflict_markers(&res.content));

        // Fully picked (ours).
        let picks = HashMap::from([(
            ConflictHunkIdx::new(0),
            ConflictPick::Term(ConflictTermKind::Side(0)),
        )]);
        let res = assemble_resolution(&[conflict()], &picks);
        assert!(res.complete);
        assert_eq!(res.content, "ours\n");
        assert!(!has_conflict_markers(&res.content));

        // An edited pick contributes its text verbatim.
        let picks = HashMap::from([(
            ConflictHunkIdx::new(0),
            ConflictPick::Edited(ConflictText::from_bytes(b"merged\n")),
        )]);
        let res = assemble_resolution(&[conflict()], &picks);
        assert!(res.complete);
        assert_eq!(res.content, "merged\n");

        // A pick naming no present term must not silently drop the hunk:
        // it falls back to markers and reports incomplete.
        let picks = HashMap::from([(
            ConflictHunkIdx::new(0),
            ConflictPick::Term(ConflictTermKind::Side(9)),
        )]);
        let res = assemble_resolution(&[conflict()], &picks);
        assert!(!res.complete);
        assert!(has_conflict_markers(&res.content));
    }

    /// Resolving keeps the file's bytes: CRLF endings and non-UTF-8 text in
    /// both the picked side and the untouched context come out as they
    /// went in.
    #[test]
    fn a_resolution_keeps_line_endings_and_encoding() {
        use crate::conflict::{ConflictHunkKind, ConflictPick, ConflictText};
        use crate::idx::ConflictHunkIdx;
        use std::collections::HashMap;
        let hunks = vec![
            ConflictHunkKind::Resolved {
                text: ConflictText::from_bytes(b"caf\xe9\r\n"),
            },
            ConflictHunkKind::Conflict {
                terms: conflict_terms(
                    &Merge::from_removes_adds(
                        vec![&b"base\r\n"[..]],
                        vec![&b"ours\r\n"[..], &b"theirs\r\n"[..]],
                    ),
                    &[false; 3],
                ),
            },
        ];
        let picks = HashMap::from([(
            ConflictHunkIdx::new(1),
            ConflictPick::Term(ConflictTermKind::Side(1)),
        )]);
        let res = assemble_resolution(&hunks, &picks);
        assert!(res.complete);
        assert_eq!(res.content, &b"caf\xe9\r\ntheirs\r\n"[..]);
    }

    /// `hunk_markers` rebuilds by term kind, so a reordered `terms` Vec
    /// still produces jj's canonical interleaved marker layout.
    #[test]
    fn hunk_markers_independent_of_term_order() {
        let terms = conflict_terms(
            &Merge::from_removes_adds(vec!["base\n"], vec!["ours\n", "theirs\n"]),
            &[false; 3],
        );
        let canonical = hunk_markers(&terms);
        let mut shuffled = terms;
        shuffled.reverse();
        assert_eq!(hunk_markers(&shuffled), canonical);
        // Sanity: the markers actually contain both sides in order.
        let ours = canonical.find("ours").unwrap();
        let theirs = canonical.find("theirs").unwrap();
        assert!(ours < theirs);
    }

    /// Sides get word-level tokens against their base (one token list per
    /// side line); bases get none.
    #[test]
    fn conflict_terms_compute_side_tokens() {
        let hunk = Merge::from_removes_adds(
            vec!["shared base\n"],
            vec!["shared ours\n", "shared theirs\nextra\n"],
        );
        let terms = conflict_terms(&hunk, &[false; 3]);
        for t in &terms {
            match t.kind {
                ConflictTermKind::Side(_) => {
                    assert_eq!(t.token_lines.len(), t.text.line_count());
                    let first = &t.token_lines[0];
                    assert!(first.iter().any(|tok| tok.kind == DiffTokenKind::Added));
                    assert!(
                        first.iter().any(|tok| tok.kind == DiffTokenKind::Unchanged
                            && tok.text.contains("shared"))
                    );
                }
                ConflictTermKind::Base(_) => assert!(t.token_lines.is_empty()),
            }
        }
    }

    /// Conflicted sides materialize with exact line ranges for the
    /// conflict regions (markers plus term content), so diff lines inside
    /// them can be tagged without parsing marker text back out.
    #[test]
    fn conflict_materialization_tracks_regions() {
        let contents = Merge::from_removes_adds(
            vec![bstr::BString::from("ctx\nbase\ntail\n")],
            vec![
                bstr::BString::from("ctx\nours\ntail\n"),
                bstr::BString::from("ctx\ntheirs\ntail\n"),
            ],
        );
        let DiffSideContent::Text {
            content,
            conflict_regions,
        } = materialize_conflict_with_regions(&contents)
        else {
            panic!("expected text");
        };
        let text = content.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(conflict_regions.len(), 1);
        let region = &conflict_regions[0];
        // 1-based, end-exclusive: the region covers exactly the marker
        // block; the shared context lines around it stay outside.
        assert_eq!(lines[0], "ctx");
        assert!(lines[(region.start - 1) as usize].starts_with("<<<<<<<"));
        assert!(lines[(region.end - 2) as usize].starts_with(">>>>>>>"));
        assert_eq!(*lines.last().unwrap(), "tail");
        assert_eq!(region.end as usize - 1, lines.len() - 1);

        // Fully resolvable merge: no regions.
        let resolved = Merge::from_removes_adds(
            vec![bstr::BString::from("a\n")],
            vec![bstr::BString::from("b\n"), bstr::BString::from("a\n")],
        );
        let DiffSideContent::Text {
            conflict_regions, ..
        } = materialize_conflict_with_regions(&resolved)
        else {
            panic!("expected text");
        };
        assert!(conflict_regions.is_empty());
    }

    /// Each format shows the context its own setting asks for: with one
    /// line, the neighbours of line 10; with five, lines 5 to 15.
    #[test]
    fn each_format_takes_its_own_context() {
        let before: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let after = before.replace("line 10\n", "line ten\n");
        let (before, after) = (bstr::BStr::new(&before), bstr::BStr::new(&after));
        let shows = |lines: &[DiffLine], n: usize| {
            let text = format!("line {n}");
            lines.iter().any(|l| l.content.trim_end() == text)
        };
        let (git, color_words) = super::diff_lines(before, after, 1, 5);
        assert!(shows(&git, 9) && shows(&git, 11));
        assert!(!shows(&git, 8) && !shows(&git, 12));
        assert!(shows(&color_words, 5) && shows(&color_words, 15));
        assert!(!shows(&color_words, 4) && !shows(&color_words, 16));
    }

    /// A colour-words diff (one line of context) as each line's shape:
    /// `-`/`+` for a split removed or added line, `~` for an inline line
    /// holding changed words, ` ` for plain context; then its old and new
    /// line numbers.
    fn color_words(before: &str, after: &str) -> Vec<(char, Option<u32>, Option<u32>)> {
        let (_, lines) = diff_lines(bstr::BStr::new(before), bstr::BStr::new(after), 1, 1);
        lines
            .into_iter()
            .filter(|l| l.kind != DiffLineKind::Header)
            .map(|l| {
                let changed = l.tokens.iter().any(|t| t.kind != DiffTokenKind::Unchanged);
                let shape = match l.kind {
                    DiffLineKind::Removed => '-',
                    DiffLineKind::Added => '+',
                    _ if changed => '~',
                    _ => ' ',
                };
                (shape, l.old_line, l.new_line)
            })
            .collect()
    }

    /// A line with a few words changed reads inline, numbered on both
    /// sides; past three switches between removed and added words it
    /// splits into the removed line and the added one.
    #[test]
    fn a_light_word_change_is_inline_and_a_heavy_one_splits() {
        let cases = [
            ("let x = 1;", "let x = 2;", true),
            ("a b c", "a b X c", true),
            ("a b c d", "x b d", true),
            ("a b c d e", "x b d y", false),
            ("one two three four five", "uno two tres four cinco", false),
        ];
        for (old, new, inline) in cases {
            let expected = if inline {
                vec![('~', Some(1), Some(1)), (' ', Some(2), Some(2))]
            } else {
                vec![
                    ('-', Some(1), None),
                    ('+', None, Some(1)),
                    (' ', Some(2), Some(2)),
                ]
            };
            assert_eq!(
                color_words(&format!("{old}\nend\n"), &format!("{new}\nend\n")),
                expected,
                "{old:?} -> {new:?}"
            );
        }
    }

    /// A whole line added or removed has no number on the side it is
    /// missing from, and the lines after it keep counting on both sides.
    #[test]
    fn a_whole_line_change_is_numbered_on_its_own_side() {
        assert_eq!(
            color_words("a\nc\n", "a\nb\nc\n"),
            [
                (' ', Some(1), Some(1)),
                ('+', None, Some(2)),
                (' ', Some(2), Some(3)),
            ]
        );
        assert_eq!(
            color_words("a\nb\nc\n", "a\nc\n"),
            [
                (' ', Some(1), Some(1)),
                ('-', Some(2), None),
                (' ', Some(3), Some(2)),
            ]
        );
    }

    /// Colour-words numbers every old and every new line of a hunk exactly
    /// once and in order, as the git format does, however the changed
    /// words fall across the lines of a block, and with the offsets that
    /// earlier hunks leave.
    #[test]
    fn color_words_numbers_each_line_once_like_the_git_format() {
        let filler: String = (1..=8).map(|n| format!("filler {n}\n")).collect();
        let cases = [
            ("a c\n", "a b c\n"),
            ("a b c\n", "a c\n"),
            ("a\n", "a x\nnew line\n"),
            ("old line\na\n", "a\n"),
            ("foo(a,\nb)\n", "foo(a, b)\n"),
            ("x\n\n", "y\n"),
            ("x", "x\n\n"),
            ("one\ntwo\n", "one\n\ntwo\n"),
            ("k = 1\n", "k = 2\n\nmore\n"),
        ];
        for (old, new) in cases {
            for (before, after) in [
                (old.to_string(), new.to_string()),
                (
                    format!("first\n{filler}{old}{filler}{old}"),
                    format!("first\ninserted\n{filler}{new}{filler}{new}"),
                ),
            ] {
                let (git, cw) = diff_lines(bstr::BStr::new(&before), bstr::BStr::new(&after), 1, 1);
                let numbers = |lines: &[DiffLine], side: fn(&DiffLine) -> Option<u32>| {
                    lines.iter().filter_map(side).collect::<Vec<_>>()
                };
                assert_eq!(
                    numbers(&cw, |l| l.old_line),
                    numbers(&git, |l| l.old_line),
                    "old side of {before:?} -> {after:?}"
                );
                assert_eq!(
                    numbers(&cw, |l| l.new_line),
                    numbers(&git, |l| l.new_line),
                    "new side of {before:?} -> {after:?}"
                );
            }
        }
    }

    /// Markers are 7 or more of one marker character opening a line; a
    /// shorter run, a run further in, or a mix of characters is content.
    #[test]
    fn conflict_markers_are_seven_marker_characters_at_line_start() {
        let cases: [(&[u8], bool); 14] = [
            (b"<<<<<<< side 1\n", true),
            (b"|||||||\n", true),
            (b"=======", true),
            (b">>>>>>>>>>> side 2\n", true),
            (b"text\n=======\nmore\n", true),
            (b"text\r\n>>>>>>>\r\nmore\r\n", true),
            (b"caf\xe9\n<<<<<<<\n", true),
            (b"======\n", false),
            (b" <<<<<<<\n", false),
            (b"text =======\n", false),
            (b"<<< quoted\n", false),
            (b"<<<<<<>\n", false),
            (b"+++++++\n", false),
            (b"", false),
        ];
        for (content, markers) in cases {
            assert_eq!(
                has_conflict_markers(content),
                markers,
                "{:?}",
                bstr::BStr::new(content)
            );
        }
    }

    /// A file is binary when a null byte falls within the first
    /// A conflict's terms are read one by one through the cap: an absent
    /// term reads as empty, every term may be as large as the limit, and
    /// one byte more in any term gives up on the whole file.
    #[test]
    fn conflict_terms_are_capped_one_by_one() {
        let term = |bytes: &[u8]| Some(futures::io::Cursor::new(bytes.to_vec()));
        let limit = 4;
        let fits = Merge::from_vec(vec![term(b"abcd"), None, term(b"wxyz")]);
        let contents = read_terms(fits, limit).unwrap().expect("within the limit");
        let texts: Vec<&[u8]> = contents.iter().map(|c| c.as_ref()).collect();
        assert_eq!(texts, [&b"abcd"[..], b"", b"wxyz"]);

        let one_over = Merge::from_vec(vec![term(b"abcd"), term(b""), term(b"wxyz!")]);
        assert!(read_terms(one_over, limit).unwrap().is_none());
    }

    /// `BINARY_SNIFF_SIZE` bytes, however large it is, and too large when
    /// it holds even one byte past the limit.
    #[test]
    fn a_diff_file_is_sniffed_for_binary_then_capped_at_the_limit() {
        let with_null_at = |len: usize, at: usize| {
            let mut bytes = vec![b'a'; len];
            bytes[at] = 0;
            bytes
        };
        let limit = 2 * BINARY_SNIFF_SIZE;
        let cases = [
            (vec![b'a'; limit], limit, "text"),
            (vec![b'a'; limit + 1], limit, "too large"),
            (Vec::new(), 0, "text"),
            (vec![b'a'], 0, "too large"),
            (with_null_at(limit, BINARY_SNIFF_SIZE - 1), limit, "binary"),
            (with_null_at(limit, BINARY_SNIFF_SIZE), limit, "text"),
            (
                with_null_at(limit + 1, BINARY_SNIFF_SIZE),
                limit,
                "too large",
            ),
            (with_null_at(BINARY_SNIFF_SIZE, 10), 5, "binary"),
        ];
        for (bytes, limit, expected) in cases {
            let read = match read_diff_file(futures::io::Cursor::new(&bytes), limit).unwrap() {
                DiffSideContent::Text { content, .. } => {
                    assert_eq!(content, bytes);
                    "text"
                }
                DiffSideContent::Binary => "binary",
                DiffSideContent::TooLarge => "too large",
            };
            assert_eq!(read, expected, "{} bytes, limit {limit}", bytes.len());
        }
    }
}
