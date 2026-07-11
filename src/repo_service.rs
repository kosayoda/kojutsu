use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::dag::{CommitDetails, DagEntry, DivergenceInfo, PrefixLengthUpdate};
use crate::repo::{JjRepo, SnapshotError};
use crate::types::{BookmarkName, CommitId, OperationId, RemoteName, RepoPath, TagName};

pub struct RepoService;

#[derive(Clone)]
pub struct RepoRequestHandle {
    request_tx: Sender<RepoRequest>,
    current_epoch: Arc<AtomicU64>,
}

pub struct RepoResponseHandle {
    result_rx: Receiver<RepoResult>,
}

/// Whether a revset load should snapshot the working copy first.
#[derive(Clone, Copy, Debug)]
pub enum RevsetLoadKind {
    /// Snapshot first — use after actions that may have changed the filesystem.
    Snapshot,
    /// Skip the snapshot — use for pure revset/UI changes.
    NoSnapshot,
}

#[derive(Clone)]
enum RepoRequestKind {
    Revset {
        revset: Option<String>,
        load_kind: RevsetLoadKind,
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
        op_id: OperationId,
    },
    EvolutionLog {
        commit_id: CommitId,
    },
    EvoLogDetails {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
    },
    EvoLogFileDiff {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
    },
    InterdiffDetails {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
    },
    InterdiffFileDiff {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
    },
    Annotate {
        commit_id: CommitId,
        path: RepoPath,
    },
    FileList {
        commit_id: CommitId,
    },
}

impl RepoRequestKind {
    fn label(&self) -> &'static str {
        match self {
            Self::Revset { .. } => "revset",
            Self::Commit { .. } => "commit",
            Self::FileDiff { .. } => "file_diff",
            Self::Operations { .. } => "operations",
            Self::ConflictHunks { .. } => "conflict_hunks",
            Self::OpDiff { .. } => "op_diff",
            Self::EvolutionLog { .. } => "evolution_log",
            Self::EvoLogDetails { .. } => "evolog_details",
            Self::EvoLogFileDiff { .. } => "evolog_file_diff",
            Self::InterdiffDetails { .. } => "interdiff_details",
            Self::InterdiffFileDiff { .. } => "interdiff_file_diff",
            Self::Annotate { .. } => "annotate",
            Self::FileList { .. } => "file_list",
        }
    }
}

#[derive(Clone)]
pub struct RepoRequest {
    epoch: u64,
    kind: RepoRequestKind,
}

/// What subsystem produced the error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoErrorKind {
    /// Repository could not be opened.
    RepoOpen,
    /// A revset expression failed to parse or evaluate.
    Revset,
    /// A jj operation failed (commit, diff, annotate, etc.).
    Operation,
    /// The workspace is stale and recovery failed.
    WorkspaceStale,
    /// A background task panicked or failed to open the repo.
    Background,
}

/// Error from the repository service layer.
#[derive(Clone, Debug)]
pub struct RepoError {
    pub kind: RepoErrorKind,
    pub message: String,
}

impl RepoError {
    pub fn new(kind: RepoErrorKind, msg: impl Into<String>) -> Self {
        Self {
            kind,
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RepoError {}

pub struct RevsetData {
    pub revset: String,
    pub repo_root: String,
    pub entries: Vec<DagEntry>,
    pub remote_bookmarks: Vec<crate::dag::RemoteBookmarkRef>,
    pub remotes: Vec<RemoteName>,
    pub all_tags: Vec<TagName>,
    pub tag_details: std::collections::HashMap<TagName, crate::dag::TagDetails>,
    pub bookmark_details: std::collections::HashMap<BookmarkName, crate::dag::BookmarkDetails>,
    pub workspace_entries: Vec<crate::app::WorkspaceViewEntry>,
    /// Non-fatal warnings from revset evaluation (e.g. immutable() failed).
    pub warnings: Vec<String>,
}

pub enum RepoResult {
    Revset {
        revset: String,
        result: Result<Box<RevsetData>, RepoError>,
    },
    /// The workspace was stale and has been (or failed to be) recovered.
    WorkspaceUpdatedStale { message: String },
    /// Background-computed is_empty for a single commit.
    CommitEmpty { commit_id: CommitId },
    DivergenceInfo {
        updates: Vec<(CommitId, DivergenceInfo)>,
    },
    PrefixLengths {
        updates: Vec<(CommitId, PrefixLengthUpdate)>,
    },
    CommitDetails {
        commit_id: CommitId,
        result: Result<CommitDetails, RepoError>,
    },
    FileDiff {
        commit_id: CommitId,
        path: RepoPath,
        result: Result<crate::dag::DiffResult, RepoError>,
    },
    /// Background-computed prefix lengths for bookmark detail commits
    /// (conflict targets, remote tracking targets not in the DAG).
    BookmarkDetailPrefixLengths {
        updates: Vec<(CommitId, PrefixLengthUpdate)>,
    },
    Operations {
        result: Result<(Vec<crate::app::OpLogEntry>, bool), RepoError>,
    },
    ConflictHunks {
        commit_id: CommitId,
        path: RepoPath,
        result: Result<Vec<crate::dag::ConflictHunkKind>, RepoError>,
    },
    OpDiff {
        op_id: OperationId,
        result: Result<Vec<crate::app::OpDetailLine>, RepoError>,
    },
    EvoLog {
        commit_id: CommitId,
        result: Result<Vec<crate::app::EvoLogEntry>, RepoError>,
    },
    EvoLogDetails {
        commit_id: CommitId,
        result: Result<Vec<crate::dag::FileChange>, RepoError>,
    },
    EvoLogFileDiff {
        commit_id: CommitId,
        path: RepoPath,
        result: Result<crate::dag::DiffResult, RepoError>,
    },
    InterdiffDetails {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        result: Result<Vec<crate::dag::FileChange>, RepoError>,
    },
    InterdiffFileDiff {
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
        result: Result<crate::dag::DiffResult, RepoError>,
    },
    Annotate {
        commit_id: CommitId,
        path: RepoPath,
        result: Result<crate::dag::AnnotateResult, RepoError>,
    },
    FileList {
        commit_id: CommitId,
        result: Result<Vec<RepoPath>, RepoError>,
    },
    /// A background computation thread panicked or failed.
    BackgroundError { error: RepoError },
}

impl RepoRequest {
    pub fn load_revset(revset: Option<String>) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Revset {
                revset,
                load_kind: RevsetLoadKind::Snapshot,
            },
        }
    }

    pub fn load_revset_no_snapshot(revset: Option<String>) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Revset {
                revset,
                load_kind: RevsetLoadKind::NoSnapshot,
            },
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

    pub fn load_op_diff(op_id: OperationId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::OpDiff { op_id },
        }
    }

    pub fn load_evolog_details(from_commit_id: CommitId, to_commit_id: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::EvoLogDetails {
                from_commit_id,
                to_commit_id,
            },
        }
    }

    pub fn load_evolog_file_diff(
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
    ) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::EvoLogFileDiff {
                from_commit_id,
                to_commit_id,
                path,
            },
        }
    }

    pub fn load_evolution_log(commit_id: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::EvolutionLog { commit_id },
        }
    }

    pub fn load_interdiff_details(from: CommitId, to: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::InterdiffDetails {
                from_commit_id: from,
                to_commit_id: to,
            },
        }
    }

    pub fn load_interdiff_file_diff(from: CommitId, to: CommitId, path: RepoPath) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::InterdiffFileDiff {
                from_commit_id: from,
                to_commit_id: to,
                path,
            },
        }
    }

    pub fn load_file_annotate(commit_id: CommitId, path: RepoPath) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::Annotate { commit_id, path },
        }
    }

    pub fn load_file_list(commit_id: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::FileList { commit_id },
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
        // Each revset load starts a new epoch; a queued revset load whose
        // epoch is no longer current has been superseded and is skipped.
        // Other requests deliver keyed results and don't need an epoch.
        request.epoch = match request.kind {
            RepoRequestKind::Revset { .. } => self.current_epoch.fetch_add(1, Ordering::SeqCst) + 1,
            _ => self.current_epoch.load(Ordering::SeqCst),
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

/// A token that background threads check to bail out early when their
/// work is no longer needed (e.g. a new revset was requested).
#[derive(Clone)]
pub struct CancellationToken(Arc<std::sync::atomic::AtomicBool>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }

    /// Signal all holders of this token to stop.
    fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Check whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

struct RepoServiceState {
    repo_path: PathBuf,
    repo: Option<Arc<JjRepo>>,
    result_tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    /// Cancellation token for background threads spawned by the current revset.
    bg_cancel: CancellationToken,
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
            bg_cancel: CancellationToken::new(),
        }
    }

    fn run(&mut self, request_rx: Receiver<RepoRequest>) {
        let mut pending: VecDeque<RepoRequest> = VecDeque::new();
        loop {
            if pending.is_empty() {
                match request_rx.recv() {
                    Ok(request) => pending.push_back(request),
                    Err(_) => return,
                }
            }
            pending.extend(request_rx.try_iter());
            // Revset loads jump the queue: they supersede stale queued work
            // and re-open the repo that subsequent requests run against.
            let request = pending
                .iter()
                .position(|r| matches!(r.kind, RepoRequestKind::Revset { .. }))
                .and_then(|i| pending.remove(i))
                .or_else(|| pending.pop_front())
                .expect("pending is non-empty");
            self.handle_request(request);
        }
    }

    fn handle_request(&mut self, request: RepoRequest) {
        let RepoRequest { epoch, kind } = request;
        tracing::debug!(epoch, request = %kind.label(), "repo request");
        match kind {
            RepoRequestKind::Revset { revset, load_kind } => {
                // A newer revset load has been requested since this one was
                // queued — skip the snapshot and evaluation entirely.
                if epoch != self.current_epoch.load(Ordering::SeqCst) {
                    tracing::debug!(epoch, "skipping superseded revset load");
                    return;
                }
                self.bg_cancel.cancel();
                self.bg_cancel = CancellationToken::new();
                self.handle_revset(epoch, revset, load_kind);
            }
            RepoRequestKind::Commit { commit_id } => {
                self.handle_commit_details(commit_id);
            }
            RepoRequestKind::FileDiff {
                commit_id,
                path,
                old_path,
            } => {
                self.handle_file_diff(commit_id, path, old_path);
            }
            RepoRequestKind::Operations { limit } => {
                self.handle_operations(limit);
            }
            RepoRequestKind::ConflictHunks { commit_id, path } => {
                self.handle_conflict_hunks(commit_id, path);
            }
            RepoRequestKind::OpDiff { op_id } => {
                self.handle_op_diff(op_id);
            }
            RepoRequestKind::EvolutionLog { commit_id } => {
                self.handle_evolution_log(commit_id);
            }
            RepoRequestKind::EvoLogDetails {
                from_commit_id,
                to_commit_id,
            } => {
                self.handle_evolog_details(from_commit_id, to_commit_id);
            }
            RepoRequestKind::EvoLogFileDiff {
                from_commit_id,
                to_commit_id,
                path,
            } => {
                self.handle_evolog_file_diff(from_commit_id, to_commit_id, path);
            }
            RepoRequestKind::InterdiffDetails {
                from_commit_id,
                to_commit_id,
            } => {
                self.handle_interdiff_details(from_commit_id, to_commit_id);
            }
            RepoRequestKind::InterdiffFileDiff {
                from_commit_id,
                to_commit_id,
                path,
            } => {
                self.handle_interdiff_file_diff(from_commit_id, to_commit_id, path);
            }
            RepoRequestKind::Annotate { commit_id, path } => {
                self.handle_file_annotate(commit_id, path);
            }
            RepoRequestKind::FileList { commit_id } => {
                self.handle_file_list(commit_id);
            }
        }
    }

    fn handle_revset(&mut self, epoch: u64, revset: Option<String>, load_kind: RevsetLoadKind) {
        let start = std::time::Instant::now();
        let requested_revset = revset.clone().unwrap_or_default();
        tracing::info!(revset = %requested_revset, ?load_kind, "loading revset");

        // Re-open the repo to pick up changes. Only snapshot when the working
        // copy may have changed (e.g. after returning from an external command).
        self.repo = None;

        if matches!(load_kind, RevsetLoadKind::Snapshot) {
            if let Err(err) = JjRepo::snapshot(&self.repo_path) {
                if matches!(err, SnapshotError::Stale(_)) {
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
                                RepoResult::Revset {
                                    revset: requested_revset,
                                    result: Err(RepoError {
                                        kind: RepoErrorKind::WorkspaceStale,
                                        message: format!(
                                            "workspace is stale and update-stale failed:\n{update_err}"
                                        ),
                                    }),
                                },
                            );
                            return;
                        }
                    }
                }
            }
        }
        match JjRepo::open(&self.repo_path) {
            Ok(repo) => self.repo = Some(Arc::new(repo)),
            Err(err) => {
                self.send_if_current(
                    epoch,
                    RepoResult::Revset {
                        revset: requested_revset,
                        result: Err(RepoError {
                            kind: RepoErrorKind::RepoOpen,
                            message: format!("{err:#}"),
                        }),
                    },
                );
                return;
            }
        }

        let Some(repo) = self.repo.as_deref() else {
            return;
        };
        let effective_revset = revset.unwrap_or_else(|| repo.default_revset());
        match repo.evaluate_revset(&effective_revset) {
            Ok(result) => {
                let warnings = result.warnings;
                let entries = result.entries;

                // Collect all commit IDs for background is_empty computation.
                // (Previously only merge commits were deferred; now all commits
                // defer is_empty to keep the initial load fast.)

                // Collect commit IDs before sending entries (which moves them).
                let all_ids: Vec<CommitId> =
                    entries.iter().map(|e| e.commit.graph_id.clone()).collect();

                let remote_bookmarks = repo.all_remote_bookmark_refs();
                let remotes = repo.git_remotes();
                let all_tags = repo.all_local_tags();
                let tag_details = repo.extract_tag_details();
                let bookmark_details = repo.extract_bookmark_details();
                let workspace_entries = repo.workspace_entries();

                // Collect unique commit IDs from bookmark + tag details + workspaces
                // for prefix computation.
                let detail_commit_ids: Vec<CommitId> = {
                    let mut ids = std::collections::HashSet::new();
                    for details in bookmark_details.values() {
                        for ct in &details.conflict_targets {
                            ids.insert(ct.summary.commit_id.clone());
                        }
                        for rt in &details.remote_targets {
                            ids.insert(rt.summary.commit_id.clone());
                        }
                    }
                    for details in tag_details.values() {
                        if let Some(lt) = &details.local_target {
                            ids.insert(lt.summary.commit_id.clone());
                        }
                        for rt in &details.remote_targets {
                            ids.insert(rt.summary.commit_id.clone());
                        }
                    }
                    for ws in &workspace_entries {
                        if let Some(ref cid) = ws.commit_id {
                            ids.insert(cid.clone());
                        }
                    }
                    ids.into_iter().collect()
                };

                let entry_count = entries.len();
                self.send_if_current(
                    epoch,
                    RepoResult::Revset {
                        revset: effective_revset.clone(),
                        result: Ok(Box::new(RevsetData {
                            revset: effective_revset,
                            repo_root: repo.workspace_root().display().to_string(),
                            entries,
                            remote_bookmarks,
                            remotes,
                            all_tags,
                            tag_details,
                            bookmark_details,
                            workspace_entries,
                            warnings,
                        })),
                    },
                );
                tracing::info!(
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    commits = entry_count,
                    "revset loaded",
                );

                // Spawn background thread to compute is_empty for all commits.
                {
                    let empty_ids = all_ids.clone();
                    let inner = repo.inner_repo();
                    let tx = self.result_tx.clone();
                    let cancel = self.bg_cancel.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        for id in &empty_ids {
                            // is_empty involves tree diffs (I/O) — check every iteration.
                            if cancel.is_cancelled() {
                                return;
                            }
                            let Some(backend_id) =
                                jj_lib::backend::CommitId::try_from_hex(id.as_str())
                            else {
                                continue;
                            };
                            let Ok(commit) = inner.store().get_commit(&backend_id) else {
                                continue;
                            };
                            if commit
                                .is_empty(inner.as_ref())
                                .block_on()
                                .inspect_err(|e| tracing::warn!("is_empty check failed: {e}"))
                                .unwrap_or(false)
                            {
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
                    let cancel = self.bg_cancel.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        let updates = JjRepo::compute_divergence_info(&inner, &div_ids, &cancel);
                        if !updates.is_empty() {
                            let _ = tx.send(RepoResult::DivergenceInfo { updates });
                        }
                    });
                }

                // Spawn background thread to compute shortest unique ID prefixes.
                // The repo is already Arc-wrapped, so background threads share
                // it instead of re-opening the workspace.
                {
                    let bg_repo = Arc::clone(self.repo.as_ref().unwrap());
                    let tx = self.result_tx.clone();
                    let cancel = self.bg_cancel.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        if cancel.is_cancelled() {
                            return;
                        }
                        match bg_repo.compute_prefix_lengths(&all_ids, &cancel) {
                            Ok(updates) if !updates.is_empty() => {
                                let _ = tx.send(RepoResult::PrefixLengths { updates });
                            }
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: RepoError {
                                        kind: RepoErrorKind::Background,
                                        message: format!("prefix lengths: {e:#}"),
                                    },
                                });
                            }
                            _ => {}
                        }
                    });
                }

                // Spawn background thread for bookmark detail prefix lengths.
                if !detail_commit_ids.is_empty() {
                    let bg_repo = Arc::clone(self.repo.as_ref().unwrap());
                    let tx = self.result_tx.clone();
                    let cancel = self.bg_cancel.clone();
                    spawn_background(self.result_tx.clone(), move || {
                        if cancel.is_cancelled() {
                            return;
                        }
                        match bg_repo.compute_prefix_lengths(&detail_commit_ids, &cancel) {
                            Ok(updates) if !updates.is_empty() => {
                                let _ =
                                    tx.send(RepoResult::BookmarkDetailPrefixLengths { updates });
                            }
                            Err(e) => {
                                let _ = tx.send(RepoResult::BackgroundError {
                                    error: RepoError {
                                        kind: RepoErrorKind::Background,
                                        message: format!("detail prefix lengths: {e:#}"),
                                    },
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
                    RepoResult::Revset {
                        revset: effective_revset,
                        result: Err(RepoError {
                            kind: RepoErrorKind::Revset,
                            message: format!("{err:#}"),
                        }),
                    },
                );
            }
        }
    }

    fn handle_commit_details(&mut self, commit_id: CommitId) {
        self.repo_op(
            |repo| repo.commit_details(&commit_id),
            |result| RepoResult::CommitDetails {
                commit_id: commit_id.clone(),
                result,
            },
        );
    }

    fn handle_file_diff(
        &mut self,
        commit_id: CommitId,
        path: RepoPath,
        old_path: Option<RepoPath>,
    ) {
        self.repo_op(
            |repo| repo.file_diff(&commit_id, &path, old_path.as_ref()),
            |result| RepoResult::FileDiff {
                commit_id: commit_id.clone(),
                path: path.clone(),
                result,
            },
        );
    }

    fn handle_operations(&mut self, limit: usize) {
        self.repo_op(
            |repo| repo.operation_log(limit),
            |result| RepoResult::Operations { result },
        );
    }

    fn handle_conflict_hunks(&mut self, commit_id: CommitId, path: RepoPath) {
        self.repo_op(
            |repo| repo.conflict_hunks(&commit_id, &path),
            |result| RepoResult::ConflictHunks {
                commit_id: commit_id.clone(),
                path: path.clone(),
                result,
            },
        );
    }

    fn handle_op_diff(&mut self, op_id: OperationId) {
        self.repo_op(
            |repo| repo.op_diff(op_id.as_str()),
            |result| RepoResult::OpDiff {
                op_id: op_id.clone(),
                result,
            },
        );
    }

    fn handle_evolog_details(&mut self, from_commit_id: CommitId, to_commit_id: CommitId) {
        self.repo_op(
            |repo| repo.inter_commit_details(from_commit_id.as_str(), to_commit_id.as_str()),
            |result| RepoResult::EvoLogDetails {
                commit_id: to_commit_id.clone(),
                result,
            },
        );
    }

    fn handle_evolog_file_diff(
        &mut self,
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
    ) {
        self.repo_op(
            |repo| {
                repo.inter_commit_file_diff(from_commit_id.as_str(), to_commit_id.as_str(), &path)
            },
            |result| RepoResult::EvoLogFileDiff {
                commit_id: to_commit_id.clone(),
                path: path.clone(),
                result,
            },
        );
    }

    fn handle_interdiff_details(&mut self, from_commit_id: CommitId, to_commit_id: CommitId) {
        self.repo_op(
            |repo| repo.interdiff_details(from_commit_id.as_str(), to_commit_id.as_str()),
            |result| RepoResult::InterdiffDetails {
                from_commit_id: from_commit_id.clone(),
                to_commit_id: to_commit_id.clone(),
                result,
            },
        );
    }

    fn handle_interdiff_file_diff(
        &mut self,
        from_commit_id: CommitId,
        to_commit_id: CommitId,
        path: RepoPath,
    ) {
        self.repo_op(
            |repo| repo.interdiff_file_diff(from_commit_id.as_str(), to_commit_id.as_str(), &path),
            |result| RepoResult::InterdiffFileDiff {
                from_commit_id: from_commit_id.clone(),
                to_commit_id: to_commit_id.clone(),
                path: path.clone(),
                result,
            },
        );
    }

    fn handle_file_annotate(&mut self, commit_id: CommitId, path: RepoPath) {
        self.repo_op(
            |repo| repo.file_annotate(&commit_id, &path),
            |result| RepoResult::Annotate {
                commit_id: commit_id.clone(),
                path: path.clone(),
                result,
            },
        );
    }

    fn handle_file_list(&mut self, commit_id: CommitId) {
        self.repo_op(
            |repo| repo.list_files(&commit_id),
            |result| RepoResult::FileList {
                commit_id: commit_id.clone(),
                result,
            },
        );
    }

    fn handle_evolution_log(&mut self, commit_id: CommitId) {
        self.repo_op(
            |repo| repo.evolution_log(commit_id.as_str()),
            |result| RepoResult::EvoLog {
                commit_id: commit_id.clone(),
                result,
            },
        );
    }

    /// Ensure the repo is open, opening it if needed. On failure, sends the
    /// error result produced by `on_error` and returns `None`.
    fn ensure_repo(&mut self, on_error: impl FnOnce(RepoError) -> RepoResult) -> Option<&JjRepo> {
        if self.repo.is_none() {
            match JjRepo::open(&self.repo_path) {
                Ok(repo) => self.repo = Some(Arc::new(repo)),
                Err(err) => {
                    let _ = self.result_tx.send(on_error(RepoError {
                        kind: RepoErrorKind::RepoOpen,
                        message: format!("{err:#}"),
                    }));
                    return None;
                }
            }
        }
        self.repo.as_deref()
    }

    /// Send a revset result only if no newer revset load has been requested.
    /// Keyed results (diffs, details, annotate, …) are sent unconditionally:
    /// they are content-addressed, so the app can apply or ignore them by key,
    /// and dropping them would strand `Loading` placeholders forever.
    fn send_if_current(&self, epoch: u64, result: RepoResult) {
        if epoch == self.current_epoch.load(Ordering::SeqCst) {
            let _ = self.result_tx.send(result);
        }
    }

    /// Ensure repo is loaded, call a fallible operation, and send the result.
    /// `wrap` converts `Result<T, RepoError>` into the appropriate `RepoResult` variant.
    fn repo_op<T>(
        &mut self,
        op: impl FnOnce(&crate::repo::JjRepo) -> color_eyre::Result<T>,
        wrap: impl Fn(Result<T, RepoError>) -> RepoResult,
    ) {
        let Some(repo) = self.ensure_repo(|e| wrap(Err(e))) else {
            return;
        };
        let result =
            op(repo).map_err(|err| RepoError::new(RepoErrorKind::Operation, format!("{err:#}")));
        let _ = self.result_tx.send(wrap(result));
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
            let _ = err_tx.send(RepoResult::BackgroundError {
                error: RepoError {
                    kind: RepoErrorKind::Background,
                    message: msg,
                },
            });
        }
    });
}
