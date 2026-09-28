use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::dag::{DagEntry, DiffSummary, DiffTarget, DivergenceInfo, PrefixLengthUpdate};
use crate::repo::{JjRepo, SnapshotError};
use crate::types::{BookmarkName, CommitId, OperationId, RemoteName, RepoPath, TagName};

pub struct RepoService;

#[derive(Clone)]
pub struct RepoRequestHandle {
    request_tx: Sender<Envelope>,
    current_epoch: Arc<AtomicU64>,
}

pub struct RepoResponseHandle {
    result_rx: Receiver<RepoResult>,
}

/// Whether a revset load should snapshot the working copy first.
#[derive(Clone, Copy, Debug)]
pub enum RevsetLoadKind {
    /// Snapshot first: use after actions that may have changed the filesystem.
    Snapshot,
    /// Skip the snapshot: use for pure revset/UI changes.
    NoSnapshot,
}

/// Work for the repository service. Results come back as [`RepoResult`]s
/// carrying the request's key.
#[derive(Clone)]
pub enum RepoRequest {
    Revset {
        revset: Option<String>,
        load_kind: RevsetLoadKind,
    },
    /// The changed files of a diff target.
    DiffSummary {
        target: DiffTarget,
    },
    /// One file's diff within a diff target. `old_path` is the source of a
    /// rename or copy.
    FileDiff {
        target: DiffTarget,
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
    Annotate {
        commit_id: CommitId,
        path: RepoPath,
    },
    FileList {
        commit_id: CommitId,
    },
    /// Replace the diff size limit, after the config was reloaded. Queued
    /// behind whatever is in flight, so a diff already being computed keeps
    /// the limit it started under.
    SetDiffSizeLimit {
        bytes: usize,
    },
}

impl RepoRequest {
    fn label(&self) -> &'static str {
        match self {
            Self::Revset { .. } => "revset",
            Self::DiffSummary { .. } => "diff_summary",
            Self::FileDiff { .. } => "file_diff",
            Self::Operations { .. } => "operations",
            Self::ConflictHunks { .. } => "conflict_hunks",
            Self::OpDiff { .. } => "op_diff",
            Self::EvolutionLog { .. } => "evolution_log",
            Self::Annotate { .. } => "annotate",
            Self::FileList { .. } => "file_list",
            Self::SetDiffSizeLimit { .. } => "set_diff_size_limit",
        }
    }
}

/// A request stamped with the revset epoch current when it was sent.
struct Envelope {
    epoch: u64,
    request: RepoRequest,
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
    /// Whether this is the complete revset. When `false`, the remaining
    /// entries follow as [`RepoResult::RevsetChunk`]s.
    pub done: bool,
}

pub enum RepoResult {
    Revset {
        revset: String,
        result: Result<Box<RevsetData>, RepoError>,
    },
    /// A follow-up batch of entries for a streamed revset load.
    RevsetChunk { entries: Vec<DagEntry>, done: bool },
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
    DiffSummary {
        target: DiffTarget,
        result: Result<DiffSummary, RepoError>,
    },
    FileDiff {
        target: DiffTarget,
        path: RepoPath,
        result: Result<crate::dag::DiffResult, RepoError>,
    },
    /// Background-computed prefix lengths for bookmark detail commits
    /// (conflict targets, remote tracking targets not in the DAG).
    BookmarkDetailPrefixLengths {
        updates: Vec<(CommitId, PrefixLengthUpdate)>,
    },
    Operations {
        limit: usize,
        result: Result<(Vec<crate::app::OpLogEntry>, bool), RepoError>,
    },
    ConflictHunks {
        commit_id: CommitId,
        path: RepoPath,
        result: Result<Vec<crate::conflict::ConflictHunkKind>, RepoError>,
    },
    OpDiff {
        op_id: OperationId,
        result: Result<Vec<crate::app::OpDetailLine>, RepoError>,
    },
    EvoLog {
        commit_id: CommitId,
        result: Result<Vec<crate::app::EvoLogEntry>, RepoError>,
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

impl RepoService {
    pub fn spawn(
        repo_path: PathBuf,
        diff_size_limit: usize,
    ) -> (RepoRequestHandle, RepoResponseHandle) {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let current_epoch = Arc::new(AtomicU64::new(0));
        let request_handle = RepoRequestHandle {
            request_tx,
            current_epoch: Arc::clone(&current_epoch),
        };
        let response_handle = RepoResponseHandle { result_rx };

        thread::spawn(move || {
            let mut service =
                RepoServiceState::new(repo_path, diff_size_limit, result_tx, current_epoch);
            service.run(request_rx);
        });

        (request_handle, response_handle)
    }
}

impl RepoRequestHandle {
    pub fn send(&self, request: RepoRequest) {
        // Each revset load starts a new epoch; a queued revset load whose
        // epoch is no longer current has been superseded and is skipped.
        // Other requests deliver keyed results and don't need an epoch.
        let epoch = match request {
            RepoRequest::Revset { .. } => self.current_epoch.fetch_add(1, Ordering::SeqCst) + 1,
            _ => self.current_epoch.load(Ordering::SeqCst),
        };
        let _ = self.request_tx.send(Envelope { epoch, request });
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
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<std::sync::atomic::AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
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
    /// Max bytes of file content (per side) materialized for a diff,
    /// from the config file.
    diff_size_limit: usize,
    result_tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    /// Cancellation token for background threads spawned by the current revset.
    bg_cancel: CancellationToken,
    /// Workers executing per-request repo operations concurrently.
    workers: WorkerPool,
}

impl RepoServiceState {
    fn new(
        repo_path: PathBuf,
        diff_size_limit: usize,
        result_tx: Sender<RepoResult>,
        current_epoch: Arc<AtomicU64>,
    ) -> Self {
        let worker_count = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        Self {
            repo_path,
            repo: None,
            diff_size_limit,
            workers: WorkerPool::new(worker_count, result_tx.clone()),
            result_tx,
            current_epoch,
            bg_cancel: CancellationToken::new(),
        }
    }

    fn run(&mut self, request_rx: Receiver<Envelope>) {
        let mut pending: VecDeque<Envelope> = VecDeque::new();
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
                .position(|e| matches!(e.request, RepoRequest::Revset { .. }))
                .and_then(|i| pending.remove(i))
                .or_else(|| pending.pop_front())
                .expect("pending is non-empty");
            self.handle_request(request);
        }
    }

    fn handle_request(&mut self, envelope: Envelope) {
        let Envelope { epoch, request } = envelope;
        tracing::debug!(epoch, request = %request.label(), "repo request");
        match request {
            RepoRequest::Revset { revset, load_kind } => {
                // A newer revset load has been requested since this one was
                // queued: skip the snapshot and evaluation entirely.
                if epoch != self.current_epoch.load(Ordering::SeqCst) {
                    tracing::debug!(epoch, "skipping superseded revset load");
                    return;
                }
                self.bg_cancel.cancel();
                self.bg_cancel = CancellationToken::new();
                self.handle_revset(epoch, revset, load_kind);
            }
            RepoRequest::DiffSummary { target } => {
                self.handle_diff_summary(target);
            }
            RepoRequest::FileDiff {
                target,
                path,
                old_path,
            } => {
                self.handle_file_diff(target, path, old_path);
            }
            RepoRequest::Operations { limit } => {
                self.handle_operations(limit);
            }
            RepoRequest::ConflictHunks { commit_id, path } => {
                self.handle_conflict_hunks(commit_id, path);
            }
            RepoRequest::OpDiff { op_id } => {
                self.handle_op_diff(op_id);
            }
            RepoRequest::EvolutionLog { commit_id } => {
                self.handle_evolution_log(commit_id);
            }
            RepoRequest::Annotate { commit_id, path } => {
                self.handle_file_annotate(commit_id, path);
            }
            RepoRequest::FileList { commit_id } => {
                self.handle_file_list(commit_id);
            }
            RepoRequest::SetDiffSizeLimit { bytes } => {
                self.diff_size_limit = bytes;
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

        if matches!(load_kind, RevsetLoadKind::Snapshot)
            && let Err(err) = JjRepo::snapshot(&self.repo_path)
            && matches!(err, SnapshotError::Stale(_))
        {
            match JjRepo::update_stale(&self.repo_path) {
                Ok(()) => {
                    let _ = self.result_tx.send(RepoResult::WorkspaceUpdatedStale {
                        message: "workspace was stale: updated".to_string(),
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
        match JjRepo::open(&self.repo_path) {
            Ok(mut repo) => {
                repo.set_diff_size_limit(self.diff_size_limit);
                self.repo = Some(Arc::new(repo));
            }
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

        let Some(repo) = self.repo.as_ref().map(Arc::clone) else {
            return;
        };
        let effective_revset = revset.unwrap_or_else(|| repo.default_revset());
        // Evaluate on a background thread so the request queue stays
        // responsive; entries stream to the app in chunks.
        let tx = self.result_tx.clone();
        let current_epoch = Arc::clone(&self.current_epoch);
        let cancel = self.bg_cancel.clone();
        spawn_background(self.result_tx.clone(), move || {
            stream_revset(
                repo,
                effective_revset,
                epoch,
                tx,
                current_epoch,
                cancel,
                start,
            );
        });
    }

    fn handle_diff_summary(&mut self, target: DiffTarget) {
        self.repo_op(
            {
                let target = target.clone();
                move |repo| repo.diff_summary(&target)
            },
            move |result| RepoResult::DiffSummary { target, result },
        );
    }

    fn handle_file_diff(&mut self, target: DiffTarget, path: RepoPath, old_path: Option<RepoPath>) {
        self.repo_op(
            {
                let target = target.clone();
                let path = path.clone();
                move |repo| repo.file_diff(&target, &path, old_path.as_ref())
            },
            move |result| RepoResult::FileDiff {
                target,
                path,
                result,
            },
        );
    }

    fn handle_operations(&mut self, limit: usize) {
        self.repo_op(
            move |repo| repo.operation_log(limit),
            move |result| RepoResult::Operations { limit, result },
        );
    }

    fn handle_conflict_hunks(&mut self, commit_id: CommitId, path: RepoPath) {
        self.repo_op(
            {
                let commit_id = commit_id.clone();
                let path = path.clone();
                move |repo| repo.conflict_hunks(&commit_id, &path)
            },
            move |result| RepoResult::ConflictHunks {
                commit_id,
                path,
                result,
            },
        );
    }

    fn handle_op_diff(&mut self, op_id: OperationId) {
        self.repo_op(
            {
                let op_id = op_id.clone();
                move |repo| repo.op_diff(op_id.as_str())
            },
            move |result| RepoResult::OpDiff { op_id, result },
        );
    }

    fn handle_file_annotate(&mut self, commit_id: CommitId, path: RepoPath) {
        self.repo_op(
            {
                let commit_id = commit_id.clone();
                let path = path.clone();
                move |repo| repo.file_annotate(&commit_id, &path)
            },
            move |result| RepoResult::Annotate {
                commit_id,
                path,
                result,
            },
        );
    }

    fn handle_file_list(&mut self, commit_id: CommitId) {
        self.repo_op(
            {
                let commit_id = commit_id.clone();
                move |repo| repo.list_files(&commit_id)
            },
            move |result| RepoResult::FileList { commit_id, result },
        );
    }

    fn handle_evolution_log(&mut self, commit_id: CommitId) {
        self.repo_op(
            {
                let commit_id = commit_id.clone();
                move |repo| repo.evolution_log(&commit_id)
            },
            move |result| RepoResult::EvoLog { commit_id, result },
        );
    }

    /// Ensure the repo is open, opening it if needed.
    fn ensure_repo(&mut self) -> Result<Arc<JjRepo>, RepoError> {
        if self.repo.is_none() {
            match JjRepo::open(&self.repo_path) {
                Ok(mut repo) => {
                    repo.set_diff_size_limit(self.diff_size_limit);
                    self.repo = Some(Arc::new(repo));
                }
                Err(err) => {
                    return Err(RepoError::new(RepoErrorKind::RepoOpen, format!("{err:#}")));
                }
            }
        }
        Ok(Arc::clone(
            self.repo.as_ref().expect("repo was just opened"),
        ))
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

    /// Ensure the repo is loaded, then run a fallible operation on a worker
    /// thread and send the result. `wrap` converts `Result<T, RepoError>`
    /// into the appropriate `RepoResult` variant.
    fn repo_op<T: Send + 'static>(
        &mut self,
        op: impl FnOnce(&crate::repo::JjRepo) -> color_eyre::Result<T> + Send + 'static,
        wrap: impl FnOnce(Result<T, RepoError>) -> RepoResult + Send + 'static,
    ) {
        let tx = self.result_tx.clone();
        match self.ensure_repo() {
            Ok(repo) => self.workers.submit(move || {
                let result = op(&repo)
                    .map_err(|err| RepoError::new(RepoErrorKind::Operation, format!("{err:#}")));
                let _ = tx.send(wrap(result));
            }),
            Err(e) => {
                let _ = tx.send(wrap(Err(e)));
            }
        }
    }
}

/// Evaluate a revset and stream its entries to the app. The first chunk is
/// sent as a full [`RepoResult::Revset`] (with view metadata), the rest as
/// [`RepoResult::RevsetChunk`]s. After the stream completes, background
/// passes (is_empty, divergence, ID prefixes) are spawned over all entries.
///
/// Runs on its own background thread; a newer revset load supersedes it via
/// the epoch counter and the cancellation token.
fn stream_revset(
    repo: Arc<JjRepo>,
    revset: String,
    epoch: u64,
    tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    cancel: CancellationToken,
    start: std::time::Instant,
) {
    let send_if_current = |result: RepoResult| -> bool {
        epoch == current_epoch.load(Ordering::SeqCst) && tx.send(result).is_ok()
    };

    let mut all_ids: Vec<CommitId> = Vec::new();
    let mut first = true;
    let stream_result = repo.evaluate_revset_streaming(&revset, |entries, done, warnings| {
        if cancel.is_cancelled() {
            return false;
        }
        all_ids.extend(entries.iter().map(|e| e.commit.graph_id.clone()));
        if !first {
            return send_if_current(RepoResult::RevsetChunk { entries, done });
        }
        first = false;

        // Gather view metadata once, alongside the first chunk.
        let remote_bookmarks = repo.all_remote_bookmark_refs();
        let remotes = repo.git_remotes();
        let all_tags = repo.all_local_tags();
        let tag_details = repo.extract_tag_details();
        let bookmark_details = repo.extract_bookmark_details();
        let workspace_entries = repo.workspace_entries();

        // Collect unique commit IDs from bookmark + tag details + workspaces,
        // and spawn their prefix computation (independent of the stream).
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
        if !detail_commit_ids.is_empty() {
            let bg_repo = Arc::clone(&repo);
            let tx = tx.clone();
            let cancel = cancel.clone();
            spawn_background(tx.clone(), move || {
                if cancel.is_cancelled() {
                    return;
                }
                match bg_repo.compute_prefix_lengths(&detail_commit_ids, &cancel) {
                    Ok(updates) if !updates.is_empty() => {
                        let _ = tx.send(RepoResult::BookmarkDetailPrefixLengths { updates });
                    }
                    Err(e) => {
                        let _ = tx.send(RepoResult::BackgroundError {
                            error: RepoError::new(
                                RepoErrorKind::Background,
                                format!("detail prefix lengths: {e:#}"),
                            ),
                        });
                    }
                    _ => {}
                }
            });
        }

        send_if_current(RepoResult::Revset {
            revset: revset.clone(),
            result: Ok(Box::new(RevsetData {
                revset: revset.clone(),
                repo_root: repo.workspace_root().display().to_string(),
                entries,
                remote_bookmarks,
                remotes,
                all_tags,
                tag_details,
                bookmark_details,
                workspace_entries,
                warnings: warnings.to_vec(),
                done,
            })),
        })
    });

    match stream_result {
        Ok(_) => {}
        Err(err) if first => {
            // Nothing was sent: report as a failed revset load.
            send_if_current(RepoResult::Revset {
                revset,
                result: Err(RepoError::new(RepoErrorKind::Revset, format!("{err:#}"))),
            });
            return;
        }
        Err(err) => {
            // Entries were already delivered: finalize the stream so the
            // app leaves streaming mode, and surface the error separately.
            send_if_current(RepoResult::RevsetChunk {
                entries: Vec::new(),
                done: true,
            });
            let _ = tx.send(RepoResult::BackgroundError {
                error: RepoError::new(RepoErrorKind::Background, format!("revset stream: {err:#}")),
            });
        }
    }
    // Superseded or cancelled mid-stream: skip the background passes.
    if first || cancel.is_cancelled() || epoch != current_epoch.load(Ordering::SeqCst) {
        return;
    }

    tracing::info!(
        elapsed_ms = start.elapsed().as_millis() as u64,
        commits = all_ids.len(),
        "revset loaded",
    );

    // Spawn background thread to compute is_empty for all commits.
    {
        let empty_ids = all_ids.clone();
        let inner = repo.inner_repo();
        let tx = tx.clone();
        let cancel = cancel.clone();
        spawn_background(tx.clone(), move || {
            for id in &empty_ids {
                // is_empty involves tree diffs (I/O), so check every iteration.
                if cancel.is_cancelled() {
                    return;
                }
                let Some(backend_id) = jj_lib::backend::CommitId::try_from_hex(id.as_str()) else {
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
        let tx = tx.clone();
        let cancel = cancel.clone();
        spawn_background(tx.clone(), move || {
            let updates = JjRepo::compute_divergence_info(&inner, &div_ids, &cancel);
            if !updates.is_empty() {
                let _ = tx.send(RepoResult::DivergenceInfo { updates });
            }
        });
    }

    // Spawn background thread to compute shortest unique ID prefixes.
    {
        let bg_repo = Arc::clone(&repo);
        spawn_background(tx.clone(), move || {
            if cancel.is_cancelled() {
                return;
            }
            match bg_repo.compute_prefix_lengths(&all_ids, &cancel) {
                Ok(updates) if !updates.is_empty() => {
                    let _ = tx.send(RepoResult::PrefixLengths { updates });
                }
                Err(e) => {
                    let _ = tx.send(RepoResult::BackgroundError {
                        error: RepoError::new(
                            RepoErrorKind::Background,
                            format!("prefix lengths: {e:#}"),
                        ),
                    });
                }
                _ => {}
            }
        });
    }
}

/// Spawn a background thread with a panic handler that reports errors
/// instead of silently dying.
fn spawn_background(err_tx: Sender<RepoResult>, f: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Err(e) = result {
            let _ = err_tx.send(RepoResult::BackgroundError {
                error: RepoError::new(RepoErrorKind::Background, panic_message(e)),
            });
        }
    });
}

fn panic_message(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// Fixed pool of worker threads running per-request repo operations (diffs,
/// annotate, op log, …) so one slow operation occupies a worker instead of
/// stalling the request queue. Results are keyed, so they may complete and
/// be delivered in any order.
struct WorkerPool {
    job_tx: Sender<Box<dyn FnOnce() + Send>>,
}

impl WorkerPool {
    fn new(threads: usize, err_tx: Sender<RepoResult>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let job_rx = Arc::new(std::sync::Mutex::new(job_rx));
        for _ in 0..threads {
            let job_rx = Arc::clone(&job_rx);
            let err_tx = err_tx.clone();
            thread::spawn(move || {
                loop {
                    // The lock is held only while waiting for a job, so pickup
                    // is serialized but execution is parallel.
                    let job = match job_rx.lock().unwrap().recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    };
                    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
                        let _ = err_tx.send(RepoResult::BackgroundError {
                            error: RepoError::new(RepoErrorKind::Background, panic_message(e)),
                        });
                    }
                }
            });
        }
        Self { job_tx }
    }

    fn submit(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.job_tx.send(Box::new(job));
    }
}
