//! Building a DAG in tests the way the repo service would deliver it.

use super::App;
use crate::dag::{CommitInfo, DagEntry};
use crate::repo_service::{RepoResult, RevsetData};

/// A 32-character change ID whose first two characters are `tag`.
pub(super) fn change_id(tag: char) -> String {
    format!("{tag}{tag}nnomkxrqvlypszwlwkvvqnstvzoxrs")
}

/// Change `tag` as the commit `commit`: two entries with the same tag
/// and different commits are one change before and after a rewrite.
pub(super) fn entry(tag: char, commit: &str) -> DagEntry {
    DagEntry {
        commit: CommitInfo::for_test(&change_id(tag), commit),
        edges: Vec::new(),
    }
}

/// A revset result starting a load of `entries`, the whole of it if `done`.
pub(super) fn load(app: &mut App, entries: Vec<DagEntry>, done: bool) {
    app.handle_repo_result(RepoResult::Revset {
        revset: "all()".into(),
        result: Ok(Box::new(RevsetData {
            revset: "all()".into(),
            repo_root: String::new(),
            entries,
            remote_bookmarks: Vec::new(),
            remotes: Vec::new(),
            all_tags: Vec::new(),
            tag_details: Default::default(),
            bookmark_details: Default::default(),
            workspace_entries: Vec::new(),
            warnings: Vec::new(),
            run_jobs: None,
            done,
        })),
    });
}

/// A later chunk of the load in progress.
pub(super) fn chunk(app: &mut App, entries: Vec<DagEntry>, done: bool) {
    app.handle_repo_result(RepoResult::RevsetChunk { entries, done });
}
