//! Diff tool mode: applies partial line-level selections to jj's diff directories.
//!
//! The TUI writes the selection with [`write_selection`] and hands jj a diff
//! editor command that runs this binary again. When invoked as `kojutsu --apply-diff <selection.json> <left_dir> <right_dir>`,
//! this module reads the selection file and modifies `right_dir` in place so that
//! only selected changes remain (unselected changes are reverted to the `left_dir`
//! version).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::Context;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::types::{FileRef, Selection};

/// What to keep of one file's changes.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum FileSelection {
    /// All of them.
    Full,
    /// Only these lines.
    Lines { selected: Vec<SelectedLine> },
}

/// A selected diff line, by its number on the side it lives on.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum SelectedLine {
    Removed { old_line: u32 },
    Added { new_line: u32 },
}

/// The file and line selections, keyed by path, in the form `apply` reads.
fn selection_map<'a>(
    selections: impl IntoIterator<Item = &'a Selection>,
) -> HashMap<&'a str, FileSelection> {
    let mut file_map: HashMap<&str, FileSelection> = HashMap::new();
    for sel in selections {
        match sel {
            Selection::Commit(_) => {}
            Selection::File(FileRef { path, .. }) => {
                file_map.insert(path.as_str(), FileSelection::Full);
            }
            Selection::Line {
                file_ref: FileRef { path, .. },
                old_line,
                new_line,
            } => {
                let line = match (*old_line, *new_line) {
                    (Some(old_line), None) => SelectedLine::Removed { old_line },
                    (None, Some(new_line)) => SelectedLine::Added { new_line },
                    _ => continue,
                };
                match file_map
                    .entry(path.as_str())
                    .or_insert_with(|| FileSelection::Lines { selected: vec![] })
                {
                    // A whole-file selection already covers the line.
                    FileSelection::Full => {}
                    FileSelection::Lines { selected } => selected.push(line),
                }
            }
        }
    }
    file_map
}

/// Write the selections to a temporary JSON file for the diff tool and
/// return its path. The diff tool removes the file once it has read it.
pub fn write_selection(selections: &HashSet<Selection>) -> Result<PathBuf> {
    let mut tmp = tempfile::NamedTempFile::new()?;
    serde_json::to_writer_pretty(&mut tmp, &selection_map(selections))?;
    tmp.flush()?;
    let (_, path) = tmp.keep()?;
    Ok(path)
}

/// Apply the selection to the diff directories.
///
/// `left_dir` is the "before" state, `right_dir` is the "after" state.
/// We modify `right_dir` in place: unselected changes are reverted to `left_dir`.
pub fn apply(selection_path: &Path, left_dir: &Path, right_dir: &Path) -> Result<()> {
    let json = fs::read_to_string(selection_path).wrap_err_with(|| {
        format!(
            "failed to read selection file: {}",
            selection_path.display()
        )
    })?;
    let file_map: HashMap<String, FileSelection> =
        serde_json::from_str(&json).wrap_err("failed to parse selection JSON")?;

    // Process files in right_dir (the "after" state).
    for entry in WalkDir::new(right_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(right_dir)
            .wrap_err("failed to strip right_dir prefix")?;
        let rel_str = rel_path.to_string_lossy();

        match file_map.get(rel_str.as_ref()) {
            Some(FileSelection::Full) => {
                // Keep right as-is (all changes included).
            }
            Some(FileSelection::Lines { selected }) => {
                // Partial: reconstruct file with only selected changes.
                let left_path = left_dir.join(rel_path);
                let left_content = if left_path.exists() {
                    fs::read(&left_path)?
                } else {
                    Vec::new() // new file -- left doesn't exist
                };
                let right_content = fs::read(entry.path())?;
                let output = apply_partial(&left_content, &right_content, selected);
                fs::write(entry.path(), output)?;
            }
            None => {
                // Not selected: revert to left (no changes).
                let left_path = left_dir.join(rel_path);
                if left_path.exists() {
                    fs::copy(&left_path, entry.path())?;
                } else {
                    // File was added but not selected -- delete from right.
                    fs::remove_file(entry.path())?;
                }
            }
        }
    }

    // Files in left_dir but not right_dir (deletions).
    for entry in WalkDir::new(left_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(left_dir)
            .wrap_err("failed to strip left_dir prefix")?;
        let right_path = right_dir.join(rel_path);
        if right_path.exists() {
            continue;
        }
        match file_map.get(rel_path.to_string_lossy().as_ref()) {
            Some(FileSelection::Full) => {
                // Deletion was selected -- keep it deleted.
            }
            Some(FileSelection::Lines { selected }) => {
                // The file comes back without its selected lines; with every
                // line selected, nothing of it is left and it stays deleted.
                let kept = apply_partial(&fs::read(entry.path())?, &[], selected);
                if !kept.is_empty() {
                    restore(entry.path(), &right_path)?;
                    fs::write(&right_path, kept)?;
                }
            }
            None => restore(entry.path(), &right_path)?,
        }
    }

    Ok(())
}

/// Put `from` at `to`, creating the directory it goes in.
fn restore(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)?;
    Ok(())
}

/// Apply partial line-level selection to reconstruct a file.
///
/// Starts from `left` (original), applies only the selected additions and
/// removals from the diff between `left` and `right`.
fn apply_partial(left: &[u8], right: &[u8], selected: &[SelectedLine]) -> Vec<u8> {
    // Simple line-by-line diff approach:
    // We diff left vs right, then for each diff operation:
    // - Context line: always include
    // - Added line: include only if selected
    // - Removed line: keep (don't remove) unless selected
    //
    // We use jj-lib's diff engine for consistency with what the TUI shows.
    use jj_lib::diff_presentation::unified::{self, DiffLineType};
    use jj_lib::merge::Diff;

    let diff = Diff::new(left.into(), right.into());
    let hunks = unified::unified_diff_hunks(diff, 0, Default::default());

    let left_lines: Vec<&[u8]> = left.split_inclusive(|&b| b == b'\n').collect();
    let mut output = Vec::new();
    let mut left_cursor: usize = 0; // current position in left_lines (0-indexed)

    for hunk in &hunks {
        // Copy unchanged lines before this hunk from left.
        let hunk_start = hunk.left_line_range.start;
        for i in left_cursor..hunk_start {
            if let Some(line) = left_lines.get(i) {
                output.extend_from_slice(line);
            }
        }

        // Track line numbers through the hunk.
        let mut old_line = hunk.left_line_range.start as u32 + 1; // 1-indexed
        let mut new_line = hunk.right_line_range.start as u32 + 1;

        for (line_type, tokens) in &hunk.lines {
            let text: Vec<u8> = tokens
                .iter()
                .flat_map(|(_, bytes)| bytes.iter().copied())
                .collect();

            match line_type {
                DiffLineType::Context => {
                    output.extend_from_slice(&text);
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineType::Added => {
                    if selected.contains(&SelectedLine::Added { new_line }) {
                        output.extend_from_slice(&text);
                    }
                    // If not selected, omit the added line.
                    new_line += 1;
                }
                DiffLineType::Removed => {
                    if !selected.contains(&SelectedLine::Removed { old_line }) {
                        // Not selected for removal -- keep the line.
                        output.extend_from_slice(&text);
                    }
                    // If selected, the line is removed (omitted).
                    old_line += 1;
                }
            }
        }

        left_cursor = hunk.left_line_range.end;
    }

    // Copy remaining unchanged lines after the last hunk.
    for i in left_cursor..left_lines.len() {
        if let Some(line) = left_lines.get(i) {
            output.extend_from_slice(line);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{FileSelection, SelectedLine, apply, apply_partial, selection_map};
    use crate::types::{ChangeId, CommitId, CommitRef, FileRef, RepoPath, Selection};

    fn file_ref(path: &str) -> FileRef {
        FileRef {
            commit_id: CommitId::new("7bbaa2cb"),
            path: RepoPath::new(path),
        }
    }

    fn line(path: &str, old_line: Option<u32>, new_line: Option<u32>) -> Selection {
        Selection::Line {
            file_ref: file_ref(path),
            old_line,
            new_line,
        }
    }

    /// A whole-file selection covers any line picked in the same file, in
    /// whichever order the two are met.
    #[test]
    fn a_file_selection_takes_in_its_own_lines() {
        let file = Selection::File(file_ref("f"));
        let removed = line("f", Some(3), None);
        for order in [[&file, &removed], [&removed, &file]] {
            assert_eq!(
                selection_map(order),
                HashMap::from([("f", FileSelection::Full)])
            );
        }
    }

    /// Only a line on one side is a change to keep: a context line, numbered
    /// on both, is no change. A commit selection says nothing about lines.
    #[test]
    fn only_changed_lines_are_kept_and_commits_are_ignored() {
        let commit = Selection::Commit(CommitRef {
            commit_id: CommitId::new("7bbaa2cb"),
            change_id: ChangeId::new("qpvuntsm"),
        });
        let selections = [
            commit,
            line("f", Some(2), None),
            line("f", None, Some(4)),
            line("f", Some(5), Some(6)),
        ];
        assert_eq!(
            selection_map(&selections),
            HashMap::from([(
                "f",
                FileSelection::Lines {
                    selected: vec![
                        SelectedLine::Removed { old_line: 2 },
                        SelectedLine::Added { new_line: 4 },
                    ]
                }
            )])
        );
    }

    #[test]
    fn only_the_selected_lines_are_applied() {
        let left = b"a\nb\nc\n";
        let right = b"a\nB\nc\nd\n";
        let selected = [
            SelectedLine::Removed { old_line: 2 },
            SelectedLine::Added { new_line: 4 },
        ];
        // `b` goes and `d` arrives; `B` wasn't picked, so it stays out.
        assert_eq!(apply_partial(left, right, &selected), b"a\nc\nd\n");
    }

    /// What is at a path in a diff directory.
    #[derive(Debug, PartialEq)]
    enum Entry {
        File(Vec<u8>),
    }

    fn file(content: &[u8]) -> Option<Entry> {
        Some(Entry::File(content.to_vec()))
    }

    /// A path and what is at it before and after the change.
    type Sides = (&'static str, Option<Entry>, Option<Entry>);

    /// Run `apply` over diff directories holding `files`, and read back
    /// what each path holds on the right.
    fn apply_to(files: &[Sides], selection: &HashMap<&str, FileSelection>) -> Vec<Option<Entry>> {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = (dir.path().join("left"), dir.path().join("right"));
        for (path, before, after) in files {
            for (root, entry) in [(&left, before), (&right, after)] {
                let at = root.join(path);
                std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                if let Some(Entry::File(content)) = entry {
                    std::fs::write(at, content).unwrap();
                }
            }
        }
        let selection_path = dir.path().join("selection.json");
        std::fs::write(&selection_path, serde_json::to_vec(selection).unwrap()).unwrap();
        apply(&selection_path, &left, &right).unwrap();
        files
            .iter()
            .map(|(path, _, _)| std::fs::read(right.join(path)).ok().map(Entry::File))
            .collect()
    }

    fn lines(selected: impl IntoIterator<Item = SelectedLine>) -> Option<FileSelection> {
        Some(FileSelection::Lines {
            selected: selected.into_iter().collect(),
        })
    }

    /// Run each case alone and check what its path holds afterwards.
    fn check(cases: impl IntoIterator<Item = (Sides, Option<FileSelection>, Option<Entry>)>) {
        for (sides, selection, expected) in cases {
            let name = format!("{} with {selection:?}", sides.0);
            let selection = selection
                .map(|s| HashMap::from([(sides.0, s)]))
                .unwrap_or_default();
            assert_eq!(apply_to(&[sides], &selection), [expected], "{name}");
        }
    }

    /// A new file holds only its selected lines, and a modified one only
    /// its selected changes, with its bytes, line endings and encoding
    /// included, passing through untouched.
    #[test]
    fn selected_lines_apply_byte_for_byte() {
        check([
            (
                ("f", None, file(b"a\nb\n")),
                lines([SelectedLine::Added { new_line: 2 }]),
                file(b"b\n"),
            ),
            (
                (
                    "f",
                    file(b"caf\xe9\r\nb\r\n"),
                    file(b"caf\xe9\r\nB\r\nna\xefve\r\n"),
                ),
                lines([SelectedLine::Added { new_line: 3 }]),
                file(b"caf\xe9\r\nb\r\nna\xefve\r\n"),
            ),
        ]);
    }

    /// A deleted file with some of its lines selected comes back without
    /// them; with every line selected it stays deleted.
    #[test]
    fn a_deleted_file_loses_only_its_selected_lines() {
        let removed = |old_lines: &[u32]| {
            lines(
                old_lines
                    .iter()
                    .map(|&old_line| SelectedLine::Removed { old_line }),
            )
        };
        check([
            (
                ("f", file(b"a\nb\nc\n"), None),
                removed(&[2]),
                file(b"a\nc\n"),
            ),
            (("f", file(b"a\nb\nc\n"), None), removed(&[1, 2, 3]), None),
        ]);
    }
}
