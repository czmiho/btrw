use ratatui::layout::Rect;
use btrw_core::btrfs_du::SnapshotUsage;
use btrw_core::compare::{
    DirCompareResult, DirStats, DiffLine, PaneRow, PatchExportSummary, SideBySideRow,
};
use btrw_core::config::TimeTravelHighlight;
use btrw_core::fs_safe::{StatSig, format_os_display};
use btrw_core::model::PanelSide;
use btrw_core::restore::RestorePlan;
use btrw_core::timetravel::{
    TimeTravelHistory, TimeTravelLine, TimeTravelLineKind, VersionContent, load_version_content,
    render_version_lines,
};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;


pub enum DuMsg {
    Started(PathBuf),
    Result(PathBuf, Result<SnapshotUsage, String>),
    Finished,
}

pub struct DuJob {
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<DuMsg>,
    pub total: usize,
    pub done: usize,
    pub current: Option<PathBuf>,
}

pub struct DirStatsJob {
    pub active_rel: PathBuf,
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<(PathBuf, DirStats)>,
}

pub enum FlatMsg {
    Progress(usize, String),
    Done(PathBuf, DirCompareResult, bool),
}

pub struct FlatJob {
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<FlatMsg>,
    pub scanned: usize,
    pub current_path: String,
}

pub struct DirLoadJob {
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<DirCompareResult>,
}

pub struct ExportPatchJob {
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<Result<(PatchExportSummary, PathBuf), String>>,
}

pub struct TimeTravelJob {
    pub cancel: Arc<AtomicBool>,
    pub rx: mpsc::Receiver<TimeTravelHistory>,
    pub rel_path: PathBuf,
    pub total_sources: usize,
    /// Context captured when the scan started so stale results after group switch are discarded.
    pub group_idx: Option<usize>,
    pub verified_live_target: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct TimeTravelState {
    pub history: TimeTravelHistory,
    pub group_idx: Option<usize>,
    pub verified_live_target: Option<PathBuf>,
    pub active_idx: usize,
    /// Up to 2 pinned version indices (sorted chronologically); when 2 are pinned, shows diff between them.
    pub pinned: Vec<usize>,
    pub highlight: TimeTravelHighlight,
    pub content_cache: HashMap<usize, VersionContent>,
    pub rendered: Vec<TimeTravelLine>,
    pub scroll_y: usize,
    pub scroll_x: usize,
}

impl TimeTravelState {
    pub fn toggle_pin(&mut self) {
        if self.history.versions.is_empty() {
            return;
        }
        let idx = self.active_idx;
        if let Some(pos) = self.pinned.iter().position(|&p| p == idx) {
            self.pinned.remove(pos);
        } else if self.pinned.len() < 2 {
            self.pinned.push(idx);
            self.pinned.sort_unstable();
        } else {
            self.pinned = vec![self.pinned[1], idx];
            self.pinned.sort_unstable();
        }
    }

    pub fn pinned_pair(&self) -> Option<(usize, usize)> {
        if self.pinned.len() == 2 {
            Some((self.pinned[0], self.pinned[1]))
        } else {
            None
        }
    }

    pub fn rebuild_lines(&mut self, max_bytes: u64, max_lines: usize, jump_to_first_change: bool) {
        if self.history.versions.is_empty() {
            self.rendered.clear();
            return;
        }
        let idx = self.active_idx.min(self.history.versions.len() - 1);
        self.active_idx = idx;

        let rel = self.history.rel_path.clone();
        let ensure =
            |cache: &mut HashMap<usize, VersionContent>, i: usize, history: &TimeTravelHistory| {
                cache
                    .entry(i)
                    .or_insert_with(|| load_version_content(&history.versions[i], &rel, max_bytes))
                    .clone()
            };

        if let Some((a, b)) = self.pinned_pair() {
            let va = ensure(&mut self.content_cache, a, &self.history);
            let vb = ensure(&mut self.content_cache, b, &self.history);
            self.rendered = render_version_lines(
                &vb,
                Some(&va),
                TimeTravelHighlight::PreviousVersion,
                max_lines,
            );
        } else {
            let cur = ensure(&mut self.content_cache, idx, &self.history);
            let reference = match self.highlight {
                TimeTravelHighlight::PreviousVersion => {
                    if idx > 0 {
                        Some(ensure(&mut self.content_cache, idx - 1, &self.history))
                    } else {
                        None
                    }
                }
                TimeTravelHighlight::LiveState => {
                    let last = self.history.versions.len() - 1;
                    if idx != last {
                        Some(ensure(&mut self.content_cache, last, &self.history))
                    } else {
                        None
                    }
                }
                TimeTravelHighlight::None => None,
            };

            self.rendered =
                render_version_lines(&cur, reference.as_ref(), self.highlight, max_lines);
        }

        if jump_to_first_change {
            if let Some(first_chg) = self.rendered.iter().position(|l| {
                matches!(l.kind, TimeTravelLineKind::Added | TimeTravelLineKind::Removed)
            }) {
                self.scroll_y = first_chg.saturating_sub(3);
            } else {
                self.scroll_y = 0;
            }
        } else {
            self.scroll_y = self.scroll_y.min(self.rendered.len().saturating_sub(1));
        }
    }

    pub fn jump_change(&mut self, forward: bool) {
        let is_chg = |l: &TimeTravelLine| {
            matches!(l.kind, TimeTravelLineKind::Added | TimeTravelLineKind::Removed)
        };
        if forward {
            if let Some(pos) = self
                .rendered
                .iter()
                .enumerate()
                .skip(self.scroll_y + 4)
                .find(|(_, l)| is_chg(l))
                .map(|(i, _)| i)
            {
                self.scroll_y = pos.saturating_sub(3);
            }
        } else if let Some(pos) = self
            .rendered
            .iter()
            .enumerate()
            .take(self.scroll_y)
            .rev()
            .find(|(_, l)| is_chg(l))
            .map(|(i, _)| i)
        {
            self.scroll_y = pos.saturating_sub(3);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LayoutRects {
    pub left_pane: Rect,
    pub right_pane: Rect,
    pub modal: Rect,
    pub footer: Rect,
}

#[derive(Debug, Clone)]
pub enum PickerEntry {
    Live {
        timeline_idx: usize,
        label: String,
    },
    Day {
        day_key: String,
        display_day: String,
        count: usize,
        expanded: bool,
    },
    Snapshot {
        timeline_idx: usize,
        day_key: String,
        time_hm: String,
        label: String,
        marker: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveAsFocus {
    DirTree,
    Filename,
}

#[derive(Debug, Clone)]
pub struct SaveAsState {
    pub source_root: PathBuf,
    pub source_label: String,
    pub rel_path: PathBuf,
    pub source_sig: StatSig,
    pub current_dir: PathBuf,
    pub subdirs: Vec<OsString>,
    pub dir_selected: usize,
    pub filename: String,
    /// Preserved raw `OsString` filename when the user has not edited `filename`.
    pub original_os_name: Option<OsString>,
    pub filename_edited: bool,
    /// Character index (0..=filename.chars().count()) for cursor editing.
    pub cursor: usize,
    pub focus: SaveAsFocus,
}

fn anchor_path(path: &std::path::Path) -> PathBuf {
    if path.as_os_str().is_empty() || path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn is_inside_any_protected(full: &std::path::Path, protected_abs: &[PathBuf]) -> bool {
    protected_abs
        .iter()
        .any(|p| full == p.as_path() || full.starts_with(p))
}

impl SaveAsState {
    pub fn reload_subdirs(&mut self, protected_dirs: &[PathBuf]) {
        let mut dirs: Vec<OsString> = Vec::new();
        let protected_abs: Vec<PathBuf> = protected_dirs
            .iter()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| anchor_path(p))
            .collect();
        let cur_abs = anchor_path(&self.current_dir);
        if let Ok(entries) = std::fs::read_dir(&self.current_dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let full = cur_abs.join(entry.file_name());
                    if is_inside_any_protected(&full, &protected_abs) {
                        continue;
                    }
                    dirs.push(entry.file_name());
                }
            }
        }
        dirs.sort_by(|a, b| {
            format_os_display(a)
                .to_lowercase()
                .cmp(&format_os_display(b).to_lowercase())
                .then_with(|| a.cmp(b))
        });
        self.subdirs = dirs;
        self.dir_selected = 0;
    }

    pub fn has_parent(&self) -> bool {
        self.current_dir.parent().is_some()
    }

    pub fn total_dir_items(&self) -> usize {
        self.subdirs.len() + usize::from(self.has_parent())
    }

    pub fn enter_selected_dir(&mut self, protected_dirs: &[PathBuf]) {
        let offset = usize::from(self.has_parent());
        if self.has_parent() && self.dir_selected == 0 {
            self.go_parent_dir(protected_dirs);
            return;
        }
        if let Some(name) = self.subdirs.get(self.dir_selected.saturating_sub(offset)) {
            self.current_dir = self.current_dir.join(name);
            self.reload_subdirs(protected_dirs);
        }
    }

    pub fn go_parent_dir(&mut self, protected_dirs: &[PathBuf]) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.reload_subdirs(protected_dirs);
        }
    }

    pub fn full_dest_path(&self) -> Option<PathBuf> {
        if !self.filename_edited
            && let Some(ref orig) = self.original_os_name
            && !orig.is_empty()
        {
            return Some(self.current_dir.join(orig));
        }
        let name = self.filename.as_str();
        if name.trim().is_empty() || name.contains('/') || name == "." || name == ".." {
            return None;
        }
        Some(self.current_dir.join(name))
    }

    fn char_to_byte_idx(&self, char_idx: usize) -> usize {
        self.filename
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.filename.len())
    }

    pub fn insert_char(&mut self, c: char) {
        if c == '/' || c.is_control() {
            return;
        }
        let b = self.char_to_byte_idx(self.cursor);
        self.filename.insert(b, c);
        self.cursor += 1;
        self.filename_edited = true;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.char_to_byte_idx(self.cursor - 1);
        let end = self.char_to_byte_idx(self.cursor);
        self.filename.replace_range(start..end, "");
        self.cursor -= 1;
        self.filename_edited = true;
    }

    pub fn delete_char(&mut self) {
        let len = self.filename.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = self.char_to_byte_idx(self.cursor);
        let end = self.char_to_byte_idx(self.cursor + 1);
        self.filename.replace_range(start..end, "");
        self.filename_edited = true;
    }

    pub fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let len = self.filename.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.filename.chars().count();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    SelectedObject,
    CurrentDir,
    WholeSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFocus {
    Scope,
    DirTree,
    Filename,
}

#[derive(Debug, Clone)]
pub struct ExportPatchState {
    pub scope: ExportScope,
    pub selected_row: Option<PaneRow>,
    /// Optional explicit `(left_root, right_root)` override (e.g. from F3 Time-Travel pinned pair).
    pub custom_roots: Option<(PathBuf, PathBuf)>,
    pub current_dir: PathBuf,
    pub subdirs: Vec<OsString>,
    pub dir_selected: usize,
    pub filename: String,
    pub cursor: usize,
    pub focus: ExportFocus,
}

impl ExportPatchState {
    pub fn reload_subdirs(&mut self, protected_dirs: &[PathBuf]) {
        let mut dirs: Vec<OsString> = Vec::new();
        let protected_abs: Vec<PathBuf> = protected_dirs
            .iter()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| anchor_path(p))
            .collect();
        let cur_abs = anchor_path(&self.current_dir);
        if let Ok(entries) = std::fs::read_dir(&self.current_dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    let full = cur_abs.join(entry.file_name());
                    if is_inside_any_protected(&full, &protected_abs) {
                        continue;
                    }
                    dirs.push(entry.file_name());
                }
            }
        }
        dirs.sort_by(|a, b| {
            format_os_display(a)
                .to_lowercase()
                .cmp(&format_os_display(b).to_lowercase())
                .then_with(|| a.cmp(b))
        });
        self.subdirs = dirs;
        self.dir_selected = 0;
    }

    pub fn has_parent(&self) -> bool {
        self.current_dir.parent().is_some()
    }

    pub fn total_dir_items(&self) -> usize {
        self.subdirs.len() + usize::from(self.has_parent())
    }

    pub fn enter_selected_dir(&mut self, protected_dirs: &[PathBuf]) {
        let offset = usize::from(self.has_parent());
        if self.has_parent() && self.dir_selected == 0 {
            self.go_parent_dir(protected_dirs);
            return;
        }
        if let Some(name) = self.subdirs.get(self.dir_selected.saturating_sub(offset)) {
            self.current_dir = self.current_dir.join(name);
            self.reload_subdirs(protected_dirs);
        }
    }

    pub fn go_parent_dir(&mut self, protected_dirs: &[PathBuf]) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.reload_subdirs(protected_dirs);
        }
    }

    pub fn cycle_scope(&mut self, forward: bool) {
        self.scope = match (self.scope, forward) {
            (ExportScope::SelectedObject, true) => ExportScope::CurrentDir,
            (ExportScope::CurrentDir, true) => ExportScope::WholeSnapshot,
            (ExportScope::WholeSnapshot, true) => ExportScope::SelectedObject,
            (ExportScope::SelectedObject, false) => ExportScope::WholeSnapshot,
            (ExportScope::CurrentDir, false) => ExportScope::SelectedObject,
            (ExportScope::WholeSnapshot, false) => ExportScope::CurrentDir,
        };
    }

    pub fn full_dest_path(&self) -> Option<PathBuf> {
        let name = self.filename.as_str();
        if name.trim().is_empty() || name.contains('/') || name == "." || name == ".." {
            return None;
        }
        Some(self.current_dir.join(name))
    }

    fn char_to_byte_idx(&self, char_idx: usize) -> usize {
        self.filename
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.filename.len())
    }

    pub fn insert_char(&mut self, c: char) {
        if c == '/' || c.is_control() {
            return;
        }
        let b = self.char_to_byte_idx(self.cursor);
        self.filename.insert(b, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.char_to_byte_idx(self.cursor - 1);
        let end = self.char_to_byte_idx(self.cursor);
        self.filename.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete_char(&mut self) {
        let len = self.filename.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = self.char_to_byte_idx(self.cursor);
        let end = self.char_to_byte_idx(self.cursor + 1);
        self.filename.replace_range(start..end, "");
    }

    pub fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let len = self.filename.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.filename.chars().count();
    }
}

#[derive(Debug, Clone)]
pub enum Mode {
    Browse,
    FilterInput,
    SubvolumePicker {
        selected: usize,
        initial: bool,
    },
    SourcePicker {
        side: PanelSide,
        selected: usize,
        expanded_days: BTreeSet<String>,
    },
    DiffView {
        title: String,
        lines: Vec<DiffLine>,
        side_rows: Option<Vec<SideBySideRow>>,
        side_by_side: bool,
        scroll_y: usize,
        scroll_x: usize,
        return_to_restore: Option<Box<RestorePlan>>,
    },
    ExportPatch(Box<ExportPatchState>),
    SaveAs(Box<SaveAsState>),
    RestoreConfirm(Box<RestorePlan>),
    TimeTravel(Box<TimeTravelState>),
    ConfigModal {
        selected: usize,
    },
    DiffMenu {
        selected: usize,
    },
    InfoModal {
        scroll: usize,
    },
    Help,
}
