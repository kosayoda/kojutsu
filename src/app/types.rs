use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use tui_input::Input;

use crate::idx::{EntryIdx, RowIdx};
use crate::types::{ActiveView, DisplayRow};

use std::sync::Arc;

use compact_str::CompactString;

use crate::dag::WorkspaceInfo;
use crate::history::{EvoLogEntry, OpDetailLine, OpLogEntry};
use crate::keymap::{CommandFlags, TrieNode};
use crate::types::{
    BookmarkName, CommitId, FollowUpOption, OperationId, PendingCommitSelect, PendingSelection,
    RemoteName, RepoPath, RevisionArg, SearchScopes, SmallVec1, Str, TagName, TargetOperation,
    TextPrompt, VisualRange, WorkspaceName,
};

use super::{FileTree, Loadable};

/// Visual selection state.
pub struct VisualState {
    /// Active visual selection mode (line or commit).
    pub mode: Option<VisualMode>,
    /// Persistent visual range (survives exiting visual mode with `v`).
    pub persistent: Option<PersistentVisualRange>,
}

impl Default for VisualState {
    fn default() -> Self {
        Self::new()
    }
}

impl VisualState {
    pub fn new() -> Self {
        Self {
            mode: None,
            persistent: None,
        }
    }
}

/// Data loaded from the repo for bookmark/tag/workspace views.
pub struct ViewData {
    /// Aggregated bookmark data for the bookmark view.
    pub bookmark_entries: Vec<BookmarkViewEntry>,
    /// Rich detail data per bookmark (conflict targets, remote tracking).
    pub bookmark_details: HashMap<BookmarkName, crate::dag::BookmarkDetails>,
    /// Bookmarks whose detail rows are collapsed (empty = all expanded).
    pub folded_bookmarks: HashSet<BookmarkName>,
    /// All local tag names (including those outside the current revset).
    pub all_tags: Vec<TagName>,
    /// Aggregated tag data for the tag view.
    pub tag_entries: Vec<TagViewEntry>,
    /// Rich detail data per tag (remote tracking).
    pub tag_details: HashMap<TagName, crate::dag::TagDetails>,
    /// Tags whose detail rows are collapsed (empty = all expanded).
    pub folded_tags: HashSet<TagName>,
    /// Aggregated workspace data for the workspace view.
    pub workspace_entries: Vec<WorkspaceInfo>,
    /// All remote bookmarks (tracked and untracked, including off-DAG).
    pub remote_bookmarks: Vec<crate::dag::RemoteBookmarkRef>,
    /// The git remotes, with what each holds.
    pub remotes: Vec<crate::dag::RemoteEntry>,
    /// Whether to show separator lines between bookmark groups.
    pub show_bookmark_separators: bool,
}

impl Default for ViewData {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewData {
    pub fn new() -> Self {
        Self {
            bookmark_entries: Vec::new(),
            bookmark_details: HashMap::new(),
            folded_bookmarks: HashSet::new(),
            all_tags: Vec::new(),
            tag_entries: Vec::new(),
            tag_details: HashMap::new(),
            folded_tags: HashSet::new(),
            workspace_entries: Vec::new(),
            remote_bookmarks: Vec::new(),
            remotes: Vec::new(),
            show_bookmark_separators: false,
        }
    }
}

/// Revset configuration and loading state.
pub struct RevsetConfig {
    /// The current revset expression.
    pub current: Str,
    /// Last failed revset attempt (pre-fills the input on retry).
    pub draft: Option<Str>,
    /// Current revset load status.
    pub load_state: Loadable<()>,
    /// Revset currently being requested, if any.
    pub pending: Option<Str>,
    /// Active preset index (into `App::config`'s `revsets.presets`), or
    /// `None` for jj default / manual revset.
    pub active_preset: Option<usize>,
    /// The revset in effect before the `conflicted()` toggle was turned on,
    /// so toggling off returns there instead of jj's default. `Some` also
    /// *is* the "toggle currently on" state, replacing string-equality
    /// inference. Cleared by any other revset change.
    pub conflicted_prev: Option<Str>,
}

/// State for the operation log view.
pub struct OpLogState {
    pub entries: Vec<Drawn<OpLogEntry>>,
    /// Whether `entries` is current, loading or failed. Entries stay shown
    /// while more are loaded.
    pub load_state: Loadable<()>,
    pub has_more: bool,
    pub limit: usize,
    pub workspace_filter: HashSet<WorkspaceName>,
    pub unfolded: HashSet<OperationId>,
    pub details: HashMap<OperationId, Loadable<Vec<OpDetailLine>>>,
}

impl Default for OpLogState {
    fn default() -> Self {
        Self::new()
    }
}

impl OpLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            load_state: Loadable::NotRequested,
            has_more: false,
            limit: super::OP_LOG_BATCH_SIZE,
            workspace_filter: HashSet::from([WorkspaceName::new("default")]),
            unfolded: HashSet::new(),
            details: HashMap::new(),
        }
    }
}

/// State for the evolution log view.
pub struct EvoLogState {
    pub entries: Vec<Drawn<EvoLogEntry>>,
    /// Whether `entries` is current for `commit_id`, loading or failed.
    pub load_state: Loadable<()>,
    pub commit_id: Option<CommitId>,
    pub unfolded: HashSet<CommitId>,
    /// Each unfolded step's changed files, keyed by the step's commit.
    pub files: HashMap<CommitId, FileTree>,
    pub unfolded_files: HashSet<(CommitId, RepoPath)>,
}

impl Default for EvoLogState {
    fn default() -> Self {
        Self::new()
    }
}

impl EvoLogState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            load_state: Loadable::NotRequested,
            commit_id: None,
            unfolded: HashSet::new(),
            files: HashMap::new(),
            unfolded_files: HashSet::new(),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.load_state = Loadable::NotRequested;
        self.unfolded.clear();
        self.files.clear();
        self.unfolded_files.clear();
    }
}

/// State for the command log view: the most recent entries, within
/// [`Self::MAX_ENTRIES`] and [`Self::MAX_OUTPUT_BYTES`].
///
/// Entries are numbered as they are logged, and a [`CommandLogIdx`] is that
/// number, not a position: dropping the oldest leaves every other entry's
/// number, and so the rows and folds pointing at it, as they were.
///
/// [`CommandLogIdx`]: crate::idx::CommandLogIdx
pub struct CommandLogState {
    /// Oldest first.
    entries: std::collections::VecDeque<CommandLogEntry>,
    /// The number of the oldest entry kept.
    first: usize,
    /// Output bytes held across the kept entries.
    output_bytes: usize,
    pub unfolded: HashSet<crate::idx::CommandLogIdx>,
}

impl Default for CommandLogState {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandLogState {
    pub const MAX_ENTRIES: usize = 1000;
    /// Output is what makes entries big: a few `jj run`s over many commits
    /// could otherwise hold on to a lot of memory.
    pub const MAX_OUTPUT_BYTES: usize = 32 << 20;

    pub fn new() -> Self {
        Self {
            entries: std::collections::VecDeque::new(),
            first: 0,
            output_bytes: 0,
            unfolded: HashSet::new(),
        }
    }

    pub fn get(&self, idx: crate::idx::CommandLogIdx) -> Option<&CommandLogEntry> {
        self.entries.get(idx.raw().checked_sub(self.first)?)
    }

    /// The kept entries' numbers, oldest first.
    pub fn indices(&self) -> impl DoubleEndedIterator<Item = crate::idx::CommandLogIdx> + use<> {
        (self.first..self.first + self.entries.len()).map(crate::idx::CommandLogIdx::new)
    }

    /// The kept entries, oldest first.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &CommandLogEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Log `entry`, dropping the oldest entries past the limits. The newest
    /// is always kept, however big its output.
    pub fn push(&mut self, entry: CommandLogEntry) {
        self.output_bytes += entry.output.len();
        self.entries.push_back(entry);
        while self.entries.len() > Self::MAX_ENTRIES
            || (self.output_bytes > Self::MAX_OUTPUT_BYTES && self.entries.len() > 1)
        {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            self.output_bytes -= oldest.output.len();
            self.unfolded
                .remove(&crate::idx::CommandLogIdx::new(self.first));
            self.first += 1;
        }
    }
}

/// The interdiff view's comparison of two commits.
pub struct Interdiff {
    pub from_label: Str,
    pub to_label: Str,
    /// The files that differ, for a `DiffTarget::Interdiff`.
    pub files: FileTree,
    pub unfolded_files: HashSet<RepoPath>,
}

/// A file at a revision the user asked to view, waiting on its content.
pub struct FileViewRequest {
    pub commit_id: CommitId,
    pub path: RepoPath,
    pub line: usize,
}

/// A file's content, ready to open read-only in `$EDITOR` at `line`.
pub struct FileView {
    pub path: RepoPath,
    pub content: Vec<u8>,
    pub line: usize,
}

/// The commit + path being annotated.
#[derive(Clone, PartialEq, Eq)]
pub struct AnnotateTarget {
    pub commit_id: CommitId,
    pub path: RepoPath,
}

/// Annotations already computed, most recently used first.
///
/// An annotation is a pure function of its target: a commit's ancestry is
/// immutable, so once computed for a given commit and path the result stays
/// valid for as long as that commit exists, including across refreshes.
/// Recomputing costs most of a second on a large repo, and time travel walks
/// back and forth over the same targets.
#[derive(Default)]
pub struct AnnotateCache {
    entries: Vec<(AnnotateTarget, crate::dag::AnnotateResult)>,
}

/// Total lines to keep. A budget rather than an entry count, because one
/// annotation of a large file costs more than several of small ones.
const MAX_CACHED_ANNOTATE_LINES: usize = 50_000;

impl AnnotateCache {
    pub fn get(&mut self, target: &AnnotateTarget) -> Option<&crate::dag::AnnotateResult> {
        let pos = self.entries.iter().position(|(t, _)| t == target)?;
        // Most recent last-used moves to the front so eviction drops the
        // targets being navigated away from.
        let entry = self.entries.remove(pos);
        self.entries.insert(0, entry);
        Some(&self.entries[0].1)
    }

    pub fn insert(&mut self, target: AnnotateTarget, result: crate::dag::AnnotateResult) {
        self.entries.retain(|(t, _)| *t != target);
        self.entries.insert(0, (target, result));

        let mut lines = 0;
        let mut newest = true;
        self.entries.retain(|(_, r)| {
            lines += r.lines.len();
            // The entry just inserted is kept whatever its size: a file too
            // large for the whole budget is the most expensive one to
            // recompute, so it is the last thing to drop.
            let keep = newest || lines <= MAX_CACHED_ANNOTATE_LINES;
            newest = false;
            keep
        });
    }
}

/// State for the annotate (blame) view.
pub struct AnnotateState {
    /// What is being annotated (None when view is inactive/cleared).
    pub target: Option<AnnotateTarget>,
    /// Annotation lines.
    pub lines: Loadable<Vec<crate::dag::AnnotateLineData>>,
    /// Per-commit detail metadata for expansion.
    pub commit_info: HashMap<CommitId, crate::dag::AnnotateCommitInfo>,
    /// Line indices that are currently unfolded (showing details).
    pub unfolded_lines: HashSet<crate::idx::AnnotateLineIdx>,
    /// After reload, jump cursor to this 1-based line number.
    pub target_line: Option<usize>,
    /// Time-travel history stack: (commit_id, line_number) for backtracking with `f`.
    pub history: Vec<(CommitId, usize)>,
    /// Show separator lines between groups of lines from different commits.
    pub show_commit_separators: bool,
    /// Previously computed annotations, so revisiting a target is free.
    pub cache: AnnotateCache,
}

impl Default for AnnotateState {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnotateState {
    pub fn new() -> Self {
        Self {
            target: None,
            lines: Loadable::NotRequested,
            commit_info: HashMap::new(),
            unfolded_lines: HashSet::new(),
            target_line: None,
            history: Vec::new(),
            show_commit_separators: false,
            cache: AnnotateCache::default(),
        }
    }

    /// Clear state for a fresh annotation, but preserve history stack and
    /// commit_info (needed for breadcrumb rendering of past commits).
    pub fn clear_keep_history(&mut self) {
        let history = std::mem::take(&mut self.history);
        let commit_info = std::mem::take(&mut self.commit_info);
        self.clear();
        self.history = history;
        self.commit_info = commit_info;
    }

    /// Reset what belongs to one annotation. The cache and the separator
    /// preference outlive any single one, so they are left alone, hence
    /// clearing field by field rather than replacing `self` wholesale.
    pub fn clear(&mut self) {
        self.target = None;
        self.lines = Loadable::NotRequested;
        self.commit_info.clear();
        self.unfolded_lines.clear();
        self.target_line = None;
        self.history.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandLogKind {
    Command,
    Background,
    Warning,
}

pub struct CommandLogEntry {
    pub kind: CommandLogKind,
    pub summary: String,
    /// Structured command parts for syntax highlighting (None for non-command entries).
    pub command_parts: Option<Vec<crate::jj_command::CommandPart>>,
    pub output: Vec<u8>,
    /// ANSI-parsed output lines, cached to avoid re-parsing every frame.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
    pub success: bool,
    pub timestamp: jiff::Timestamp,
}

/// What a target selection has picked.
#[derive(Debug, Clone)]
pub struct Picks {
    /// The commits the operation acts on: one, or every selected commit
    /// for an operation that takes several.
    pub sources: SmallVec1<RevisionArg>,
    /// The targets picked so far, in the order they were picked.
    pub targets: Vec<RevisionArg>,
}

/// What a follow-up asks.
pub enum FollowUpPrompt {
    /// A question of its own.
    Text(String),
    /// How to carry out an operation on what a target selection picked.
    Picked {
        operation: &'static str,
        picks: Picks,
    },
}

pub struct SubmenuToggle {
    pub node: keymap_parser::Node,
    pub flag: CommandFlags,
    pub description: CompactString,
    /// The jj feature the flag needs, as a hint for greying it out.
    pub jj: Option<crate::jj_version::JjFeature>,
}

/// The action a chain of prompts was started by. Each prompt in the chain
/// carries it until the chain ends: the command the chain produces runs
/// that action's post-hooks, and the toggles of the submenu it was picked
/// from stay on offer along the way.
#[derive(Clone)]
pub struct Invocation {
    pub action: crate::keymap::AppAction,
    pub toggles: Arc<[SubmenuToggle]>,
}

/// Parse ANSI-colored bytes into styled lines. Bytes that are not UTF-8
/// show as U+FFFD rather than cutting the line short, and a malformed
/// escape sequence falls back to plain text.
pub fn parse_ansi_lines(output: &[u8]) -> Vec<ratatui::text::Line<'static>> {
    use ansi_to_tui::IntoText as _;
    let text = String::from_utf8_lossy(output);
    match text.as_bytes().into_text() {
        Ok(parsed) => parsed.lines,
        Err(_) => text
            .lines()
            .map(|line| ratatui::text::Line::from(line.to_string()))
            .collect(),
    }
}

/// Drop ANSI styling from `output`, keeping the text the overlay would show.
/// Shares [`parse_ansi_lines`]'s parser, so a plugin parsing a jj result sees
/// exactly the characters the user sees, and inherits the same plain-text
/// fallback for malformed escape sequences.
pub fn strip_ansi(output: &str) -> String {
    let mut plain = String::with_capacity(output.len());
    for (i, line) in parse_ansi_lines(output.as_bytes()).iter().enumerate() {
        if i > 0 {
            plain.push('\n');
        }
        for span in &line.spans {
            plain.push_str(&span.content);
        }
    }
    // The parser splits on newlines and keeps none of its own, so a trailing
    // one has to be put back: patterns anchored on it are common.
    if output.ends_with('\n') && !plain.ends_with('\n') {
        plain.push('\n');
    }
    plain
}

#[cfg(test)]
mod strip_ansi_tests {
    use super::strip_ansi;

    /// Real `jj duplicate` output: the change id's unique prefix is styled
    /// separately from its tail, so the escapes land mid-identifier.
    #[test]
    fn strips_styling_from_inside_an_identifier() {
        let colored = "Duplicated 502a6da5a699 as \x1b[1m\x1b[38;5;5mk\x1b[0m\
                       \x1b[38;5;8mszsoywm\x1b[39m \x1b[1m\x1b[38;5;4m4\x1b[0m\
                       \x1b[38;5;8m878f22c\x1b[39m first\n";
        assert_eq!(
            strip_ansi(colored),
            "Duplicated 502a6da5a699 as kszsoywm 4878f22c first\n"
        );
    }

    /// Templates use tabs to separate fields, so they have to survive.
    #[test]
    fn preserves_tabs_and_blank_lines() {
        assert_eq!(
            strip_ansi("8b88470d06ee\tfeature-c\n\nd787ee7889db\t\n"),
            "8b88470d06ee\tfeature-c\n\nd787ee7889db\t\n"
        );
    }

    #[test]
    fn a_malformed_escape_falls_back_to_the_raw_text() {
        let mangled = "Duplicated \x1b[38;5 as abcd\n";
        // Whatever the parser makes of it, the identifier must still be there
        // and the call must not panic.
        assert!(strip_ansi(mangled).contains("abcd"));
    }
}

/// State for the command output overlay.
pub struct CommandOutputState {
    /// The command that was run, e.g. `"$ jj abandon xvzwolmw"`.
    pub command: String,
    /// Structured command parts for syntax highlighting.
    pub command_parts: Option<Vec<crate::jj_command::CommandPart>>,
    /// Raw stdout+stderr bytes (kept for consumers that need the bytes,
    /// e.g. Lua post-hooks).
    pub output: Vec<u8>,
    /// ANSI-parsed output lines; parsed once at construction, refreshed via
    /// [`Self::reparse`] after `output` is mutated.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
    /// Whether the command succeeded.
    pub success: bool,
    /// Follow-up options offered when the overlay is dismissed (e.g. retry
    /// with `--ignore-immutable`). Empty means no retry available.
    pub retry: Vec<crate::types::FollowUpOption>,
    /// The invocation that ran the command, handed on to a retry.
    pub origin: Option<Invocation>,
    /// Scroll offset in lines from the top; clamped during draw.
    pub scroll: u16,
}

impl CommandOutputState {
    pub fn new(
        command: String,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
        retry: Vec<crate::types::FollowUpOption>,
    ) -> Self {
        let parsed_lines = parse_ansi_lines(&output);
        Self {
            command,
            command_parts,
            output,
            parsed_lines,
            success,
            retry,
            origin: None,
            scroll: 0,
        }
    }

    /// Re-parse `parsed_lines` after mutating `output`.
    pub fn reparse(&mut self) {
        self.parsed_lines = parse_ansi_lines(&self.output);
    }

    /// Total displayed lines: the command line plus the parsed output.
    pub fn line_count(&self) -> usize {
        1 + self.parsed_lines.len()
    }

    /// Overlay height for the given overlay-base height (content plus the
    /// top border, capped at half the base).
    pub fn overlay_height(&self, base_height: u16) -> u16 {
        (self.parsed_lines.len().max(1) as u16 + 3)
            .min(base_height / 2)
            .max(3)
    }

    /// How far the content can scroll at the given overlay-base height
    /// (0 = fits entirely; the overlay has a top border, so one row of the
    /// overlay is not content).
    pub fn max_scroll(&self, base_height: u16) -> u16 {
        let visible = self.overlay_height(base_height).saturating_sub(1);
        (self.line_count() as u16).saturating_sub(visible)
    }
}

/// State for a background jj command with live output streaming.
pub struct CommandRunningState {
    pub command: String,
    pub command_parts: Vec<crate::jj_command::CommandPart>,
    pub job: crate::jj_command::JobHandle,
    /// Raw stdout+stderr bytes accumulated so far (chronologically interleaved).
    pub output: Vec<u8>,
    /// ANSI-parsed complete lines, cached to avoid re-parsing every frame.
    pub parsed_lines: Vec<ratatui::text::Line<'static>>,
    /// Byte offset into `output` up to which lines have been parsed
    /// (always just past a newline).
    pub parsed_upto: usize,
    /// Scroll offset in lines from the bottom; 0 follows the tail as
    /// output arrives. Clamped against content during draw.
    pub scroll_from_bottom: usize,
}

impl CommandRunningState {
    pub fn new(
        command: String,
        command_parts: Vec<crate::jj_command::CommandPart>,
        job: crate::jj_command::JobHandle,
    ) -> Self {
        Self {
            command,
            command_parts,
            job,
            output: Vec::new(),
            parsed_lines: Vec::new(),
            parsed_upto: 0,
            scroll_from_bottom: 0,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PersistedState {
    pub show_line_numbers: bool,
    pub ignore_immutable: bool,
    pub ignore_working_copy: bool,
    pub debug: bool,
    /// Per-view search scope bitflags, keyed by the view's name so that
    /// adding or reordering views keeps each view's scopes.
    #[serde(
        default,
        alias = "search_scopes",
        deserialize_with = "deserialize_view_search_scopes"
    )]
    pub view_search_scopes: std::collections::BTreeMap<String, u16>,
    /// By name: an index would name another preset once the list is
    /// reordered.
    pub active_preset_name: Option<String>,
    pub git_diff: bool,
    pub annotate_separators: bool,
    pub bookmark_separators: bool,
    pub diff_underline: bool,
    /// Previously run `jj run` commands, most recent first.
    pub run_history: Vec<String>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            show_line_numbers: false,
            ignore_immutable: false,
            ignore_working_copy: false,
            debug: false,
            view_search_scopes: std::collections::BTreeMap::new(),
            active_preset_name: None,
            git_diff: false,
            annotate_separators: false,
            bookmark_separators: false,
            diff_underline: true,
            run_history: Vec::new(),
        }
    }
}

/// Search scopes as saved by view name, or as the array older versions
/// saved, indexed by the views in the order they had then.
fn deserialize_view_search_scopes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, u16>, D::Error> {
    const INDEXED_ORDER: [ActiveView; 9] = [
        ActiveView::Dag,
        ActiveView::Bookmarks,
        ActiveView::Tags,
        ActiveView::Operations,
        ActiveView::Workspaces,
        ActiveView::Evolog,
        ActiveView::CommandLog,
        ActiveView::Interdiff,
        ActiveView::Annotate,
    ];
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Saved {
        ByView(std::collections::BTreeMap<String, u16>),
        Indexed(Vec<u16>),
    }
    let saved = <Saved as serde::Deserialize>::deserialize(deserializer)?;
    Ok(match saved {
        Saved::ByView(scopes) => scopes,
        Saved::Indexed(bits) => INDEXED_ORDER
            .iter()
            .map(ActiveView::to_string)
            .zip(bits)
            .collect(),
    })
}

/// The revset to open with, and the preset it came from: the command
/// line's revset if one was given (no preset), else the remembered preset,
/// or the first one if none is remembered or it is gone, else jj's default.
pub fn initial_revset(
    presets: &[crate::config::Preset],
    requested: Option<String>,
    remembered: Option<&str>,
) -> (Option<String>, Option<usize>) {
    if requested.is_some() {
        return (requested, None);
    }
    let preset = remembered
        .and_then(|name| presets.iter().position(|p| p.name == name))
        .or((!presets.is_empty()).then_some(0));
    (preset.map(|i| presets[i].revset.clone()), preset)
}

/// Path for user-wide persistent state (`~/.local/state/kojutsu/state.json`).
fn state_path() -> Option<PathBuf> {
    Some(
        dirs::state_dir()
            .or_else(dirs::data_dir)?
            .join("kojutsu/state.json"),
    )
}

pub fn load_persisted_state() -> PersistedState {
    let Some(path) = state_path() else {
        return PersistedState::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    }
}

pub fn save_persisted_state(state: &PersistedState) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Error,
}

/// Number of operations to load per batch in the op log view.
pub const OP_LOG_BATCH_SIZE: usize = 200;

/// Saved per-view state (cursor position, scroll, horizontal scroll, search scopes).
#[derive(Clone)]
pub struct ViewState {
    pub cursor: RowIdx,
    pub scroll_offset: usize,
    pub h_scroll: usize,
    pub search_scopes: SearchScopes,
    /// For a list view, what its cursor is on by name, and the DAG load it
    /// was taken at, while a load may still reorder its list.
    pub(crate) anchor: Option<(super::list_anchor::ListAnchor, u64)>,
}

impl ViewState {
    const fn new(scopes: SearchScopes) -> Self {
        Self {
            cursor: RowIdx::new(0),
            scroll_offset: 0,
            h_scroll: 0,
            search_scopes: scopes,
            anchor: None,
        }
    }
}

pub(super) fn default_view_states(
    configured: SearchScopes,
) -> [ViewState; <ActiveView as strum::EnumCount>::COUNT] {
    std::array::from_fn(|i| {
        ViewState::new(ActiveView::from_repr(i).unwrap().default_scopes(configured))
    })
}

/// Discriminant for the 4 mutually exclusive bookmark states.
pub enum BookmarkKind {
    /// Pure local bookmark (no remote tracking).
    Local { is_dirty: bool, is_conflicted: bool },
    /// Local bookmark that tracks a remote.
    Tracking { is_dirty: bool, is_conflicted: bool },
    /// Tracked remote-only bookmark.
    TrackedRemote { remote: RemoteName },
    /// Untracked remote-only bookmark.
    UntrackedRemote { remote: RemoteName },
}

impl BookmarkKind {
    /// A remote-only bookmark on `remote`, tracked locally or not.
    pub fn remote_only(remote: RemoteName, is_tracked: bool) -> Self {
        if is_tracked {
            Self::TrackedRemote { remote }
        } else {
            Self::UntrackedRemote { remote }
        }
    }

    /// The remote name, if this is a remote bookmark.
    pub fn remote(&self) -> Option<&RemoteName> {
        match self {
            Self::TrackedRemote { remote } | Self::UntrackedRemote { remote } => Some(remote),
            _ => None,
        }
    }

    /// Whether this bookmark has conflicting targets.
    pub fn is_conflicted(&self) -> bool {
        matches!(
            self,
            Self::Local {
                is_conflicted: true,
                ..
            } | Self::Tracking {
                is_conflicted: true,
                ..
            }
        )
    }

    /// Whether the local bookmark differs from its tracked remote.
    pub fn is_dirty(&self) -> bool {
        matches!(
            self,
            Self::Local { is_dirty: true, .. } | Self::Tracking { is_dirty: true, .. }
        )
    }

    /// Sort rank: local(0) < tracking(1) < tracked-remote(2) < untracked-remote(3).
    pub fn rank(&self) -> u8 {
        match self {
            Self::Local { .. } => 0,
            Self::Tracking { .. } => 1,
            Self::TrackedRemote { .. } => 2,
            Self::UntrackedRemote { .. } => 3,
        }
    }
}

pub struct BookmarkViewEntry {
    pub name: BookmarkName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub short_commit_id: Option<crate::dag::ShortId>,
    /// The revision to hand `jj` for this row's commit. Carried rather than
    /// rebuilt from `change_id`, which doesn't know about divergence.
    pub revision: Option<RevisionArg>,
    pub description: Option<String>,
    pub kind: BookmarkKind,
}

pub struct TagViewEntry {
    pub name: TagName,
    pub commit_id: Option<CommitId>,
    pub change_id: Option<crate::dag::ShortId>,
    pub short_commit_id: Option<crate::dag::ShortId>,
    /// The revision to hand `jj` for this row's commit. Carried rather than
    /// rebuilt from `change_id`, which doesn't know about divergence.
    pub revision: Option<RevisionArg>,
    pub description: Option<String>,
    pub presence: crate::dag::TagPresence,
    /// The local tag points at several commits at once.
    pub is_conflicted: bool,
}

/// A log entry with its graph drawn. The repo supplies the entry; the
/// app draws the graph, with the configured glyphs, once the whole log is
/// known.
pub struct Drawn<T> {
    pub entry: T,
    pub graph: crate::graph::GraphLines,
}

impl<T> std::ops::Deref for Drawn<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.entry
    }
}

/// An entry of a log the app draws as a graph.
pub trait LogEntry {
    fn log_id(&self) -> &str;
    fn log_parents(&self) -> Vec<&str>;
    /// Whether it is the newest entry: drawn with the working-copy glyph.
    fn is_head(&self) -> bool;
}

impl LogEntry for OpLogEntry {
    fn log_id(&self) -> &str {
        self.id.as_str()
    }

    fn log_parents(&self) -> Vec<&str> {
        self.parent_ids.iter().map(|id| id.as_str()).collect()
    }

    fn is_head(&self) -> bool {
        self.is_current
    }
}

impl LogEntry for EvoLogEntry {
    fn log_id(&self) -> &str {
        self.commit_id.as_str()
    }

    fn log_parents(&self) -> Vec<&str> {
        self.predecessor_ids.iter().map(|id| id.as_str()).collect()
    }

    fn is_head(&self) -> bool {
        self.is_current
    }
}

/// Draw a log's graph with the configured glyphs.
pub fn draw_log<T: LogEntry>(entries: Vec<T>, glyphs: &crate::config::GlyphChars) -> Vec<Drawn<T>> {
    use crate::config::Glyph;
    let nodes: Vec<crate::graph::LogNode<'_>> = entries
        .iter()
        .map(|entry| crate::graph::LogNode {
            id: entry.log_id(),
            parents: entry.log_parents(),
            glyph: glyphs.char_for(if entry.is_head() {
                Glyph::WorkingCopy
            } else {
                Glyph::Normal
            }),
        })
        .collect();
    let graphs = crate::graph::render_log(&nodes);
    entries
        .into_iter()
        .zip(graphs)
        .map(|(entry, graph)| Drawn { entry, graph })
        .collect()
}

/// Active visual selection mode.
pub enum VisualMode {
    /// Visual selection of diff lines within one file.
    Lines {
        /// Row index where `v` was pressed.
        anchor: RowIdx,
    },
    /// Visual selection of the commits between two entries in display
    /// order.
    Commits {
        /// Entry where `v` was pressed.
        anchor: EntryIdx,
        /// The end that moves with the cursor.
        head: EntryIdx,
    },
    /// Visual selection of files within a single commit.
    Files {
        /// Row index where `v` was pressed.
        anchor: RowIdx,
        /// Which commit the anchor belongs to (single-commit constraint).
        entry_idx: EntryIdx,
    },
}

/// Persistent visual range (survives exiting visual mode with `v`).
pub enum PersistentVisualRange {
    Lines(VisualRange),
    /// The commits from `lo` to `hi` in display order.
    Commits {
        lo: EntryIdx,
        hi: EntryIdx,
    },
    /// A contiguous range of files within one commit.
    Files {
        entry_idx: EntryIdx,
        lo: crate::idx::FileIdx,
        hi: crate::idx::FileIdx,
    },
}

/// State for the select-from-list overlay (e.g. picking a bookmark).
pub struct SelectFromListState {
    pub title: String,
    pub items: Vec<String>,
    pub filtered_indices: Vec<usize>,
    /// Byte offsets of the matched characters for each entry in
    /// `filtered_indices`.
    pub match_positions: Vec<Vec<usize>>,
    pub cursor: usize,
    pub scroll_offset: usize,
    pub marked: HashSet<usize>,
    pub multi: bool,
    pub filter: String,
    pub filtering: bool,
    /// When present, `items[0]` is a free-input affordance rather than data:
    /// it is pinned to the top through filtering, cannot be marked, and
    /// selecting it opens the text input this payload describes instead of
    /// resolving to an item.
    pub custom_entry: Option<CustomEntry>,
    pub on_select: PendingSelection,
    pub origin: Option<Invocation>,
}

/// The free-input affordance of a select list: what selecting the pinned
/// custom row does. Carrying the prompt and submit handler here makes the
/// row's behavior total: a list cannot be built with a custom row that
/// does nothing.
pub struct CustomEntry {
    /// Prompt for the text input (e.g. `"run: "`).
    pub prompt: String,
    /// Submit handler for the text input.
    pub on_submit: TextPrompt,
}

impl SelectFromListState {
    pub fn new(
        title: impl Into<String>,
        items: Vec<String>,
        multi: bool,
        on_select: PendingSelection,
        focus_filter: bool,
    ) -> Self {
        let count = items.len();
        Self {
            title: title.into(),
            items,
            filtered_indices: (0..count).collect(),
            match_positions: vec![vec![]; count],
            cursor: 0,
            scroll_offset: 0,
            marked: HashSet::new(),
            multi,
            filter: String::new(),
            filtering: focus_filter,
            custom_entry: None,
            on_select,
            origin: None,
        }
    }
}

/// State for jump mode: typed-label navigation to a visible row.
pub struct JumpState {
    /// Each label and the row it jumps to, by the row itself rather than its
    /// position: rows can be rebuilt while the labels show.
    pub labels: Vec<(String, DisplayRow)>,
    /// Characters typed so far (for multi-char label matching).
    pub input: String,
}

/// The current interaction mode.
pub enum AppMode {
    /// Normal browsing.
    Normal,
    /// A prefix key was pressed; showing submenu options in the bottom bar.
    Submenu {
        /// Display string for the prefix key (e.g., "s", "b", "g").
        key: String,
        label: CompactString,
        children: Arc<[(keymap_parser::Node, TrieNode)]>,
        flags: CommandFlags,
    },
    /// Showing the result of a shell command. Scrolls when the output
    /// overflows; dismissed on any non-scroll keypress.
    CommandOutput(CommandOutputState),
    /// Help overlay showing all keybindings.
    Help { scroll: u16 },
    /// Single-line text input in the bottom bar.
    TextInput {
        prompt: String,
        input: Input,
        on_submit: TextPrompt,
        origin: Option<Invocation>,
    },
    /// Live search input at the bottom bar.
    SearchInput,
    /// Navigating to select a target commit for a two-commit operation.
    TargetSelect {
        picks: Picks,
        /// Where the cursor was, to return to: on Esc, and on confirming,
        /// since the command acts on what was there, not on the target.
        started_on: Option<DisplayRow>,
        operation: TargetOperation,
        flags: CommandFlags,
        origin: Option<Invocation>,
    },
    /// Navigating to select a single commit (e.g. for workspace revision).
    CommitSelect {
        /// Where the cursor was, to return to on Esc.
        started_on: Option<DisplayRow>,
        pending: PendingCommitSelect,
        flags: CommandFlags,
        origin: Option<Invocation>,
    },
    /// Choosing from a set of follow-up options after target selection.
    FollowUp {
        prompt: FollowUpPrompt,
        options: Vec<FollowUpOption>,
        origin: Option<Invocation>,
    },
    /// Jump mode: labels visible on jumpable rows, type label chars to jump.
    Jump(JumpState),
    /// Selecting an item from a list (e.g. picking a bookmark).
    SelectFromList(SelectFromListState),
    /// A background jj command is running with live output; the UI stays
    /// live. Esc/Ctrl-C cancels, j/k scrolls the output.
    CommandRunning(CommandRunningState),
}

impl AppMode {
    /// Construct a `CommandOutput` mode scrolled to the top.
    pub fn command_output(
        command: String,
        command_parts: Option<Vec<crate::jj_command::CommandPart>>,
        output: Vec<u8>,
        success: bool,
        retry: Vec<crate::types::FollowUpOption>,
    ) -> Self {
        AppMode::CommandOutput(CommandOutputState::new(
            command,
            command_parts,
            output,
            success,
            retry,
        ))
    }

    /// Construct a `TextInput` mode with the given prompt, prefill, and submit handler.
    pub fn text_input(
        prompt: impl Into<String>,
        prefill: impl Into<String>,
        on_submit: impl Into<TextPrompt>,
    ) -> Self {
        AppMode::TextInput {
            prompt: prompt.into(),
            input: Input::new(prefill.into()),
            on_submit: on_submit.into(),
            origin: None,
        }
    }

    /// Construct a `SelectFromList` mode with default cursor/filter state.
    pub fn select_from_list(
        title: impl Into<String>,
        items: Vec<String>,
        multi: bool,
        on_select: PendingSelection,
        focus_filter: bool,
    ) -> Self {
        AppMode::SelectFromList(SelectFromListState::new(
            title,
            items,
            multi,
            on_select,
            focus_filter,
        ))
    }

    /// Construct a single-select `SelectFromList` whose first row is a
    /// free-input affordance labelled `custom_label` (see
    /// [`SelectFromListState::custom_entry`]).
    pub fn select_from_list_with_custom(
        title: impl Into<String>,
        custom_label: &str,
        custom: CustomEntry,
        mut items: Vec<String>,
        on_select: PendingSelection,
    ) -> Self {
        items.insert(0, custom_label.to_string());
        let mut state = SelectFromListState::new(title, items, false, on_select, false);
        state.custom_entry = Some(custom);
        AppMode::SelectFromList(state)
    }

    /// Replace a `CommandOutput` mode with its retry prompt, if it offers
    /// one, or with `Normal`. Returns whether it offered one.
    pub fn open_command_retry(&mut self) -> bool {
        match std::mem::replace(self, AppMode::Normal) {
            AppMode::CommandOutput(state) if !state.retry.is_empty() => {
                *self = AppMode::FollowUp {
                    prompt: FollowUpPrompt::Text("Retry?".into()),
                    options: state.retry,
                    origin: state.origin,
                };
                true
            }
            AppMode::CommandOutput(_) => false,
            other => {
                *self = other;
                false
            }
        }
    }

    /// What a target selection, or the follow-up to one, has picked. The
    /// DAG marks them while either is on screen.
    pub fn picks(&self) -> Option<&Picks> {
        match self {
            AppMode::TargetSelect { picks, .. }
            | AppMode::FollowUp {
                prompt: FollowUpPrompt::Picked { picks, .. },
                ..
            } => Some(picks),
            _ => None,
        }
    }

    /// The invocation the current prompt chain was started by.
    pub fn origin(&self) -> Option<&Invocation> {
        match self {
            AppMode::TextInput { origin, .. }
            | AppMode::TargetSelect { origin, .. }
            | AppMode::CommitSelect { origin, .. }
            | AppMode::FollowUp { origin, .. } => origin.as_ref(),
            AppMode::SelectFromList(state) => state.origin.as_ref(),
            _ => None,
        }
    }

    /// Record `origin` on a prompt, or a command's output, that doesn't
    /// have one yet.
    pub fn adopt_origin(&mut self, origin: &Invocation) {
        let slot = match self {
            AppMode::TextInput { origin, .. }
            | AppMode::TargetSelect { origin, .. }
            | AppMode::CommitSelect { origin, .. }
            | AppMode::FollowUp { origin, .. } => origin,
            AppMode::SelectFromList(state) => &mut state.origin,
            AppMode::CommandOutput(state) => &mut state.origin,
            _ => return,
        };
        if slot.is_none() {
            *slot = Some(origin.clone());
        }
    }

    /// Take the state out of a `Jump` mode, replacing `self` with `Normal`.
    pub fn take_jump(&mut self) -> Option<JumpState> {
        match std::mem::replace(self, AppMode::Normal) {
            AppMode::Jump(state) => Some(state),
            other => {
                *self = other;
                None
            }
        }
    }
}

#[cfg(test)]
mod annotate_cache_tests {
    use super::*;
    use crate::dag::AnnotateResult;

    fn target(commit: &str, path: &str) -> AnnotateTarget {
        AnnotateTarget {
            commit_id: CommitId::new(commit),
            path: RepoPath::new(path),
        }
    }

    fn result(lines: usize) -> AnnotateResult {
        AnnotateResult {
            lines: vec![
                crate::dag::AnnotateLineData {
                    commit_id: CommitId::new("abc"),
                    change_id: crate::dag::ShortId::new("uunnomkx"),
                    author: String::new(),
                    relative_time: crate::types::Str::new(""),
                    line_number: 1,
                    content: String::new(),
                    syntax_tokens: Vec::new(),
                    outside_domain: false,
                };
                lines
            ],
            commit_info: HashMap::new(),
        }
    }

    #[test]
    fn re_inserting_a_target_replaces_it_rather_than_duplicating() {
        let mut cache = AnnotateCache::default();
        cache.insert(target("aaa", "a.rs"), result(3));
        cache.insert(target("aaa", "a.rs"), result(9));

        assert_eq!(cache.get(&target("aaa", "a.rs")).unwrap().lines.len(), 9);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn the_line_budget_evicts_the_least_recently_used() {
        let mut cache = AnnotateCache::default();
        let big = MAX_CACHED_ANNOTATE_LINES / 2;
        cache.insert(target("aaa", "a.rs"), result(big));
        cache.insert(target("bbb", "b.rs"), result(big));

        // Touching `aaa` makes `bbb` the least recently used.
        assert!(cache.get(&target("aaa", "a.rs")).is_some());
        cache.insert(target("ccc", "c.rs"), result(big));

        assert!(cache.get(&target("ccc", "c.rs")).is_some());
        assert!(cache.get(&target("aaa", "a.rs")).is_some());
        assert!(cache.get(&target("bbb", "b.rs")).is_none());
    }

    #[test]
    fn one_annotation_over_budget_is_still_kept() {
        // Otherwise a file larger than the whole budget could never be cached,
        // and it is exactly the slowest case to recompute.
        let mut cache = AnnotateCache::default();
        cache.insert(target("aaa", "a.rs"), result(MAX_CACHED_ANNOTATE_LINES * 2));

        assert!(cache.get(&target("aaa", "a.rs")).is_some());
    }
}

#[cfg(test)]
mod draw_log_tests {
    use super::draw_log;
    use crate::config::GlyphChars;
    use crate::history::EvoLogEntry;
    use crate::types::CommitId;

    /// The logs draw with the configured glyphs, as the DAG does, rather
    /// than with fixed characters.
    #[test]
    fn a_log_draws_with_the_configured_glyphs() {
        let mut current = EvoLogEntry::for_test(CommitId::new("b"), vec![CommitId::new("a")]);
        current.is_current = true;
        let older = EvoLogEntry::for_test(CommitId::new("a"), Vec::new());
        let glyphs = GlyphChars {
            working_copy: 'W',
            normal: 'N',
            ..GlyphChars::default()
        };

        let drawn = draw_log(vec![current, older], &glyphs);
        assert!(drawn[0].graph.node.contains('W'));
        assert!(drawn[1].graph.node.contains('N'));
    }
}

#[cfg(test)]
mod initial_revset_tests {
    use super::initial_revset;
    use crate::config::Preset;

    fn presets() -> Vec<Preset> {
        ["mine", "all"]
            .iter()
            .map(|name| Preset {
                name: (*name).into(),
                revset: format!("{name}()"),
            })
            .collect()
    }

    /// A revset asked for on the command line is shown as itself, not as
    /// whatever preset was last active.
    #[test]
    fn a_command_line_revset_belongs_to_no_preset() {
        assert_eq!(
            initial_revset(&presets(), Some("@".into()), Some("all")),
            (Some("@".into()), None)
        );
    }

    /// Remembered by name, so reordering the list in between keeps it.
    #[test]
    fn the_remembered_preset_opens_wherever_it_now_is() {
        let mut presets = presets();
        presets.reverse();
        assert_eq!(
            initial_revset(&presets, None, Some("mine")),
            (Some("mine()".into()), Some(1))
        );
    }

    /// A remembered preset the config no longer has falls back to the first.
    #[test]
    fn a_vanished_preset_falls_back_to_the_first() {
        assert_eq!(
            initial_revset(&presets(), None, Some("gone")),
            (Some("mine()".into()), Some(0))
        );
    }

    #[test]
    fn without_presets_jj_decides() {
        assert_eq!(initial_revset(&[], None, Some("mine")), (None, None));
    }
}

#[cfg(test)]
mod ansi_tests {
    use super::parse_ansi_lines;

    /// A line with a byte that is not UTF-8 keeps the rest of its text,
    /// the byte shown as the replacement character.
    #[test]
    fn a_line_that_is_not_utf8_is_shown_rather_than_cut() {
        let lines = parse_ansi_lines(b"first\nbad \xff byte\n");
        let texts: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(texts, ["first", "bad \u{fffd} byte"]);
    }
}

#[cfg(test)]
mod persisted_state_tests {
    use super::PersistedState;
    use crate::app::App;
    use crate::types::{ActiveView, SearchScopes};

    /// The array older versions saved still lands on the views it was for,
    /// along with everything else in the file.
    #[test]
    fn search_scopes_saved_as_an_array_are_read_by_their_old_order() {
        let saved = r#"{
            "ignore_immutable": true,
            "view_search_scopes": [5, 12, 132, 4, 5, 5, 5, 5, 65]
        }"#;
        let state: PersistedState = serde_json::from_str(saved).unwrap();
        assert!(state.ignore_immutable);
        let mut app = App::for_test();
        app.apply_persisted_state(&state);
        let scopes = |view: ActiveView| app.view_states[view.idx()].search_scopes.bits();
        assert_eq!(scopes(ActiveView::Bookmarks), 12);
        assert_eq!(scopes(ActiveView::Tags), 132);
        assert_eq!(scopes(ActiveView::Annotate), 65);
    }

    /// Scopes are saved by view name and read back the same; a name this
    /// version doesn't know is skipped rather than losing the file.
    #[test]
    fn search_scopes_round_trip_by_view_name() {
        let mut app = App::for_test();
        app.view_states[ActiveView::Tags.idx()].search_scopes = SearchScopes::TAG;
        let mut json = serde_json::to_value(app.to_persisted_state()).unwrap();
        json["view_search_scopes"]["from_a_newer_version"] = 4.into();

        let state: PersistedState = serde_json::from_value(json).unwrap();
        let mut restored = App::for_test();
        restored.apply_persisted_state(&state);
        assert_eq!(
            restored.view_states[ActiveView::Tags.idx()].search_scopes,
            SearchScopes::TAG
        );
    }
}
