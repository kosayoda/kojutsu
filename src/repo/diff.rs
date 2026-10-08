use std::collections::HashSet;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use jj_lib::backend::TreeValue;
use jj_lib::backend::{MergedTreeValue, MergedTreeValueExt as _};
use jj_lib::conflict_labels::ConflictLabels;
use jj_lib::conflicts::{
    ConflictMaterializeOptions, MaterializedTreeValue, materialize_tree_value,
    try_materialize_file_conflict_value,
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
    let materialized = materialize_tree_value(store, path, value, labels).block_on()?;
    match materialized {
        MaterializedTreeValue::File(mut file) => {
            let read_err = |e: futures::io::Error| {
                color_eyre::eyre::eyre!("failed to read `{}`: {e}", path.as_internal_file_string())
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
                .take(limit.saturating_add(1).saturating_sub(contents.len()) as u64)
                .read_to_end(&mut contents)
                .block_on()
                .map_err(read_err)?;
            if contents.len() > limit {
                return Ok(DiffSideContent::TooLarge);
            }
            Ok(DiffSideContent::text(contents))
        }
        MaterializedTreeValue::FileConflict(conflict) => {
            Ok(materialize_conflict_with_regions(&conflict.contents))
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

        let Some(materialized) =
            try_materialize_file_conflict_value(repo.store(), &repo_path, &tree_value, &labels)
                .block_on()
                .wrap_err("failed to materialize conflict")?
        else {
            return Ok(vec![]);
        };

        // `ids` parallels `contents` term-for-term; a `None` id means the
        // file is absent on that term (materialized as empty content).
        let absent: Vec<bool> = materialized.ids.iter().map(|id| id.is_none()).collect();
        let merge_result = jj_lib::files::merge_hunks(&materialized.contents, &merge_options());

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
            _ => Err(color_eyre::eyre::eyre!("path is not a regular file")),
        }
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
                last.text = last.text.trim_end_matches('\n').to_string();
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
pub fn hunk_markers(terms: &[crate::conflict::ConflictTerm]) -> String {
    let term_bytes = |t: &crate::conflict::ConflictTerm| bstr::BString::from(t.text.to_content());
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
    let materialized = jj_lib::conflicts::materialize_merge_result_to_bytes(
        &merge,
        &ConflictLabels::unlabeled(),
        &default_materialize_options(),
    );
    String::from_utf8_lossy(&materialized).into_owned()
}

/// Whether any line looks like a git-style conflict marker (7+ repeats of
/// a marker character at line start). Used to reject hand-edited hunk
/// resolutions that still contain markers.
pub fn has_conflict_markers(content: &str) -> bool {
    content.lines().any(|l| {
        let mut chars = l.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        matches!(first, '<' | '>' | '=' | '|') && chars.take_while(|c| *c == first).count() >= 6
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
    let mut content = String::new();
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
                            content.push_str(&hunk_markers(terms));
                        }
                    }
                }
                Some(crate::conflict::ConflictPick::Edited(text)) => text.write_to(&mut content),
                None => {
                    complete = false;
                    content.push_str(&hunk_markers(terms));
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
                                    conflict_region: false,
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
                                    conflict_region: false,
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
                                    conflict_region: false,
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
                    conflict_region: false,
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

    /// `Merge::from_removes_adds` is jj's canonical constructor: adds are
    /// the sides, removes are the bases. Pin that our tagging recovers
    /// them correctly from the interleaved iteration order.
    #[test]
    fn conflict_terms_tags_sides_and_bases() {
        let hunk = Merge::from_removes_adds(vec!["base\n"], vec!["ours\n", "theirs\n"]);
        let terms = conflict_terms(&hunk, &[false; 3]);
        let tagged: Vec<(ConflictTermKind, &str)> = terms
            .iter()
            .map(|t| (t.kind, t.text.lines[0].as_str()))
            .collect();
        assert!(tagged.contains(&(ConflictTermKind::Side(0), "ours")));
        assert!(tagged.contains(&(ConflictTermKind::Side(1), "theirs")));
        assert!(tagged.contains(&(ConflictTermKind::Base(0), "base")));
    }

    #[test]
    fn conflict_terms_tags_n_way_merges() {
        let hunk = Merge::from_removes_adds(
            vec!["base1\n", "base2\n"],
            vec!["side1\n", "side2\n", "side3\n"],
        );
        let terms = conflict_terms(&hunk, &[false; 5]);
        assert_eq!(terms.len(), 5);
        let sides: Vec<&str> = terms
            .iter()
            .filter(|t| matches!(t.kind, ConflictTermKind::Side(_)))
            .map(|t| t.text.lines[0].as_str())
            .collect();
        let bases: Vec<&str> = terms
            .iter()
            .filter(|t| matches!(t.kind, ConflictTermKind::Base(_)))
            .map(|t| t.text.lines[0].as_str())
            .collect();
        assert_eq!(sides, ["side1", "side2", "side3"]);
        assert_eq!(bases, ["base1", "base2"]);
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
                    assert!(t.text.lines.is_empty());
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
        assert!(res.content.starts_with("ctx1\ntheirs\nctx2\n<<<<<<<"));
        assert!(res.content.contains("|||||||"));
        assert!(res.content.contains("======="));
        assert!(res.content.contains(">>>>>>>"));
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
                    assert_eq!(t.token_lines.len(), t.text.lines.len());
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
}
