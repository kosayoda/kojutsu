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
    load_data(app, revset_data(entries, done));
}

/// What a load of `entries` delivers, with nothing else in the repo, for a
/// test to add to before handing it to [`load_data`].
pub(super) fn revset_data(entries: Vec<DagEntry>, done: bool) -> RevsetData {
    RevsetData {
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
        editor: Default::default(),
        done,
    }
}

/// A revset result starting a load that delivers `data`.
pub(super) fn load_data(app: &mut App, data: RevsetData) {
    app.handle_repo_result(RepoResult::Revset {
        revset: data.revset.clone(),
        result: Ok(Box::new(data)),
    });
}

/// A later chunk of the load in progress.
pub(super) fn chunk(app: &mut App, entries: Vec<DagEntry>, done: bool) {
    app.handle_repo_result(RepoResult::RevsetChunk { entries, done });
}
