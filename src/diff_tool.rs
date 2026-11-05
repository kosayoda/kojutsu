//! Diff tool mode: applies partial line-level selections to jj's diff directories.
//!
//! The TUI writes the selection with [`write_selection`] and hands jj a diff
//! editor command that runs this binary again. When invoked as `kojutsu --apply-diff <selection.json> <left_dir> <right_dir>`,
//! this module reads the selection file and modifies `right_dir` in place so that
//! only selected changes remain (unselected changes are reverted to the `left_dir`
//! version).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::Context;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::types::{FileRef, Selection, StagedFile};

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

/// Write the selections to a staged JSON file for the diff tool. The
/// diff tool removes the file once it has read it; the command that
/// names it removes it otherwise.
pub fn write_selection(selections: &HashSet<Selection>) -> Result<StagedFile> {
    let json = serde_json::to_vec_pretty(&selection_map(selections))?;
    Ok(StagedFile::write(&json)?)
}

/// Merge tool mode: `content`, staged by the TUI, becomes the resolved
/// file at `output`. `content` is removed whether or not that works, as
/// it is read once.
pub fn apply_resolution(content: &Path, output: &Path) -> std::io::Result<()> {
    let copied = fs::copy(content, output);
    let _ = fs::remove_file(content);
    copied.map(|_| ())
}

/// Apply the selection to the diff directories.
///
/// `left_dir` is the "before" state, `right_dir` is the "after" state.
/// We modify `right_dir` in place: unselected changes are reverted to `left_dir`.
///
/// Both trees come from the repository, links included, so a path is only
/// used under a tree when every directory above it is a real directory
/// (see [`Tree::under`]). A link among the ancestors on the left means the
/// left tree has no such file; on the right it means the user kept a link
/// where an unselected change needs a directory, which cannot both hold,
/// so the whole apply fails and jj applies nothing.
pub fn apply(selection_path: &Path, left_dir: &Path, right_dir: &Path) -> Result<()> {
    let json = fs::read_to_string(selection_path).wrap_err_with(|| {
        format!(
            "failed to read selection file: {}",
            selection_path.display()
        )
    })?;
    let file_map: HashMap<String, FileSelection> =
        serde_json::from_str(&json).wrap_err("failed to parse selection JSON")?;

    let mut left = Tree::new(left_dir);
    let mut right = Tree::new(right_dir);

    // Files and links in right_dir (the "after" state).
    for rel_path in right.entries()? {
        let right_path = right_dir.join(&rel_path);
        let left_path = left.under(&rel_path).filter(|p| present(p));
        let left_is_link = left_path.as_deref().is_some_and(is_link);

        match (file_map.get(rel_path.to_string_lossy().as_ref()), left_path) {
            (Some(FileSelection::Full), _) => {
                // Keep right as-is (all changes included).
            }
            // A link's change is whole: selecting any of it selects all of it.
            (Some(FileSelection::Lines { .. }), _) if is_link(&right_path) || left_is_link => {}
            (Some(FileSelection::Lines { selected }), left_path) => {
                // Partial: reconstruct file with only selected changes.
                let left_content = match left_path {
                    Some(left_path) => fs::read(left_path)?,
                    None => Vec::new(), // new file -- left doesn't exist
                };
                let right_content = fs::read(&right_path)?;
                let output = apply_partial(&left_content, &right_content, selected);
                fs::write(&right_path, output)?;
            }
            // Not selected: revert to left (no changes).
            (None, Some(left_path)) => restore(&left_path, &right_path)?,
            // Added but not selected: delete from right.
            (None, None) => fs::remove_file(&right_path)?,
        }
    }

    // Files and links in left_dir but not right_dir (deletions).
    for rel_path in left.entries()? {
        let left_path = left_dir.join(&rel_path);
        let selection = file_map.get(rel_path.to_string_lossy().as_ref());
        let keep_deleted = match selection {
            // Deletion was selected -- keep it deleted.
            Some(FileSelection::Full) => true,
            Some(FileSelection::Lines { .. }) => is_link(&left_path),
            None => false,
        };
        if keep_deleted {
            continue;
        }
        let rel = rel_path.display();
        let Some(right_path) = right.under(&rel_path) else {
            color_eyre::eyre::bail!(
                "`{rel}` cannot be put back: a link or file kept in the result stands where its directory would be"
            );
        };
        match right_path.symlink_metadata() {
            // Right has the path too; the first loop settled it.
            Ok(meta) if !meta.is_dir() => continue,
            // A directory took the path's place. Empty, it is only what
            // reverting its files left behind; otherwise files were kept in
            // it, and the left side's file or link cannot stand there too.
            Ok(_) => {
                fs::remove_dir(&right_path).wrap_err_with(|| {
                    format!("`{rel}` cannot be put back: files kept in the result live under it")
                })?;
                right.forget(&rel_path);
            }
            Err(_) => {}
        }
        match selection {
            Some(FileSelection::Lines { selected }) => {
                // The file comes back without its selected lines; with every
                // line selected, nothing of it is left and it stays deleted.
                let kept = apply_partial(&fs::read(&left_path)?, &[], selected);
                if !kept.is_empty() {
                    restore(&left_path, &right_path)?;
                    fs::write(&right_path, kept)?;
                }
            }
            _ => restore(&left_path, &right_path)?,
        }
    }

    Ok(())
}

/// One of jj's diff directories. Paths are only built under it through
/// real directories: a link among the ancestors would resolve to wherever
/// the repository pointed it, which may be outside the tree.
struct Tree<'a> {
    root: &'a Path,
    /// Directories already found to be real, relative to `root`.
    real_dirs: HashSet<PathBuf>,
}

impl<'a> Tree<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            real_dirs: HashSet::new(),
        }
    }

    /// Every file and link in the tree, relative to the root, collected
    /// before anything is written so the walk never sees its own changes.
    /// Links are not followed, so a link to a directory is a leaf.
    fn entries(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in WalkDir::new(self.root) {
            let entry = entry?;
            if entry.file_type().is_dir() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(self.root)
                .wrap_err("walked outside the diff directory")?;
            paths.push(rel.to_path_buf());
        }
        Ok(paths)
    }

    /// `rel` under this tree, provided everything between the root and
    /// its last component is a real directory or does not exist yet.
    /// `None` when a link or a file stands in the way.
    fn under(&mut self, rel: &Path) -> Option<PathBuf> {
        let mut dir = PathBuf::new();
        for component in rel.parent()?.components() {
            dir.push(component);
            if self.real_dirs.contains(&dir) {
                continue;
            }
            match self.root.join(&dir).symlink_metadata() {
                Ok(meta) if meta.is_dir() => {
                    self.real_dirs.insert(dir.clone());
                }
                Ok(_) => return None,
                Err(_) => {}
            }
        }
        Some(self.root.join(rel))
    }

    /// The directory at `rel` is gone.
    fn forget(&mut self, rel: &Path) {
        self.real_dirs.remove(rel);
    }
}

/// Whether anything, a dangling link included, is at `path`.
fn present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn is_link(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// Put `from` at `to` in place of whatever is there. What is there is
/// removed first, so a link at `to` is replaced rather than written
/// through. A directory at `from` only clears `to`: its files come back
/// one by one from the walk of the left tree.
fn restore(from: &Path, to: &Path) -> Result<()> {
    if present(to) {
        fs::remove_file(to)?;
    } else if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let from_type = from.symlink_metadata()?.file_type();
    if from_type.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)?;
    } else if !from_type.is_dir() {
        fs::copy(from, to)?;
    }
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
    use std::path::{Path, PathBuf};

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
        Link(PathBuf),
    }

    fn file(content: &[u8]) -> Option<Entry> {
        Some(Entry::File(content.to_vec()))
    }

    fn link(target: &str) -> Option<Entry> {
        Some(Entry::Link(target.into()))
    }

    /// A path and what is at it before and after the change.
    type Sides = (&'static str, Option<Entry>, Option<Entry>);

    /// Diff directories as jj lays them out, beside an `outside` directory
    /// that no link from either tree should ever lead into.
    struct Dirs {
        _dir: tempfile::TempDir,
        left: PathBuf,
        right: PathBuf,
        outside: PathBuf,
    }

    fn dirs() -> Dirs {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            left: dir.path().join("left"),
            right: dir.path().join("right"),
            outside: dir.path().join("outside"),
            _dir: dir,
        };
        for d in [&dirs.left, &dirs.right, &dirs.outside] {
            std::fs::create_dir(d).unwrap();
        }
        dirs
    }

    fn put(root: &Path, path: &str, entry: &Option<Entry>) {
        let at = root.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        match entry {
            Some(Entry::File(content)) => std::fs::write(at, content).unwrap(),
            Some(Entry::Link(target)) => std::os::unix::fs::symlink(target, at).unwrap(),
            None => {}
        }
    }

    /// What `path` holds under `root`; a directory reads as a file holding
    /// its entry names.
    fn at(root: &Path, path: &str) -> Option<Entry> {
        let at = root.join(path);
        let meta = at.symlink_metadata().ok()?;
        Some(if meta.file_type().is_symlink() {
            Entry::Link(std::fs::read_link(at).unwrap())
        } else if meta.is_dir() {
            let mut names: Vec<String> = std::fs::read_dir(at)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Entry::File(names.join(",").into_bytes())
        } else {
            Entry::File(std::fs::read(at).unwrap())
        })
    }

    fn run(dirs: &Dirs, selection: &HashMap<&str, FileSelection>) -> color_eyre::Result<()> {
        let selection_path = dirs._dir.path().join("selection.json");
        std::fs::write(&selection_path, serde_json::to_vec(selection).unwrap()).unwrap();
        apply(&selection_path, &dirs.left, &dirs.right)
    }

    /// Run `apply` over diff directories holding `files`, and read back
    /// what each path holds on the right.
    fn apply_to(files: &[Sides], selection: &HashMap<&str, FileSelection>) -> Vec<Option<Entry>> {
        let dirs = dirs();
        for (path, before, after) in files {
            put(&dirs.left, path, before);
            put(&dirs.right, path, after);
        }
        run(&dirs, selection).unwrap();
        files
            .iter()
            .map(|(path, _, _)| at(&dirs.right, path))
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

    /// A whole-file selection keeps the file's change, an addition or a
    /// deletion included; without one the file goes back to how it was,
    /// so an unselected new file disappears and an unselected deleted one
    /// returns, into its directory if that went with it.
    #[test]
    fn each_file_keeps_its_change_only_when_selected() {
        let full = || Some(FileSelection::Full);
        check([
            (("f", file(b"a\n"), file(b"b\n")), full(), file(b"b\n")),
            (("f", file(b"a\n"), file(b"b\n")), None, file(b"a\n")),
            (("f", None, file(b"new\n")), full(), file(b"new\n")),
            (("f", None, file(b"new\n")), None, None),
            (("f", file(b"old\n"), None), full(), None),
            (("f", file(b"old\n"), None), None, file(b"old\n")),
            (("gone/f", file(b"old\n"), None), None, file(b"old\n")),
        ]);
    }

    /// Links follow the same rules as files, dangling or not, and a line
    /// selection takes a link's change whole. Restoring replaces whatever
    /// took the path's place rather than writing through it.
    #[test]
    fn each_link_keeps_its_change_only_when_selected() {
        let full = || Some(FileSelection::Full);
        let a_line = || lines([SelectedLine::Added { new_line: 1 }]);
        check([
            (("l", None, link("t")), full(), link("t")),
            (("l", None, link("t")), None, None),
            (("l", link("a"), link("b")), full(), link("b")),
            (("l", link("a"), link("b")), a_line(), link("b")),
            (("l", link("a"), link("b")), None, link("a")),
            (("l", link("a"), None), full(), None),
            (("l", link("a"), None), a_line(), None),
            (("l", link("a"), None), None, link("a")),
            (("l", file(b"x\n"), link("t")), None, file(b"x\n")),
            (("l", file(b"x\n"), link("t")), a_line(), link("t")),
            (("l", link("t"), file(b"x\n")), None, link("t")),
        ]);
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

    /// The staged content is read once, so it is removed whether or not
    /// it could be put in place.
    #[test]
    fn the_resolution_content_is_removed_even_when_it_cannot_be_applied() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("content");
        std::fs::write(&content, b"resolved\n").unwrap();
        let unwritable = dir.path().join("missing").join("out");
        assert!(super::apply_resolution(&content, &unwritable).is_err());
        assert!(!content.exists());

        std::fs::write(&content, b"resolved\n").unwrap();
        let output = dir.path().join("out");
        super::apply_resolution(&content, &output).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"resolved\n");
        assert!(!content.exists());
    }

    /// A link on the left that leads out of its tree is never read
    /// through: the left tree simply has nothing under it. An unselected
    /// file there is an addition to drop, and once the directory is empty
    /// the link takes its place again. Files kept under it leave nowhere
    /// for the link, so the apply fails rather than lose the kept files
    /// or the link's deletion.
    #[test]
    fn a_link_on_the_left_is_never_read_through() {
        let full = || Some(FileSelection::Full);
        let a_line = || lines([SelectedLine::Added { new_line: 1 }]);
        let cases = [
            (None, None, Ok(link("../outside"))),
            (None, a_line(), Err(())),
            (None, full(), Err(())),
            (full(), full(), Ok(Some(Entry::File(b"secret".to_vec())))),
        ];
        for (d, planted, expected) in cases {
            let dirs = dirs();
            std::fs::write(dirs.outside.join("secret"), b"REAL\n").unwrap();
            put(&dirs.left, "d", &link("../outside"));
            put(&dirs.right, "d/secret", &file(b"planted\n"));
            let selection: HashMap<&str, FileSelection> = [("d", d), ("d/secret", planted)]
                .into_iter()
                .filter_map(|(k, v)| Some((k, v?)))
                .collect();
            let name = format!("{selection:?}");
            let result = run(&dirs, &selection);
            match expected {
                Ok(entry) => {
                    result.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(at(&dirs.right, "d"), entry, "{name}");
                }
                Err(()) => assert!(result.is_err(), "{name}"),
            }
            assert_eq!(at(&dirs.outside, "secret"), file(b"REAL\n"), "{name}");
            // With the link back in place `d/secret` is `outside/secret`
            // itself; while `d` is a directory the planted file must hold
            // what was planted, never what the link leads to.
            if !matches!(at(&dirs.right, "d"), Some(Entry::Link(_))) {
                assert_eq!(at(&dirs.right, "d/secret"), file(b"planted\n"), "{name}");
            }
        }
    }

    /// A link kept on the right is never written through: an unselected
    /// deletion under it cannot be put back, so the apply fails and the
    /// link's target is untouched. With the link's own change unselected
    /// the directory comes back, file by file.
    #[test]
    fn a_link_on_the_right_is_never_written_through() {
        for (d, expected) in [(Some(FileSelection::Full), Err(())), (None, Ok(()))] {
            let dirs = dirs();
            put(&dirs.left, "d/f", &file(b"F\n"));
            put(&dirs.right, "d", &link("../outside"));
            let selection: HashMap<&str, FileSelection> = d.into_iter().map(|s| ("d", s)).collect();
            let name = format!("{selection:?}");
            let result = run(&dirs, &selection);
            match expected {
                Ok(()) => {
                    result.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(
                        at(&dirs.right, "d"),
                        Some(Entry::File(b"f".to_vec())),
                        "{name}"
                    );
                    assert_eq!(at(&dirs.right, "d/f"), file(b"F\n"), "{name}");
                }
                Err(()) => assert!(result.is_err(), "{name}"),
            }
            assert_eq!(at(&dirs.outside, "f"), None, "{name}");
        }
    }

    /// A directory replaced by a file comes back with its files when the
    /// change is unselected.
    #[test]
    fn an_unselected_directory_comes_back_file_by_file() {
        let dirs = dirs();
        put(&dirs.left, "d/f", &file(b"F\n"));
        put(&dirs.left, "d/g", &file(b"G\n"));
        put(&dirs.right, "d", &file(b"x\n"));
        run(&dirs, &HashMap::new()).unwrap();
        assert_eq!(at(&dirs.right, "d/f"), file(b"F\n"));
        assert_eq!(at(&dirs.right, "d/g"), file(b"G\n"));
    }
}
