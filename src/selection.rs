//! Serialization of file/line selections to JSON for the diff tool.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use color_eyre::Result;
use serde::Serialize;

use crate::types::{FileRef, Selection};

/// Serialized selection for a single file.
#[derive(Serialize)]
#[serde(tag = "mode")]
enum FileSelection {
    /// Include all changes from this file.
    #[serde(rename = "full")]
    Full,
    /// Include only specific lines.
    #[serde(rename = "lines")]
    Lines { selected: Vec<SelectedLine> },
}

/// A single selected diff line (for the diff tool to apply).
#[derive(Serialize)]
struct SelectedLine {
    kind: &'static str,
    old_line: Option<u32>,
    new_line: Option<u32>,
}

/// Serialize the current selections to a temporary JSON file.
///
/// Returns the path to the temp file. The file is NOT auto-deleted;
/// the caller should clean up or use `tempfile::NamedTempFile::keep()`.
pub fn serialize_selections(selections: &HashSet<Selection>) -> Result<PathBuf> {
    let mut file_map: HashMap<&str, FileSelection> = HashMap::new();

    for sel in selections {
        match sel {
            Selection::File(FileRef { path, .. }) => {
                file_map.insert(path.as_str(), FileSelection::Full);
            }
            Selection::Line {
                file_ref: FileRef {
                    path,
                    change_id: _,
                },
                old_line,
                new_line,
            } => {
                let kind = match (old_line, new_line) {
                    (Some(_), None) => "removed",
                    (None, Some(_)) => "added",
                    _ => continue,
                };
                match file_map.get_mut(path.as_str()) {
                    Some(FileSelection::Full) => {
                        // File is already fully selected; line selection is redundant.
                    }
                    Some(FileSelection::Lines { selected }) => {
                        selected.push(SelectedLine {
                            kind,
                            old_line: *old_line,
                            new_line: *new_line,
                        });
                    }
                    None => {
                        file_map.insert(
                            path.as_str(),
                            FileSelection::Lines {
                                selected: vec![SelectedLine {
                                    kind,
                                    old_line: *old_line,
                                    new_line: *new_line,
                                }],
                            },
                        );
                    }
                }
            }
        }
    }

    let mut tmp = tempfile::NamedTempFile::new()?;
    serde_json::to_writer_pretty(&mut tmp, &file_map)?;
    tmp.flush()?;

    // Persist the temp file (don't delete on drop) and return its path.
    let (_, path) = tmp.keep()?;
    Ok(path)
}
