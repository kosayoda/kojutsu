use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use jj_lib::repo::Repo as _;

use crate::dag::{DagEntry, DiffLine, FileChange, LineStats};
use crate::repo::JjRepo;
use crate::types::CommitId;

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
    Revset { revset: Option<String> },
    Commit { commit_id: CommitId },
    FileDiff { commit_id: CommitId, path: String },
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
        untracked_bookmarks: Vec<String>,
        tracked_bookmarks: Vec<String>,
    },
    RevsetFailed {
        revset: String,
        error: String,
    },
    /// The workspace was stale and has been (or failed to be) recovered.
    WorkspaceUpdatedStale {
        message: String,
    },
    /// Background-computed is_empty for a single merge commit.
    CommitEmpty {
        commit_id: CommitId,
    },
    CommitDetailsLoaded {
        commit_id: CommitId,
        files: Vec<FileChange>,
        stats: LineStats,
        is_empty: bool,
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

    pub fn load_file_diff(commit_id: CommitId, path: String) -> Self {
        Self {
            epoch: 0,
            kind: RepoRequestKind::FileDiff { commit_id, path },
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
            RepoRequestKind::Commit { .. } | RepoRequestKind::FileDiff { .. } => {
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
    repo: Option<JjRepo>,
    result_tx: Sender<RepoResult>,
    current_epoch: Arc<AtomicU64>,
    in_flight_commit_details: HashSet<CommitId>,
    in_flight_file_diffs: HashSet<(CommitId, String)>,
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
            RepoRequestKind::FileDiff { commit_id, path } => {
                self.handle_file_diff(epoch, commit_id, path);
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

        let repo = self.repo.as_ref().unwrap();
        let effective_revset = revset.unwrap_or_else(|| repo.default_revset());
        match repo.evaluate_revset(&effective_revset) {
            Ok(entries) => {
                // Collect merge commit IDs for background is_empty computation.
                let merge_ids: Vec<String> = entries
                    .iter()
                    .filter(|e| e.commit.is_merge)
                    .map(|e| e.commit.graph_id.as_str().to_string())
                    .collect();

                let untracked_bookmarks = repo.untracked_remote_bookmarks();
                let tracked_bookmarks = repo.tracked_remote_bookmarks();

                self.send_if_current(
                    epoch,
                    RepoResult::RevsetLoaded {
                        revset: effective_revset,
                        repo_root: repo.workspace_root().display().to_string(),
                        entries,
                        untracked_bookmarks,
                        tracked_bookmarks,
                    },
                );

                // Spawn background thread to compute is_empty for merge commits.
                if !merge_ids.is_empty() {
                    let inner = repo.inner_repo();
                    let empty_tx = self.result_tx.clone();
                    thread::spawn(move || {
                        for hex_id in &merge_ids {
                            let Some(backend_id) = jj_lib::backend::CommitId::try_from_hex(hex_id)
                            else {
                                continue;
                            };
                            let Ok(commit) = inner.store().get_commit(&backend_id) else {
                                continue;
                            };
                            if commit.is_empty(inner.as_ref()).unwrap_or(false) {
                                let _ = empty_tx.send(RepoResult::CommitEmpty {
                                    commit_id: CommitId::new(hex_id.clone()),
                                });
                            }
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

        match repo.commit_details(commit_id.as_str()) {
            Ok((files, stats, is_empty)) => self.send_if_current(
                epoch,
                RepoResult::CommitDetailsLoaded {
                    commit_id: commit_id.clone(),
                    files,
                    stats,
                    is_empty,
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

    fn handle_file_diff(&mut self, epoch: u64, commit_id: CommitId, path: String) {
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

        match repo.file_diff(commit_id.as_str(), &path) {
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

    fn send_if_current(&self, epoch: u64, result: RepoResult) {
        if epoch == self.current_epoch.load(Ordering::SeqCst) {
            let _ = self.result_tx.send(result);
        }
    }
}
