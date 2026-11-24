mod diff_export;
mod jobs;
mod restore;
mod state;
#[cfg(test)]
mod tests;

pub use state::{
    DirLoadJob, DirStatsJob, DuJob, DuMsg, ExportFocus, ExportPatchJob, ExportPatchState,
    ExportScope, FlatJob, FlatMsg, LayoutRects, Mode, PickerEntry, SaveAsFocus, SaveAsState,
    TimeTravelJob, TimeTravelState,
};

use btrw_core::btrfs_du::SnapshotUsage;
use btrw_core::compare::{DirCompareResult, DirStats, DiffLine, DiffLineKind, PaneRow, sort_pane_rows};
use btrw_core::config::{
    Config, DiffFormat, LiveDiffStrategy, PermissionsColumn, SortOrder, Theme,
};
use btrw_core::discovery::{
    SubvolumeTreeEntry, auto_snapshot_roots, discover_subvolume_tree,
};
use btrw_core::fs_safe::{
    format_day_local, format_hm_local, format_system_time_local,
};
use btrw_core::i18n::{Catalog, Locale};
use btrw_core::model::{PanelSide, SnapshotKind, Source};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;


pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub cli_lang: Option<String>,
    /// Runtime-only `--snapshots-dir` override (never persisted when saving F9 settings).
    pub cli_snapshots_dir: Option<PathBuf>,
    /// Auto-discovered Btrfs snapshot container roots (always protected from writes).
    pub auto_snapshot_roots: Vec<PathBuf>,
    pub catalog: Catalog,
    /// Discovered Btrfs subvolume groups ordered as a virtual nesting tree.
    pub groups: Vec<SubvolumeTreeEntry>,
    pub active_group: Option<usize>,
    /// Chronological list of selectable sources for the active subvolume (oldest -> newest -> LIVE).
    pub timeline: Vec<Source>,
    pub left_idx: usize,
    pub right_idx: usize,
    pub active_side: PanelSide,
    pub rel_dir: PathBuf,
    pub compare: DirCompareResult,
    pub sort_order: SortOrder,
    /// Selected row index within visible items (0 is `..` when `rel_dir` is non-empty).
    pub selected: usize,
    pub scroll: usize,
    pub modal_scroll: usize,
    pub filter: String,
    pub hide_unchanged: bool,
    pub layout: LayoutRects,
    pub last_click: Option<(Instant, u16, u16)>,
    pub notice: Option<String>,
    pub uid_names: HashMap<u32, String>,
    pub gid_names: HashMap<u32, String>,
    pub du_cache: HashMap<PathBuf, Result<SnapshotUsage, String>>,
    pub du_job: Option<DuJob>,
    pub dir_stats: HashMap<PathBuf, DirStats>,
    pub dir_stats_job: Option<DirStatsJob>,
    pub dir_stats_queue: VecDeque<PathBuf>,
    pub dir_load_gen: u64,
    pub dir_load_job: Option<DirLoadJob>,
    pub export_job: Option<ExportPatchJob>,
    /// Cancelled/superseded export jobs kept alive until their worker finishes so any
    /// patch already published to disk is still announced to the user.
    pub pending_exports: Vec<ExportPatchJob>,
    pub flat_mode: Option<(PathBuf, bool)>,
    pub flat_job: Option<FlatJob>,
    pub tt_job: Option<TimeTravelJob>,
    pub mode: Mode,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        config: Config,
        config_path: PathBuf,
        cli_lang: Option<String>,
        cli_snapshots_dir: Option<PathBuf>,
        catalog: Catalog,
        left_override: Option<PathBuf>,
        right_override: Option<PathBuf>,
    ) -> Self {
        let hide_unchanged = config.panels.hide_unchanged;
        let sort_order = config.panels.default_sort;
        let uid_names = load_id_map("/etc/passwd");
        let gid_names = load_id_map("/etc/group");
        let auto_snapshot_roots = auto_snapshot_roots();

        if let (Some(l), Some(r)) = (left_override, right_override) {
            let l_label = l
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| l.display().to_string());
            let r_label = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.display().to_string());
            let timeline = vec![
                Source::CustomDir {
                    path: l,
                    label: l_label,
                },
                Source::CustomDir {
                    path: r,
                    label: r_label,
                },
            ];
            let mut app = Self {
                config,
                config_path,
                cli_lang,
                cli_snapshots_dir,
                auto_snapshot_roots,
                catalog,
                groups: Vec::new(),
                active_group: None,
                timeline,
                left_idx: 0,
                right_idx: 1,
                active_side: PanelSide::Left,
                rel_dir: PathBuf::new(),
                compare: DirCompareResult::default(),
                sort_order,
                selected: 0,
                scroll: 0,
                modal_scroll: 0,
                filter: String::new(),
                hide_unchanged,
                layout: LayoutRects::default(),
                last_click: None,
                notice: None,
                uid_names,
                gid_names,
                du_cache: HashMap::new(),
                du_job: None,
                dir_stats: HashMap::new(),
                dir_stats_job: None,
                dir_stats_queue: VecDeque::new(),
                dir_load_gen: 0,
                dir_load_job: None,
                export_job: None,
                pending_exports: Vec::new(),
                flat_mode: None,
                flat_job: None,
                tt_job: None,
                mode: Mode::Browse,
                should_quit: false,
            };
            app.reload_dir();
            return app;
        }

        let snaps_dir = cli_snapshots_dir
            .as_deref()
            .unwrap_or(config.general.snapshots_dir.as_path());
        let groups = discover_subvolume_tree(snaps_dir);
        let mut app = Self {
            config,
            config_path,
            cli_lang,
            cli_snapshots_dir,
            auto_snapshot_roots,
            catalog,
            groups,
            active_group: None,
            timeline: Vec::new(),
            left_idx: 0,
            right_idx: 0,
            active_side: PanelSide::Left,
            rel_dir: PathBuf::new(),
            compare: DirCompareResult::default(),
            sort_order,
            selected: 0,
            scroll: 0,
            modal_scroll: 0,
            filter: String::new(),
            hide_unchanged,
            layout: LayoutRects::default(),
            last_click: None,
            notice: None,
            uid_names,
            gid_names,
            du_cache: HashMap::new(),
            du_job: None,
            dir_stats: HashMap::new(),
            dir_stats_job: None,
            dir_stats_queue: VecDeque::new(),
            dir_load_gen: 0,
            dir_load_job: None,
            export_job: None,
            pending_exports: Vec::new(),
            flat_mode: None,
            flat_job: None,
            tt_job: None,
            mode: Mode::Browse,
            should_quit: false,
        };

        if app.groups.len() == 1 {
            app.select_group(0);
        } else if app.groups.len() > 1 {
            app.mode = Mode::SubvolumePicker {
                selected: 0,
                initial: true,
            };
        }
        app
    }

    pub fn effective_snapshots_dir(&self) -> &std::path::Path {
        self.cli_snapshots_dir
            .as_deref()
            .unwrap_or(self.config.general.snapshots_dir.as_path())
    }

    /// Returns all snapshot root directories that must be protected from writes:
    /// the configured/`--snapshots-dir` directory (if non-empty) plus all auto-discovered
    /// snapshot containers (so F6 and SaveAs/ExportPatch block writes into `@snapshots`
    /// even when `general.snapshots_dir` is `""`).
    pub fn protected_snapshot_dirs(&self) -> Vec<PathBuf> {
        Self::collect_protected_snapshot_dirs(
            self.cli_snapshots_dir.as_deref(),
            self.config.general.snapshots_dir.as_path(),
            &self.auto_snapshot_roots,
        )
    }

    pub fn collect_protected_snapshot_dirs(
        cli_snapshots_dir: Option<&std::path::Path>,
        config_snapshots_dir: &std::path::Path,
        auto_snapshot_roots: &[PathBuf],
    ) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let eff = cli_snapshots_dir.unwrap_or(config_snapshots_dir);
        if !eff.as_os_str().is_empty() {
            dirs.push(eff.to_path_buf());
        }
        for p in auto_snapshot_roots {
            if !p.as_os_str().is_empty() && !dirs.iter().any(|d| d == p) {
                dirs.push(p.clone());
            }
        }
        dirs
    }

    /// Locks both panels to the chosen subvolume and defaults both sides to the LIVE state
    /// (or to the latest two snapshots when the group has no accessible live directory).
    pub fn select_group(&mut self, group_idx: usize) {
        let Some(entry) = self.groups.get(group_idx) else {
            return;
        };
        let group = &entry.group;
        let target = group.live_target.clone();
        let mut sources: Vec<Source> = group
            .snapshots
            .iter()
            .cloned()
            .map(Source::Snapshot)
            .collect();
        let has_live = target.is_absolute() && target.is_dir();
        let (left_idx, right_idx) = if has_live {
            sources.push(Source::Live {
                path: target.clone(),
                label: format!(
                    "{} ({})",
                    self.catalog.t("source-live"),
                    target.display()
                ),
            });
            let live_idx = sources.len().saturating_sub(1);
            (live_idx, live_idx)
        } else {
            let right_idx = sources.len().saturating_sub(1);
            let left_idx = right_idx.saturating_sub(1);
            (left_idx, right_idx)
        };
        self.cancel_timetravel_job();
        self.active_group = Some(group_idx);
        self.timeline = sources;
        self.left_idx = left_idx;
        self.right_idx = right_idx;
        self.rel_dir.clear();
        self.selected = 0;
        self.scroll = 0;
        self.filter.clear();
        self.mode = Mode::Browse;
        self.reload_dir();
    }

    pub fn verified_live_target(&self) -> Option<&PathBuf> {
        self.active_group
            .and_then(|idx| self.groups.get(idx))
            .filter(|e| e.group.verified_live_root)
            .map(|e| &e.group.live_target)
    }

    pub fn active_group_target(&self) -> Option<&PathBuf> {
        self.active_group
            .and_then(|idx| self.groups.get(idx))
            .map(|e| &e.group.live_target)
    }

    pub fn open_subvolume_picker(&mut self) {
        if self.groups.is_empty() {
            return;
        }
        self.mode = Mode::SubvolumePicker {
            selected: self.active_group.unwrap_or(0),
            initial: false,
        };
    }

    pub fn left_source(&self) -> Option<&Source> {
        self.timeline.get(self.left_idx)
    }

    pub fn right_source(&self) -> Option<&Source> {
        self.timeline.get(self.right_idx)
    }

    pub fn has_parent(&self) -> bool {
        self.flat_mode.is_none() && !self.rel_dir.as_os_str().is_empty()
    }


    pub fn toggle_sort(&mut self, key: char) {
        self.sort_order = match (key, self.sort_order) {
            ('n', SortOrder::NameAsc) => SortOrder::NameDesc,
            ('n', _) => SortOrder::NameAsc,
            ('s', SortOrder::SizeDesc) => SortOrder::SizeAsc,
            ('s', _) => SortOrder::SizeDesc,
            ('c', SortOrder::DeltaDesc) => SortOrder::DeltaAsc,
            ('c', _) => SortOrder::DeltaDesc,
            ('t', SortOrder::MtimeDesc) => SortOrder::MtimeAsc,
            ('t', _) => SortOrder::MtimeDesc,
            _ => return,
        };
        sort_pane_rows(&mut self.compare.rows, self.sort_order);
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn sort_label(&self) -> String {
        let key = match self.sort_order {
            SortOrder::NameAsc => "sort-name-asc",
            SortOrder::NameDesc => "sort-name-desc",
            SortOrder::SizeDesc => "sort-size-desc",
            SortOrder::SizeAsc => "sort-size-asc",
            SortOrder::DeltaDesc => "sort-delta-desc",
            SortOrder::DeltaAsc => "sort-delta-asc",
            SortOrder::MtimeDesc => "sort-mtime-desc",
            SortOrder::MtimeAsc => "sort-mtime-asc",
        };
        self.catalog.t(key)
    }

    pub const CONFIG_ITEM_COUNT: usize = 13;

    pub fn config_item_row(&self, idx: usize) -> (String, String) {
        let on = || self.catalog.t("val-on");
        let off = || self.catalog.t("val-off");
        let flag = |b: bool| if b { on() } else { off() };
        match idx {
            0 => (
                self.catalog.t("cfg-language"),
                self.config.general.language.clone(),
            ),
            1 => (
                self.catalog.t("cfg-theme"),
                match self.config.general.theme {
                    Theme::McBlue => "mc_blue",
                    Theme::Dark => "dark",
                    Theme::Terminal => "terminal",
                }
                .to_string(),
            ),
            2 => (
                self.catalog.t("cfg-hide-unchanged"),
                flag(self.config.panels.hide_unchanged),
            ),
            3 => (
                self.catalog.t("cfg-show-size"),
                flag(self.config.panels.show_size),
            ),
            4 => (
                self.catalog.t("cfg-show-delta"),
                flag(self.config.panels.show_delta),
            ),
            5 => (
                self.catalog.t("cfg-show-mtime"),
                flag(self.config.panels.show_mtime),
            ),
            6 => (
                self.catalog.t("cfg-show-permissions"),
                match self.config.panels.show_permissions {
                    PermissionsColumn::Off => self.catalog.t("perm-off"),
                    PermissionsColumn::Octal => self.catalog.t("perm-octal"),
                    PermissionsColumn::OctalOwnerGroup => self.catalog.t("perm-octal-owner"),
                },
            ),
            7 => (
                self.catalog.t("cfg-mouse"),
                flag(self.config.panels.mouse_enabled),
            ),
            8 => (
                self.catalog.t("cfg-show-metadata"),
                flag(self.config.diff.show_metadata_only),
            ),
            9 => (
                self.catalog.t("cfg-diff-format"),
                match self.config.diff.format {
                    DiffFormat::Unified => "unified",
                    DiffFormat::SideBySide => "side_by_side",
                }
                .to_string(),
            ),
            10 => (
                self.catalog.t("cfg-ext-diff"),
                self.config
                    .diff
                    .external_diff_cmd
                    .clone()
                    .unwrap_or_else(|| "auto".to_string()),
            ),
            11 => (
                self.catalog.t("cfg-create-backup"),
                flag(self.config.restore.create_backup),
            ),
            _ => (
                self.catalog.t("cfg-live-strategy"),
                match self.config.btrfs.live_strategy {
                    LiveDiffStrategy::ReadOnlyFiemap => "read_only_fiemap",
                    LiveDiffStrategy::TempRoSnapshot => "temp_ro_snapshot",
                }
                .to_string(),
            ),
        }
    }

    /// Toggles/cycles config item `idx` and persists immediately to `config_path`.
    pub fn cycle_config_item(&mut self, idx: usize) {
        match idx {
            0 => {
                self.config.general.language = match self.config.general.language.as_str() {
                    "auto" => "en",
                    "en" => "cs",
                    _ => "auto",
                }
                .to_string();
                let locale = Locale::resolve(
                    self.cli_lang.as_deref(),
                    Some(&self.config.general.language),
                );
                self.catalog = Catalog::new(locale);
            }
            1 => {
                self.config.general.theme = match self.config.general.theme {
                    Theme::McBlue => Theme::Dark,
                    Theme::Dark => Theme::Terminal,
                    Theme::Terminal => Theme::McBlue,
                };
            }
            2 => {
                self.config.panels.hide_unchanged = !self.config.panels.hide_unchanged;
                self.hide_unchanged = self.config.panels.hide_unchanged;
                self.clamp_selection();
            }
            3 => self.config.panels.show_size = !self.config.panels.show_size,
            4 => self.config.panels.show_delta = !self.config.panels.show_delta,
            5 => self.config.panels.show_mtime = !self.config.panels.show_mtime,
            6 => {
                self.config.panels.show_permissions = match self.config.panels.show_permissions {
                    PermissionsColumn::Off => PermissionsColumn::Octal,
                    PermissionsColumn::Octal => PermissionsColumn::OctalOwnerGroup,
                    PermissionsColumn::OctalOwnerGroup => PermissionsColumn::Off,
                };
            }
            7 => self.config.panels.mouse_enabled = !self.config.panels.mouse_enabled,
            8 => self.config.diff.show_metadata_only = !self.config.diff.show_metadata_only,
            9 => {
                self.config.diff.format = match self.config.diff.format {
                    DiffFormat::Unified => DiffFormat::SideBySide,
                    DiffFormat::SideBySide => DiffFormat::Unified,
                };
            }
            10 => {
                self.config.diff.external_diff_cmd =
                    match self.config.diff.external_diff_cmd.as_deref() {
                        None => Some("meld".to_string()),
                        Some("meld") => Some("kdiff3".to_string()),
                        Some("kdiff3") => Some("code --diff".to_string()),
                        Some("code --diff") => Some("vimdiff".to_string()),
                        _ => None,
                    };
            }
            11 => self.config.restore.create_backup = !self.config.restore.create_backup,
            12 => {
                self.config.btrfs.live_strategy = match self.config.btrfs.live_strategy {
                    LiveDiffStrategy::ReadOnlyFiemap => LiveDiffStrategy::TempRoSnapshot,
                    LiveDiffStrategy::TempRoSnapshot => LiveDiffStrategy::ReadOnlyFiemap,
                };
            }
            _ => return,
        }
        match self.config.save(&self.config_path) {
            Ok(()) => self.notice = Some(self.catalog.t("config-saved")),
            Err(e) => {
                self.notice = Some(
                    self.catalog
                        .t_args("config-save-failed", &[("detail", e.to_string().into())]),
                )
            }
        }
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        self.compare
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                if self.hide_unchanged && !row.is_dir() && row.status.is_none() {
                    return false;
                }
                if !q.is_empty() && !row.name.to_lowercase().contains(&q) {
                    return false;
                }
                true
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn total_visible_items(&self) -> usize {
        self.visible_indices().len() + usize::from(self.has_parent())
    }

    pub fn selected_row(&self) -> Option<&PaneRow> {
        let offset = usize::from(self.has_parent());
        if self.selected < offset {
            return None;
        }
        let vis = self.visible_indices();
        let row_idx = *vis.get(self.selected - offset)?;
        self.compare.rows.get(row_idx)
    }

    pub fn clamp_selection(&mut self) {
        let total = self.total_visible_items();
        if total == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else if self.selected >= total {
            self.selected = total - 1;
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let total = self.total_visible_items();
        if total == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, (total - 1) as isize) as usize;
        self.selected = next;
    }

    pub fn step_timeline(&mut self, delta: isize) {
        if self.timeline.is_empty() {
            return;
        }
        let max = (self.timeline.len() - 1) as isize;
        match self.active_side {
            PanelSide::Left => {
                let next = (self.left_idx as isize + delta).clamp(0, max) as usize;
                if next != self.left_idx {
                    self.left_idx = next;
                    self.reload_dir();
                }
            }
            PanelSide::Right => {
                let next = (self.right_idx as isize + delta).clamp(0, max) as usize;
                if next != self.right_idx {
                    self.right_idx = next;
                    self.reload_dir();
                }
            }
        }
    }

    pub fn step_timeline_day(&mut self, delta: isize) {
        if self.timeline.len() < 2 {
            return;
        }
        let cur_idx = match self.active_side {
            PanelSide::Left => self.left_idx,
            PanelSide::Right => self.right_idx,
        };
        let cur_day = source_day_key(&self.timeline[cur_idx]);
        let mut idx = cur_idx as isize + delta.signum();
        while idx >= 0 && (idx as usize) < self.timeline.len() {
            if source_day_key(&self.timeline[idx as usize]) != cur_day {
                let target = idx as usize;
                match self.active_side {
                    PanelSide::Left => self.left_idx = target,
                    PanelSide::Right => self.right_idx = target,
                }
                self.reload_dir();
                return;
            }
            idx += delta.signum();
        }
    }

    pub fn swap_panels(&mut self) {
        std::mem::swap(&mut self.left_idx, &mut self.right_idx);
        self.reload_dir();
    }

    pub fn go_parent(&mut self) {
        // Lock navigation strictly inside the selected subvolume root.
        if self.rel_dir.as_os_str().is_empty() {
            return;
        }
        if let Some(parent) = self.rel_dir.parent() {
            self.rel_dir = parent.to_path_buf();
            self.selected = 0;
            self.scroll = 0;
            self.reload_dir();
        }
    }

    pub fn activate_selected(&mut self) {
        if self.has_parent() && self.selected == 0 {
            self.go_parent();
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if row.is_dir() {
            self.rel_dir = row.rel_path;
            self.selected = 0;
            self.scroll = 0;
            self.filter.clear();
            self.reload_dir();
        } else {
            self.open_diff_for_selected();
        }
    }



    pub fn open_picker(&mut self, side: PanelSide) {
        if self.timeline.is_empty() {
            return;
        }
        let active_idx = match side {
            PanelSide::Left => self.left_idx,
            PanelSide::Right => self.right_idx,
        };
        let mut expanded_days = BTreeSet::new();
        if let Some(day) = source_day_key(&self.timeline[active_idx]) {
            expanded_days.insert(day);
        } else if let Some(latest_day) = self
            .timeline
            .iter()
            .rev()
            .find_map(source_day_key)
        {
            expanded_days.insert(latest_day);
        }
        let entries = self.build_picker_entries(&expanded_days);
        let selected = entries
            .iter()
            .position(|e| match e {
                PickerEntry::Live { timeline_idx, .. }
                | PickerEntry::Snapshot { timeline_idx, .. } => *timeline_idx == active_idx,
                PickerEntry::Day { .. } => false,
            })
            .unwrap_or(0);
        self.mode = Mode::SourcePicker {
            side,
            selected,
            expanded_days,
        };
    }

    /// Builds the Day -> Hour hierarchical list for the SourcePicker modal (newest day first).
    pub fn build_picker_entries(&self, expanded_days: &BTreeSet<String>) -> Vec<PickerEntry> {
        build_picker_entries_from(&self.timeline, &self.catalog, expanded_days)
    }
}

pub fn build_picker_entries_from(
    timeline: &[Source],
    catalog: &Catalog,
    expanded_days: &BTreeSet<String>,
) -> Vec<PickerEntry> {
    let mut entries = Vec::new();
    let mut by_day: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut undated: Vec<usize> = Vec::new();

    for (idx, src) in timeline.iter().enumerate() {
        match src {
            Source::Live { label, .. } | Source::CustomDir { label, .. } => {
                entries.push(PickerEntry::Live {
                    timeline_idx: idx,
                    label: label.clone(),
                });
            }
            Source::Snapshot(info) => {
                if let Some(ts) = info.created_at {
                    by_day.entry(format_day_local(ts)).or_default().push(idx);
                } else {
                    undated.push(idx);
                }
            }
        }
    }

    // Newest days first, newest snapshots within each day first.
    for (day_key, mut idxs) in by_day.into_iter().rev() {
        idxs.reverse();
        let expanded = expanded_days.contains(&day_key);
        entries.push(PickerEntry::Day {
            day_key: day_key.clone(),
            display_day: day_key.clone(),
            count: idxs.len(),
            expanded,
        });
        if expanded {
            for timeline_idx in idxs {
                let src = &timeline[timeline_idx];
                let time_hm = match src {
                    Source::Snapshot(info) => info
                        .created_at
                        .map(format_hm_local)
                        .unwrap_or_else(|| "--:--".to_string()),
                    _ => "--:--".to_string(),
                };
                entries.push(PickerEntry::Snapshot {
                    timeline_idx,
                    day_key: day_key.clone(),
                    time_hm,
                    label: source_label(src).to_string(),
                    marker: source_marker(src),
                });
            }
        }
    }

    if !undated.is_empty() {
        let day_key = "__undated__".to_string();
        let expanded = expanded_days.contains(&day_key);
        entries.push(PickerEntry::Day {
            day_key: day_key.clone(),
            display_day: catalog.t("picker-unknown-date"),
            count: undated.len(),
            expanded,
        });
        if expanded {
            for timeline_idx in undated.into_iter().rev() {
                let src = &timeline[timeline_idx];
                entries.push(PickerEntry::Snapshot {
                    timeline_idx,
                    day_key: day_key.clone(),
                    time_hm: "--:--".to_string(),
                    label: source_label(src).to_string(),
                    marker: source_marker(src),
                });
            }
        }
    }

    entries
}

fn load_id_map(path: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in content.lines() {
        let mut fields = line.split(':');
        let Some(name) = fields.next() else { continue };
        let _ = fields.next(); // password placeholder
        let Some(id_str) = fields.next() else { continue };
        if let Ok(id) = id_str.parse::<u32>() {
            map.entry(id).or_insert_with(|| name.to_string());
        }
    }
    map
}

fn info_line(text: String) -> DiffLine {
    DiffLine {
        kind: DiffLineKind::Info,
        text,
    }
}

pub fn source_day_key(source: &Source) -> Option<String> {
    match source {
        Source::Snapshot(info) => info.created_at.map(format_day_local),
        Source::Live { .. } | Source::CustomDir { .. } => None,
    }
}

pub fn source_label(source: &Source) -> &str {
    match source {
        Source::Snapshot(info) => &info.label,
        Source::Live { label, .. } | Source::CustomDir { label, .. } => label,
    }
}

/// Formats source label together with its creation date/time for panel headers.
pub fn source_header_label(source: &Source) -> String {
    match source {
        Source::Snapshot(info) => match info.created_at {
            Some(ts) => format!("{} ({})", info.label, format_system_time_local(ts)),
            None => info.label.clone(),
        },
        Source::Live { label, .. } | Source::CustomDir { label, .. } => label.clone(),
    }
}

/// Short date/time or label for the timeline summary line.
pub fn source_short_time(source: &Source) -> String {
    match source {
        Source::Snapshot(info) => match info.created_at {
            Some(ts) => format_system_time_local(ts),
            None => info.label.clone(),
        },
        Source::Live { label, .. } | Source::CustomDir { label, .. } => label.clone(),
    }
}

pub fn source_marker(source: &Source) -> &'static str {
    match source {
        Source::Snapshot(info) => match info.kind {
            SnapshotKind::Event => "●",
            SnapshotKind::Scheduled => "·",
            SnapshotKind::Manual => "◆",
        },
        Source::Live { .. } => "★",
        Source::CustomDir { .. } => "○",
    }
}

