use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self as std_mpsc, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;
use tokio::task::{spawn_local, JoinHandle, LocalSet};

use crate::dag::{DagEntry, DiffLine, FileChange, LineStats};
use crate::repo::JjRepo;
use crate::types::CommitId;

pub struct RepoService;

#[derive(Clone)]
pub struct RepoRequestHandle {
    request_tx: UnboundedSender<RepoRequest>,
    current_epoch: Arc<AtomicU64>,
}

pub struct RepoResponseHandle {
    result_rx: Receiver<RepoResult>,
}

#[derive(Clone)]
enum RepoRequestKind {
    LoadRevset {
        revset: Option<String>,
        refresh: bool,
    },
    LoadCommitDetails {
        commit_id: CommitId,
    },
    LoadFileDiff {
        commit_id: CommitId,
        path: String,
    },
}

#[derive(Clone)]
pub struct RepoRequest {
    epoch: u64,
    kind: RepoRequestKind,
}

pub enum RepoResult {
    RevsetLoaded {
        revset: String,
        repo_root: String,
        entries: Vec<DagEntry>,
    },
    RevsetFailed {
        revset: String,
        error: String,
    },
    /// The workspace was stale and has been (or failed to be) recovered.
    WorkspaceUpdatedStale {
        message: String,
    },
    CommitDetailsLoaded {
        commit_id: CommitId,
        files: Vec<FileChange>,
        stats: LineStats,
    },
    CommitDetailsFailed {
        commit_id: CommitId,
        error: String,
    },
    FileDiffLoaded {
        commit_id: CommitId,
        path: String,
        lines: Vec<DiffLine>,
    },
    FileDiffFailed {
        commit_id: CommitId,
        path: String,
        error: String,
    },
}

impl RepoRequest {
    pub fn load_revset(revset: Option<String>, refresh: bool) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::LoadRevset { revset, refresh },
        }
    }

    pub fn load_commit_details(commit_id: CommitId) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::LoadCommitDetails { commit_id },
        }
    }

    pub fn load_file_diff(commit_id: CommitId, path: String) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::LoadFileDiff { commit_id, path },
        }
    }
}

impl RepoService {
    pub fn spawn(repo_path: PathBuf) -> (RepoRequestHandle, RepoResponseHandle) {
        let (request_tx, request_rx) = unbounded_channel();
        let (result_tx, result_rx) = std_mpsc::channel();
        let current_epoch = Arc::new(AtomicU64::new(0));
        let request_handle = RepoRequestHandle {
            request_tx,
            current_epoch: Arc::clone(&current_epoch),
        };
        let response_handle = RepoResponseHandle { result_rx };

        thread::spawn(move || {
            let runtime = build_runtime();
            let local = LocalSet::new();
            let service = RepoServiceState::new(repo_path, result_tx, current_epoch);
            local.block_on(&runtime, service.run(request_rx));
        });

        (request_handle, response_handle)
    }
}

impl RepoRequestHandle {
    pub fn send(&self, mut request: RepoRequest) {
        request.epoch = match request.kind {
            RepoRequestKind::LoadRevset { .. } => {
                self.current_epoch.fetch_add(1, Ordering::SeqCst) + 1
            }
            RepoRequestKind::LoadCommitDetails { .. } | RepoRequestKind::LoadFileDiff { .. } => {
                self.current_epoch.load(Ordering::SeqCst)
            }
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
    repo: Rc<RefCell<Option<Rc<JjRepo>>>>,
    result_tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    revset_task: Option<JoinHandle<()>>,
    detail_tasks: Vec<JoinHandle<()>>,
    in_flight_commit_details: Rc<RefCell<HashSet<CommitId>>>,
    in_flight_file_diffs: Rc<RefCell<HashSet<(CommitId, String)>>>,
    detail_semaphore: Arc<Semaphore>,
}

impl RepoServiceState {
    fn new(
        repo_path: PathBuf,
        result_tx: Sender<RepoResult>,
        current_epoch: Arc<AtomicU64>,
    ) -> Self {
        Self {
            repo_path,
            repo: Rc::new(RefCell::new(None)),
            result_tx,
            current_epoch,
            revset_task: None,
            detail_tasks: Vec::new(),
            in_flight_commit_details: Rc::new(RefCell::new(HashSet::new())),
            in_flight_file_diffs: Rc::new(RefCell::new(HashSet::new())),
            detail_semaphore: Arc::new(Semaphore::new(4)),
        }
    }

    async fn run(mut self, mut request_rx: UnboundedReceiver<RepoRequest>) {
        while let Some(request) = request_rx.recv().await {
            self.detail_tasks.retain(|task| !task.is_finished());
            self.handle_request(request);
        }

        self.abort_revset_task();
        self.abort_detail_tasks();
    }

    fn handle_request(&mut self, request: RepoRequest) {
        let RepoRequest { epoch, kind } = request;
        match kind {
            RepoRequestKind::LoadRevset { revset, refresh } => {
                self.spawn_revset_task(epoch, revset, refresh)
            }
            RepoRequestKind::LoadCommitDetails { commit_id } => {
                self.spawn_commit_details_task(epoch, commit_id)
            }
            RepoRequestKind::LoadFileDiff { commit_id, path } => {
                self.spawn_file_diff_task(epoch, commit_id, path)
            }
        }
    }

    fn spawn_revset_task(&mut self, epoch: u64, revset: Option<String>, refresh: bool) {
        self.abort_revset_task();
        self.abort_detail_tasks();

        let repo_path = self.repo_path.clone();
        let repo = Rc::clone(&self.repo);
        let result_tx = self.result_tx.clone();
        let current_epoch = Arc::clone(&self.current_epoch);

        self.revset_task = Some(spawn_local(async move {
            let requested_revset = revset.clone().unwrap_or_default();

            let active_repo = if refresh || repo.borrow().is_none() {
                if let Err(err) = JjRepo::snapshot(&repo_path) {
                    if err.contains("stale") {
                        match JjRepo::update_stale(&repo_path) {
                            Ok(()) => {
                                let _ = result_tx.send(RepoResult::WorkspaceUpdatedStale {
                                    message: "workspace was stale — updated".to_string(),
                                });
                                // Retry snapshot after update-stale.
                                let _ = JjRepo::snapshot(&repo_path);
                            }
                            Err(update_err) => {
                                send_if_current(
                                    &result_tx,
                                    current_epoch.as_ref(),
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
                    // Non-stale snapshot errors are non-fatal (e.g. no working copy).
                }
                match JjRepo::open_async(&repo_path).await {
                    Ok(opened_repo) => {
                        let opened_repo = Rc::new(opened_repo);
                        repo.replace(Some(Rc::clone(&opened_repo)));
                        opened_repo
                    }
                    Err(err) => {
                        send_if_current(
                            &result_tx,
                            current_epoch.as_ref(),
                            epoch,
                            RepoResult::RevsetFailed {
                                revset: requested_revset,
                                error: format!("{err:#}"),
                            },
                        );
                        return;
                    }
                }
            } else {
                repo.borrow()
                    .as_ref()
                    .cloned()
                    .expect("repo should be loaded")
            };

            let effective_revset = revset.unwrap_or_else(|| active_repo.default_revset());
            match active_repo.evaluate_revset(&effective_revset) {
                Ok(entries) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::RevsetLoaded {
                        revset: effective_revset,
                        repo_root: active_repo.workspace_root().display().to_string(),
                        entries,
                    },
                ),
                Err(err) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::RevsetFailed {
                        revset: effective_revset,
                        error: format!("{err:#}"),
                    },
                ),
            }
        }));
    }

    fn spawn_commit_details_task(&mut self, epoch: u64, commit_id: CommitId) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        if !self
            .in_flight_commit_details
            .borrow_mut()
            .insert(commit_id.clone())
        {
            return;
        }
        let Some(active_repo) = self.repo.borrow().as_ref().cloned() else {
            self.in_flight_commit_details
                .borrow_mut()
                .remove(&commit_id);
            self.send_if_current(
                epoch,
                RepoResult::CommitDetailsFailed {
                    commit_id,
                    error: "repository not loaded yet".to_string(),
                },
            );
            return;
        };

        let result_tx = self.result_tx.clone();
        let current_epoch = Arc::clone(&self.current_epoch);
        let in_flight = Rc::clone(&self.in_flight_commit_details);
        let detail_semaphore = Arc::clone(&self.detail_semaphore);
        self.detail_tasks.push(spawn_local(async move {
            let _permit = detail_semaphore
                .acquire_owned()
                .await
                .expect("detail semaphore closed");
            match active_repo.commit_details(commit_id.as_str()).await {
                Ok((files, stats)) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::CommitDetailsLoaded {
                        commit_id: commit_id.clone(),
                        files,
                        stats,
                    },
                ),
                Err(err) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::CommitDetailsFailed {
                        commit_id: commit_id.clone(),
                        error: format!("{err:#}"),
                    },
                ),
            }
            in_flight.borrow_mut().remove(&commit_id);
        }));
    }

    fn spawn_file_diff_task(&mut self, epoch: u64, commit_id: CommitId, path: String) {
        if epoch != self.current_epoch.load(Ordering::SeqCst) {
            return;
        }
        let key = (commit_id.clone(), path.clone());
        if !self.in_flight_file_diffs.borrow_mut().insert(key.clone()) {
            return;
        }
        let Some(active_repo) = self.repo.borrow().as_ref().cloned() else {
            self.in_flight_file_diffs.borrow_mut().remove(&key);
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

        let result_tx = self.result_tx.clone();
        let current_epoch = Arc::clone(&self.current_epoch);
        let in_flight = Rc::clone(&self.in_flight_file_diffs);
        let detail_semaphore = Arc::clone(&self.detail_semaphore);
        self.detail_tasks.push(spawn_local(async move {
            let _permit = detail_semaphore
                .acquire_owned()
                .await
                .expect("detail semaphore closed");
            match active_repo.file_diff(commit_id.as_str(), &path).await {
                Ok(lines) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::FileDiffLoaded {
                        commit_id: commit_id.clone(),
                        path: path.clone(),
                        lines,
                    },
                ),
                Err(err) => send_if_current(
                    &result_tx,
                    current_epoch.as_ref(),
                    epoch,
                    RepoResult::FileDiffFailed {
                        commit_id: commit_id.clone(),
                        path: path.clone(),
                        error: format!("{err:#}"),
                    },
                ),
            }
            in_flight.borrow_mut().remove(&key);
        }));
    }

    fn send_if_current(&self, epoch: u64, result: RepoResult) {
        send_if_current(&self.result_tx, self.current_epoch.as_ref(), epoch, result);
    }

    fn abort_revset_task(&mut self) {
        if let Some(handle) = self.revset_task.take() {
            handle.abort();
        }
    }

    fn abort_detail_tasks(&mut self) {
        for handle in self.detail_tasks.drain(..) {
            handle.abort();
        }
        self.in_flight_commit_details.borrow_mut().clear();
        self.in_flight_file_diffs.borrow_mut().clear();
    }
}

fn build_runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
}

fn send_if_current(
    result_tx: &Sender<RepoResult>,
    current_epoch: &AtomicU64,
    epoch: u64,
    result: RepoResult,
) {
    if epoch == current_epoch.load(Ordering::SeqCst) {
        let _ = result_tx.send(result);
    }
}
