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
fn selection_map(selections: &HashSet<Selection>) -> HashMap<&str, FileSelection> {
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
                    fs::read_to_string(&left_path)?
                } else {
                    String::new() // new file -- left doesn't exist
                };
                let right_content = fs::read_to_string(entry.path())?;
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

    // Handle files that exist in left but not in right (deleted files).
    // If a deleted file's removal wasn't selected, restore it.
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
        if !right_path.exists() {
            let rel_str = rel_path.to_string_lossy();
            match file_map.get(rel_str.as_ref()) {
                Some(FileSelection::Full) => {
                    // Deletion was selected -- keep it deleted.
                }
                _ => {
                    // Deletion not selected or only partial lines selected
                    // (partial deletion of a deleted file doesn't make sense,
                    // so treat it as "keep the file").
                    if let Some(parent) = right_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(entry.path(), &right_path)?;
                }
            }
        }
    }

    Ok(())
}

/// Apply partial line-level selection to reconstruct a file.
///
/// Starts from `left` (original), applies only the selected additions and
/// removals from the diff between `left` and `right`.
fn apply_partial(left: &str, right: &str, selected: &[SelectedLine]) -> String {
    // Simple line-by-line diff approach:
    // We diff left vs right, then for each diff operation:
    // - Context line: always include
    // - Added line: include only if selected
    // - Removed line: keep (don't remove) unless selected
    //
    // We use jj-lib's diff engine for consistency with what the TUI shows.
    use jj_lib::diff_presentation::unified::{self, DiffLineType};
    use jj_lib::merge::Diff;

    let diff = Diff::new(left.as_bytes().into(), right.as_bytes().into());
    let hunks = unified::unified_diff_hunks(diff, 0, Default::default());

    let left_lines: Vec<&str> = left.split_inclusive('\n').collect();
    let mut output = String::new();
    let mut left_cursor: usize = 0; // current position in left_lines (0-indexed)

    for hunk in &hunks {
        // Copy unchanged lines before this hunk from left.
        let hunk_start = hunk.left_line_range.start;
        for i in left_cursor..hunk_start {
            if let Some(line) = left_lines.get(i) {
                output.push_str(line);
            }
        }

        // Track line numbers through the hunk.
        let mut old_line = hunk.left_line_range.start as u32 + 1; // 1-indexed
        let mut new_line = hunk.right_line_range.start as u32 + 1;

        for (line_type, tokens) in &hunk.lines {
            let text: String = tokens
                .iter()
                .map(|(_, bytes)| String::from_utf8_lossy(bytes))
                .collect();

            match line_type {
                DiffLineType::Context => {
                    output.push_str(&text);
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineType::Added => {
                    if selected.contains(&SelectedLine::Added { new_line }) {
                        output.push_str(&text);
                    }
                    // If not selected, omit the added line.
                    new_line += 1;
                }
                DiffLineType::Removed => {
                    if !selected.contains(&SelectedLine::Removed { old_line }) {
                        // Not selected for removal -- keep the line.
                        output.push_str(&text);
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
            output.push_str(line);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::{FileSelection, SelectedLine, apply_partial, selection_map};
    use crate::types::{CommitId, FileRef, RepoPath, Selection};

    fn file_ref(path: &str) -> FileRef {
        FileRef {
            commit_id: CommitId::new("7bbaa2cb"),
            path: RepoPath::new(path),
        }
    }

    /// What the TUI writes is what the diff tool reads back.
    #[test]
    fn the_selection_survives_the_trip_through_json() {
        let selections = HashSet::from([
            Selection::File(file_ref("whole")),
            Selection::Line {
                file_ref: file_ref("part"),
                old_line: Some(3),
                new_line: None,
            },
        ]);
        let json = serde_json::to_string(&selection_map(&selections)).unwrap();
        let read: HashMap<String, FileSelection> = serde_json::from_str(&json).unwrap();

        assert_eq!(read["whole"], FileSelection::Full);
        assert_eq!(
            read["part"],
            FileSelection::Lines {
                selected: vec![SelectedLine::Removed { old_line: 3 }]
            }
        );
    }

    #[test]
    fn only_the_selected_lines_are_applied() {
        let left = "a\nb\nc\n";
        let right = "a\nB\nc\nd\n";
        let selected = [
            SelectedLine::Removed { old_line: 2 },
            SelectedLine::Added { new_line: 4 },
        ];
        // `b` goes and `d` arrives; `B` wasn't picked, so it stays out.
        assert_eq!(apply_partial(left, right, &selected), "a\nc\nd\n");
    }
}
