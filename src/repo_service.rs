use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::dag::{CommitDetails, DagEntry, DiffLine, DivergenceUpdate, PrefixLengthUpdate};
use crate::repo::JjRepo;
use crate::types::{BookmarkName, CommitId, RepoPath, Str};

pub struct RepoService;

#[derive(Clone)]
pub struct RepoRequestHandle {
    request_tx: Sender<RepoRequest>,
    current_epoch: Arc<AtomicU64>,
}

pub struct RepoResponseHandle {
    result_rx: Receiver<RepoResult>,
}

#[derive(Clone)]
enum RepoRequestKind {
    Revset {
        revset: Option<String>,
    },
    Commit {
        commit_id: CommitId,
    },
    FileDiff {
        commit_id: CommitId,
        path: RepoPath,
        old_path: Option<RepoPath>,
    },
    Operations {
        limit: usize,
    },
    ConflictHunks {
        commit_id: CommitId,
        path: RepoPath,
    },
    OpDiff {
        op_id: Str,
    },
}

#[derive(Clone)]
pub struct RepoRequest {
    epoch: u64,
    kind: RepoRequestKind,
}

pub struct RevsetData {
    pub revset: String,
    pub repo_root: String,
    pub entries: Vec<DagEntry>,
    pub untracked_bookmarks: Vec<Str>,
    pub tracked_bookmarks: Vec<Str>,
    pub remotes: Vec<Str>,
    pub all_tags: Vec<Str>,
    pub tag_details: std::collections::HashMap<Str, crate::dag::TagDetails>,
    pub bookmark_details: std::collections::HashMap<BookmarkName, crate::dag::BookmarkDetails>,
    pub workspace_entries: Vec<crate::app::WorkspaceViewEntry>,
}

pub enum RepoResult {
    RevsetLoaded(Box<RevsetData>),
    RevsetFailed {
        revset: String,
        error: String,
    },
    /// The workspace was stale and has been (or failed to be) recovered.
    WorkspaceUpdatedStale {
        message: String,
    },
    /// Background-computed is_empty for a single commit.
    CommitEmpty {
        commit_id: CommitId,
    },
    DivergenceInfo {
        updates: Vec<(CommitId, DivergenceUpdate)>,
    },
    PrefixLengths {
        updates: Vec<(CommitId, PrefixLengthUpdate)>,
    },
    CommitDetailsLoaded {
        commit_id: CommitId,
        details: CommitDetails,
    },
    CommitDetailsFailed {
        commit_id: CommitId,
        error: String,
    },
    FileDiffLoaded {
        commit_id: CommitId,
        path: RepoPath,
        lines: Vec<DiffLine>,
    },
    FileDiffFailed {
        commit_id: CommitId,
        path: RepoPath,
        error: String,
    },
    /// Background-computed prefix lengths for bookmark detail commits
    /// (conflict targets, remote tracking targets not in the DAG).
    BookmarkDetailPrefixLengths {
        updates: Vec<(CommitId, PrefixLengthUpdate)>,
    },
    OperationsLoaded {
        entries: Vec<crate::app::OpLogEntry>,
        has_more: bool,
    },
    OperationsFailed {
        error: String,
    },
    ConflictHunksLoaded {
        commit_id: CommitId,
        path: RepoPath,
        hunks: Vec<crate::dag::ConflictHunk>,
    },
    ConflictHunksFailed {
        commit_id: CommitId,
        path: RepoPath,
        error: String,
    },
    OpDiffLoaded {
        op_id: Str,
        lines: Vec<crate::app::OpDetailLine>,
    },
    OpDiffFailed {
        op_id: Str,
        error: String,
    },
    /// A background computation thread panicked or failed.
    BackgroundError {
        error: String,
    },
}

impl RepoRequest {
    pub fn load_revset(revset: Option<String>) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Revset { revset },
        }
    }

    pub fn load_commit_details(commit_id: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Commit { commit_id },
        }
    }

    pub fn load_file_diff(commit_id: CommitId, path: RepoPath, old_path: Option<RepoPath>) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::FileDiff {
                commit_id,
                path,
                old_path,
            },
        }
    }

    pub fn load_conflict_hunks(commit_id: CommitId, path: RepoPath) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::ConflictHunks { commit_id, path },
        }
    }

    pub fn load_operations(limit: usize) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Operations { limit },
        }
    }

    pub fn load_op_diff(op_id: Str) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::OpDiff { op_id },
        }
    }
}

impl RepoService {
    pub fn spawn(repo_path: PathBuf) -> (RepoRequestHandle, RepoResponseHandle) {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let current_epoch = Arc::new(AtomicU64::new(0));
        let request_handle = RepoRequestHandle {
            request_tx,
            current_epoch: Arc::clone(&current_epoch),
        };
        let response_handle = RepoResponseHandle { result_rx };

        thread::spawn(move || {
            let mut service = RepoServiceState::new(repo_path, result_tx, current_epoch);
            service.run(request_rx);
        });

        (request_handle, response_handle)
    }
}

impl RepoRequestHandle {
    pub fn send(&self, mut request: RepoRequest) {
        request.epoch = match request.kind {
            RepoRequestKind::Revset { .. } => self.current_epoch.fetch_add(1, Ordering::SeqCst) + 1,
            RepoRequestKind::Commit { .. }
            | RepoRequestKind::FileDiff { .. }
            | RepoRequestKind::Operations { .. }
            | RepoRequestKind::ConflictHunks { .. }
            | RepoRequestKind::OpDiff { .. } => self.current_epoch.load(Ordering::SeqCst),
        };
        let _ = self.request_tx.send(request);
    }
}

impl RepoResponseHandle {
    pub fn into_receiver(self) -> Receiver<RepoResult> {
        self.result_rx
    }

    pub fn spawn_forwarder<T, F>(self, event_tx: Sender<T>, map: F) -> thread::JoinHandle<()>
    where
        T: Send + 'static,
        F: Fn(RepoResult) -> T + Send + 'static,
    {
        thread::spawn(move || {
            while let Ok(result) = self.result_rx.recv() {
                if event_tx.send(map(result)).is_err() {
                    break;
                }
            }
        })
    }
}

struct RepoServiceState {
    repo_path: PathBuf,
    repo: Option<JjRepo>,
    result_tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    in_flight_commit_details: HashSet<CommitId>,
    in_flight_file_diffs: HashSet<(CommitId, RepoPath)>,
}

impl RepoServiceState {
    fn new(
        repo_path: PathBuf,
        result_tx: Sender<RepoResult>,
        current_epoch: Arc<AtomicU64>,
    ) -> Self {
        Self {
            repo_path,
            repo: None,
            result_tx,
            current_epoch,
            in_flight_commit_details: HashSet::new(),
            in_flight_file_diffs: HashSet::new(),
        }
    }

    fn run(&mut self, request_rx: Receiver<RepoRequest>) {
        while let Ok(request) = request_rx.recv() {
            self.handle_request(request);
        }
    }

    fn handle_request(&mut self, request: RepoRequest) {
        let RepoRequest { epoch, kind } = request;
        match kind {
            RepoRequestKind::Revset { revset } => {
                self.in_flight_commit_details.clear();
                self.in_flight_file_diffs.clear();
                self.handle_revset(epoch, revset);
            }
            RepoRequestKind::Commit { commit_id } => {
                self.handle_commit_details(epoch, commit_id);
            }
            RepoRequestKind::FileDiff {
                commit_id,
                path,
                old_path,
            } => {
                self.handle_file_diff(epoch, commit_id, path, old_path);
            }
            RepoRequestKind::Operations { limit } => {
                self.handle_operations(epoch, limit);
            }
            RepoRequestKind::ConflictHunks { commit_id, path } => {
                self.handle_conflict_hunks(epoch, commit_id, path);
            }
            RepoRequestKind::OpDiff { op_id } => {
                self.handle_op_diff(epoch, op_id);
            }
        }
    }

    fn handle_revset(&mut self, epoch: u64, revset: Option<String>) {
        let requested_revset = revset.clone().unwrap_or_default();

        // Always do a full snapshot + open to pick up all changes.
        self.repo = None;

        if let Err(err) = JjRepo::snapshot(&self.repo_path) {
            if err.contains("stale") {
                match JjRepo::update_stale(&self.repo_path) {
                    Ok(()) => {
                        let _ = self.result_tx.send(RepoResult::WorkspaceUpdatedStale {
                            message: "workspace was stale — updated".to_string(),
                        });
                        let _ = JjRepo::snapshot(&self.repo_path);
                    }
                    Err(update_err) => {
                        self.send_if_current(
                            epoch,
                            RepoResult::RevsetFailed {
                                revset: requested_revset,
                                error: format!(
                                    "workspace is stale and update-stale failed:\n{update_err}"
                                ),
                            },
                        );
                        return;
                    }
                }
            }
        }
        match JjRepo::open(&self.repo_path) {
            Ok(repo) => self.repo = Some(repo),
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::RevsetFailed {
                        revset: requested_revset,
                        error: format!("{err:#}"),
                    },
                );
                return;
            }
        }

        let Some(repo) = self.repo.as_ref() else {
            return;
        };
        let effective_revset = revset.unwrap_or_else(|| repo.default_revset());
        match repo.evaluate_revset(&effective_revset) {
            Ok(entries) => {
                // Collect all commit IDs for background is_empty computation.
                // (Previously only merge commits were deferred; now all commits
                // defer is_empty to keep the initial load fast.)

                // Collect commit IDs before sending entries (which moves them).
                let all_ids: Vec<CommitId> =
                    entries.iter().map(|e| e.commit.graph_id.clone()).collect();

                let untracked_bookmarks = repo.untracked_remote_bookmarks();
                let tracked_bookmarks = repo.tracked_remote_bookmarks();
                let remotes = repo.git_remotes();
                let all_tags = repo.all_local_tags();
                let tag_details = repo.extract_tag_details();
                let bookmark_details = repo.extract_bookmark_details();
                let workspace_entries = repo.workspace_entries();

                // Collect unique commit IDs from bookmark + tag details for prefix computation.
                let detail_commit_ids: Vec<CommitId> = {
                    let mut ids = std::collections::HashSet::new();
                    for details in bookmark_details.values() {
                        for ct in &details.conflict_targets {
                            ids.insert(ct.commit_id.clone());
                        }
                        for rt in &details.remote_targets {
                            ids.insert(rt.commit_id.clone());
                        }
                    }
                    for details in tag_details.values() {
                        if let Some(lt) = &details.local_target {
                            ids.insert(lt.commit_id.clone());
                        }
                        for rt in &details.remote_targets {
                            ids.insert(rt.commit_id.clone());
                        }
                    }
                    ids.into_iter().collect()
                };

                self.send_if_current(
                    epoch,
                    RepoResult::RevsetLoaded(Box::new(RevsetData {
                        revset: effective_revset,
                        repo_root: repo.workspace_root().display().to_string(),
                        entries,
                        untracked_bookmarks,
                        tracked_bookmarks,
                        remotes,
                        all_tags,
                        tag_details,
                        bookmark_details,
                        workspace_entries,
                    })),
                );

                // Spawn background thread to compute is_empty for all commits.
                {
                    let empty_ids = all_ids.clone();
                    let inner = repo.inner_repo();
                    let tx = self.result_tx.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        for id in &empty_ids {
                            let Some(backend_id) =
                                jj_lib::backend::CommitId::try_from_hex(id.as_str())
                            else {
                                continue;
                            };
                            let Ok(commit) = inner.store().get_commit(&backend_id) else {
                                continue;
                            };
                            if commit.is_empty(inner.as_ref()).block_on().unwrap_or(false) {
                                let _ = tx.send(RepoResult::CommitEmpty {
                                    commit_id: id.clone(),
                                });
                            }
                        }
                    });
                }

                // Spawn background thread to compute divergence/hidden status.
                {
                    let inner = repo.inner_repo();
                    let div_ids = all_ids.clone();
                    let tx = self.result_tx.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        let updates = JjRepo::compute_divergence_info(&inner, &div_ids);
                        if !updates.is_empty() {
                            let _ = tx.send(RepoResult::DivergenceInfo { updates });
                        }
                    });
                }

                // Spawn background thread to compute shortest unique ID prefixes.
                {
                    let tx = self.result_tx.clone();
                    let repo_path = self.repo_path.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        let bg_repo = match JjRepo::open(&repo_path) {
                            Ok(r) => r,
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: format!("prefix lengths: {e:#}"),
                                });
                                return;
                            }
                        };
                        match bg_repo.compute_prefix_lengths(&all_ids) {
                            Ok(updates) if !updates.is_empty() => {
                                let _ = tx.send(RepoResult::PrefixLengths { updates });
                            }
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: format!("prefix lengths: {e:#}"),
                                });
                            }
                            _ => {}
                        }
                    });
                }

                // Spawn background thread for bookmark detail prefix lengths.
                if !detail_commit_ids.is_empty() {
                    let tx = self.result_tx.clone();
                    let repo_path = self.repo_path.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        let bg_repo = match JjRepo::open(&repo_path) {
                            Ok(r) => r,
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: format!("detail prefix lengths: {e:#}"),
                                });
                                return;
                            }
                        };
                        match bg_repo.compute_prefix_lengths(&detail_commit_ids) {
                            Ok(updates) if !updates.is_empty() => {
                                let _ =
                                    tx.send(RepoResult::BookmarkDetailPrefixLengths { updates });
                            }
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: format!("detail prefix lengths: {e:#}"),
                                });
                            }
                            _ => {}
                        }
                    });
                }
            }
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::RevsetFailed {
                        revset: effective_revset,
                        error: format!("{err:#}"),
                    },
                );
            }
        }
    }

    fn handle_commit_details(&mut self, epoch: u64, commit_id: CommitId) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        if !self.in_flight_commit_details.insert(commit_id.clone()) {
            return;
        }
        let Some(repo) = self.repo.as_ref() else {
            self.in_flight_commit_details.remove(&commit_id);
            self.send_if_current(
                epoch,
                RepoResult::CommitDetailsFailed {
                    commit_id,
                    error: "repository not loaded yet".to_string(),
                },
            );
            return;
        };

        match repo.commit_details(&commit_id) {
            Ok(details) => self.send_if_current(
                epoch,
                RepoResult::CommitDetailsLoaded {
                    commit_id: commit_id.clone(),
                    details,
                },
            ),
            Err(err) => self.send_if_current(
                epoch,
                RepoResult::CommitDetailsFailed {
                    commit_id: commit_id.clone(),
                    error: format!("{err:#}"),
                },
            ),
        }
        self.in_flight_commit_details.remove(&commit_id);
    }

    fn handle_file_diff(
        &mut self,
        epoch: u64,
        commit_id: CommitId,
        path: RepoPath,
        old_path: Option<RepoPath>,
    ) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        let key = (commit_id.clone(), path.clone());
        if !self.in_flight_file_diffs.insert(key.clone()) {
            return;
        }
        let Some(repo) = self.repo.as_ref() else {
            self.in_flight_file_diffs.remove(&key);
            self.send_if_current(
                epoch,
                RepoResult::FileDiffFailed {
                    commit_id,
                    path,
                    error: "repository not loaded yet".to_string(),
                },
            );
            return;
        };

        match repo.file_diff(&commit_id, &path, old_path.as_ref()) {
            Ok(lines) => self.send_if_current(
                epoch,
                RepoResult::FileDiffLoaded {
                    commit_id: commit_id.clone(),
                    path: path.clone(),
                    lines,
                },
            ),
            Err(err) => self.send_if_current(
                epoch,
                RepoResult::FileDiffFailed {
                    commit_id: commit_id.clone(),
                    path: path.clone(),
                    error: format!("{err:#}"),
                },
            ),
        }
        self.in_flight_file_diffs.remove(&key);
    }

    fn handle_operations(&mut self, epoch: u64, limit: usize) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        // Op log needs a repo; open one if not already loaded.
        if self.repo.is_none() {
            match JjRepo::open(&self.repo_path) {
                Ok(repo) => self.repo = Some(repo),
                Err(err) => {
                    self.send_if_current(
                        epoch,
                        RepoResult::OperationsFailed {
                            error: format!("{err:#}"),
                        },
                    );
                    return;
                }
            }
        }
        let repo = self.repo.as_ref().unwrap();
        match repo.operation_log(limit) {
            Ok((entries, has_more)) => {
                self.send_if_current(epoch, RepoResult::OperationsLoaded { entries, has_more });
            }
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::OperationsFailed {
                        error: format!("{err:#}"),
                    },
                );
            }
        }
    }

    fn handle_conflict_hunks(&mut self, epoch: u64, commit_id: CommitId, path: RepoPath) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        if self.repo.is_none() {
            match JjRepo::open(&self.repo_path) {
                Ok(repo) => self.repo = Some(repo),
                Err(err) => {
                    self.send_if_current(
                        epoch,
                        RepoResult::ConflictHunksFailed {
                            commit_id,
                            path,
                            error: format!("{err:#}"),
                        },
                    );
                    return;
                }
            }
        }
        let repo = self.repo.as_ref().unwrap();
        match repo.conflict_hunks(&commit_id, &path) {
            Ok(hunks) => {
                self.send_if_current(
                    epoch,
                    RepoResult::ConflictHunksLoaded {
                        commit_id,
                        path,
                        hunks,
                    },
                );
            }
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::ConflictHunksFailed {
                        commit_id,
                        path,
                        error: format!("{err:#}"),
                    },
                );
            }
        }
    }

    fn handle_op_diff(&mut self, epoch: u64, op_id: Str) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        if self.repo.is_none() {
            match JjRepo::open(&self.repo_path) {
                Ok(repo) => self.repo = Some(repo),
                Err(err) => {
                    self.send_if_current(
                        epoch,
                        RepoResult::OpDiffFailed {
                            op_id,
                            error: format!("{err:#}"),
                        },
                    );
                    return;
                }
            }
        }
        let repo = self.repo.as_ref().unwrap();
        match repo.op_diff(&op_id) {
            Ok(lines) => {
                self.send_if_current(epoch, RepoResult::OpDiffLoaded { op_id, lines });
            }
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::OpDiffFailed {
                        op_id,
                        error: format!("{err:#}"),
                    },
                );
            }
        }
    }

    fn send_if_current(&self, epoch: u64, result: RepoResult) {
        if epoch == self.current_epoch.load(Ordering::SeqCst) {
            let _ = self.result_tx.send(result);
        }
    }
}

/// Spawn a background thread with a panic handler that reports errors
/// instead of silently dying.
fn spawn_background(err_tx: Sender<RepoResult>, f: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Err(e) = result {
            let msg = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            let _ = err_tx.send(RepoResult::BackgroundError { error: msg });
        }
    });
}
