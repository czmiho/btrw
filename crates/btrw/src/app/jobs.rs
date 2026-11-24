use super::{
    App, DirLoadJob, DirStatsJob, DuJob, DuMsg, FlatJob, FlatMsg, Mode, TimeTravelJob,
    TimeTravelState,
};
use btrw_core::btrfs_du::compute_snapshot_usage;
use btrw_core::btrfs_send::diff_snapshots_via_send;
use btrw_core::compare::{
    DirCompareResult, DirStats, PatchExportSummary, compare_directory_cancellable,
    scan_directory_stats, scan_flat_changes, sort_pane_rows,
};
use btrw_core::config::{LiveDiffStrategy, SortOrder};
use btrw_core::model::Source;
use btrw_core::temp_snapshot::TempRoSnapshot;
use btrw_core::timetravel::{TimeTravelHistory, discover_file_versions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

impl App {
    pub fn cancel_dir_load_job(&mut self) -> bool {
        if let Some(job) = self.dir_load_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn cancel_export_job(&mut self) -> bool {
        if let Some(job) = self.export_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            // Keep receiver alive so any patch already published to disk is still announced.
            self.pending_exports.push(job);
            true
        } else {
            false
        }
    }

    pub fn reload_dir(&mut self) {
        self.cancel_dir_load_job();
        self.cancel_dir_stats_job();
        self.cancel_flat_job();
        self.flat_mode = None;
        self.dir_stats.clear();
        self.dir_load_gen = self.dir_load_gen.wrapping_add(1);
        let generation = self.dir_load_gen;
        if let (Some(left), Some(right)) = (self.left_source(), self.right_source()) {
            let lpath = left.path().clone();
            let rpath = right.path().clone();
            let rel = self.rel_dir.clone();
            let show_meta = self.config.diff.show_metadata_only;
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_w = Arc::clone(&cancel);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let res = compare_directory_cancellable(&lpath, &rpath, &rel, show_meta, &cancel_w);
                if !cancel_w.load(Ordering::Relaxed) {
                    let _ = tx.send(res);
                }
            });
            // Wait briefly for fast directories so normal navigation has zero flicker,
            // while slow/hashed directories continue asynchronously without blocking keys.
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(mut res) => {
                    sort_pane_rows(&mut res.rows, self.sort_order);
                    self.compare = res;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.compare = DirCompareResult::default();
                    self.dir_load_job = Some(DirLoadJob {
                        generation,
                        cancel,
                        rx,
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.compare = DirCompareResult::default();
                    self.notice = Some(self.catalog.t("status-worker-error"));
                }
            }
        } else {
            self.compare = DirCompareResult::default();
        }
        self.clamp_selection();
    }


    pub fn start_du_job(&mut self, paths: Vec<PathBuf>, force: bool) {
        self.cancel_du_job();
        let targets: Vec<PathBuf> = if force {
            for p in &paths {
                self.du_cache.remove(p);
            }
            paths
        } else {
            paths
                .into_iter()
                .filter(|p| !self.du_cache.contains_key(p))
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let total = targets.len();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            for path in targets {
                if cancel_worker.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(DuMsg::Started(path.clone()));
                let res = compute_snapshot_usage(&path, &cancel_worker);
                if cancel_worker.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(DuMsg::Result(path, res));
            }
            let _ = tx.send(DuMsg::Finished);
        });

        self.du_job = Some(DuJob {
            cancel,
            rx,
            total,
            done: 0,
            current: None,
        });
    }

    pub fn cancel_du_job(&mut self) -> bool {
        if let Some(job) = self.du_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn start_dir_stats_for_selected(&mut self) {
        if self.flat_mode.is_some() {
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if !row.is_dir() {
            return;
        }
        if self.left_source().is_none() || self.right_source().is_none() {
            return;
        }
        let rel = row.rel_path;
        // Avoid duplicating an already active or queued directory scan.
        if self
            .dir_stats_job
            .as_ref()
            .is_some_and(|j| j.active_rel == rel)
            || self.dir_stats_queue.iter().any(|p| p == &rel)
        {
            return;
        }
        self.dir_stats.insert(rel.clone(), DirStats::default());
        self.dir_stats_queue.push_back(rel);
        self.pump_dir_stats_queue();
    }

    pub(super) fn pump_dir_stats_queue(&mut self) {
        if self.dir_stats_job.is_some() {
            return;
        }
        let Some(rel) = self.dir_stats_queue.pop_front() else {
            return;
        };
        let (lpath, rpath) = match (self.left_source(), self.right_source()) {
            (Some(left), Some(right)) => (left.path().clone(), right.path().clone()),
            _ => {
                self.dir_stats_queue.clear();
                return;
            }
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let active_rel = rel.clone();

        thread::spawn(move || {
            let rel_send = rel.clone();
            scan_directory_stats(&lpath, &rpath, &rel, &cancel_worker, |stats| {
                let _ = tx.send((rel_send.clone(), stats.clone()));
            });
        });

        self.dir_stats_job = Some(DirStatsJob {
            active_rel,
            cancel,
            rx,
        });
    }

    pub fn cancel_dir_stats_job(&mut self) -> bool {
        let had_queued = !self.dir_stats_queue.is_empty();
        for queued in self.dir_stats_queue.drain(..) {
            if self.dir_stats.get(&queued).is_some_and(|s| !s.done) {
                self.dir_stats.remove(&queued);
            }
        }
        if let Some(job) = self.dir_stats_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            had_queued
        }
    }

    pub fn start_flat_scan(&mut self, scope: PathBuf) {
        let (lpath, rpath, left_ro, right_ro, left_live, right_live) =
            match (self.left_source(), self.right_source()) {
                (Some(left), Some(right)) => (
                    left.path().clone(),
                    right.path().clone(),
                    left.is_read_only(),
                    right.is_read_only(),
                    matches!(left, Source::Live { .. }),
                    matches!(right, Source::Live { .. }),
                ),
                _ => return,
            };
        self.cancel_flat_job();
        self.cancel_dir_stats_job();
        let show_metadata = self.config.diff.show_metadata_only;
        let use_temp_ro = self.config.btrfs.live_strategy == LiveDiffStrategy::TempRoSnapshot;
        let snaps_dir = self.effective_snapshots_dir().to_path_buf();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        let scope_worker = scope.clone();

        thread::spawn(move || {
            let mut used_send = false;
            let both_ro = left_ro && right_ro && lpath != rpath;
            let can_temp_ro = use_temp_ro
                && lpath != rpath
                && ((left_ro && right_live) || (right_ro && left_live));

            let send_attempt = if both_ro {
                Some(diff_snapshots_via_send(
                    &lpath,
                    &rpath,
                    &scope_worker,
                    show_metadata,
                    &cancel_worker,
                    |scanned, path| {
                        let _ = tx.send(FlatMsg::Progress(scanned, path.display().to_string()));
                    },
                ))
            } else if can_temp_ro {
                let live_path = if right_live { &rpath } else { &lpath };
                match TempRoSnapshot::create(live_path, &snaps_dir) {
                    Ok(temp_snap) => {
                        let (sl, sr) = if right_live {
                            (lpath.as_path(), temp_snap.path())
                        } else {
                            (temp_snap.path(), rpath.as_path())
                        };
                        let r = diff_snapshots_via_send(
                            sl,
                            sr,
                            &scope_worker,
                            show_metadata,
                            &cancel_worker,
                            |scanned, path| {
                                let _ =
                                    tx.send(FlatMsg::Progress(scanned, path.display().to_string()));
                            },
                        );
                        // Explicit drop ensures immediate deletion before sending results to UI.
                        drop(temp_snap);
                        Some(r)
                    }
                    Err(_) => None,
                }
            } else {
                None
            };

            let res = match send_attempt {
                Some(Ok(r)) => {
                    used_send = true;
                    r
                }
                Some(Err(_)) if cancel_worker.load(Ordering::Relaxed) => return,
                _ => scan_flat_changes(
                    &lpath,
                    &rpath,
                    &scope_worker,
                    show_metadata,
                    &cancel_worker,
                    |scanned, path| {
                        let _ = tx.send(FlatMsg::Progress(scanned, path.display().to_string()));
                    },
                ),
            };
            if !cancel_worker.load(Ordering::Relaxed) {
                let _ = tx.send(FlatMsg::Done(scope_worker, res, used_send));
            }
        });

        self.flat_job = Some(FlatJob {
            cancel,
            rx,
            scanned: 0,
            current_path: String::new(),
        });
    }

    pub fn cancel_flat_job(&mut self) -> bool {
        if let Some(job) = self.flat_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn start_timetravel_for_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if row.is_dir() || self.timeline.is_empty() {
            return;
        }
        self.cancel_timetravel_job();
        let sources = self.timeline.clone();
        let total_sources = sources.len();
        let rel_path = row.rel_path;
        let rel_worker = rel_path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let hist = discover_file_versions(&sources, &rel_worker, &cancel_worker);
            if !cancel_worker.load(Ordering::Relaxed) {
                let _ = tx.send(hist);
            }
        });

        self.tt_job = Some(TimeTravelJob {
            cancel,
            rx,
            rel_path,
            total_sources,
            group_idx: self.active_group,
            verified_live_target: self.verified_live_target().cloned(),
        });
    }

    pub fn cancel_timetravel_job(&mut self) -> bool {
        if let Some(job) = self.tt_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn cancel_all_jobs(&mut self) {
        self.cancel_du_job();
        self.cancel_dir_load_job();
        self.cancel_dir_stats_job();
        self.cancel_export_job();
        self.cancel_flat_job();
        self.cancel_timetravel_job();
    }

    pub fn poll_background_jobs(&mut self) {
        let mut dir_load_result: Option<(u64, DirCompareResult)> = None;
        let mut dir_load_disconnected = false;
        if let Some(job) = &self.dir_load_job {
            match job.rx.try_recv() {
                Ok(res) => dir_load_result = Some((job.generation, res)),
                Err(mpsc::TryRecvError::Disconnected) => dir_load_disconnected = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some((generation, mut res)) = dir_load_result {
            self.dir_load_job = None;
            if generation == self.dir_load_gen && self.flat_mode.is_none() {
                sort_pane_rows(&mut res.rows, self.sort_order);
                self.compare = res;
                self.clamp_selection();
            }
        } else if dir_load_disconnected {
            self.dir_load_job = None;
            self.notice = Some(self.catalog.t("status-worker-error"));
        }

        let mut export_result: Option<Result<(PatchExportSummary, PathBuf), String>> = None;
        let mut export_disconnected = false;
        if let Some(job) = &self.export_job {
            match job.rx.try_recv() {
                Ok(res) => export_result = Some(res),
                Err(mpsc::TryRecvError::Disconnected) => export_disconnected = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(res) = export_result {
            self.export_job = None;
            match res {
                Ok((summary, output_path)) => {
                    self.notice = Some(self.catalog.t_args(
                        "export-ok",
                        &[
                            ("exported", (summary.files_exported as i64).into()),
                            ("path", output_path.display().to_string().into()),
                            ("skipped", (summary.files_skipped as i64).into()),
                        ],
                    ));
                }
                Err(detail) => {
                    self.notice = Some(
                        self.catalog
                            .t_args("export-err", &[("detail", detail.into())]),
                    );
                }
            }
        } else if export_disconnected {
            self.export_job = None;
            self.notice = Some(self.catalog.t("status-worker-error"));
        }

        // Poll cancelled/superseded export jobs so any file already published to disk is still announced.
        if !self.pending_exports.is_empty() {
            let mut still_pending = Vec::new();
            let mut announced: Option<(PatchExportSummary, PathBuf)> = None;
            for job in self.pending_exports.drain(..) {
                match job.rx.try_recv() {
                    Ok(Ok((summary, output_path))) => announced = Some((summary, output_path)),
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {}
                    Err(mpsc::TryRecvError::Empty) => still_pending.push(job),
                }
            }
            self.pending_exports = still_pending;
            if let Some((summary, output_path)) = announced {
                self.notice = Some(self.catalog.t_args(
                    "export-ok",
                    &[
                        ("exported", (summary.files_exported as i64).into()),
                        ("path", output_path.display().to_string().into()),
                        ("skipped", (summary.files_skipped as i64).into()),
                    ],
                ));
            }
        }
        let mut du_finished = false;
        if let Some(job) = &mut self.du_job {
            loop {
                match job.rx.try_recv() {
                    Ok(DuMsg::Started(p)) => job.current = Some(p),
                    Ok(DuMsg::Result(p, res)) => {
                        self.du_cache.insert(p, res);
                        job.done += 1;
                        job.current = None;
                    }
                    Ok(DuMsg::Finished) | Err(mpsc::TryRecvError::Disconnected) => {
                        du_finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if du_finished {
            self.du_job = None;
        }

        let mut dir_done = false;
        if let Some(job) = &mut self.dir_stats_job {
            loop {
                match job.rx.try_recv() {
                    Ok((rel, stats)) => {
                        if stats.done {
                            dir_done = true;
                        }
                        self.dir_stats.insert(rel, stats);
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        dir_done = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if dir_done {
            self.dir_stats_job = None;
            self.pump_dir_stats_queue();
        }

        let mut flat_result: Option<(PathBuf, DirCompareResult, bool)> = None;
        let mut flat_disconnected = false;
        if let Some(job) = &mut self.flat_job {
            loop {
                match job.rx.try_recv() {
                    Ok(FlatMsg::Progress(n, p)) => {
                        job.scanned = n;
                        job.current_path = p;
                    }
                    Ok(FlatMsg::Done(scope, res, used_send)) => {
                        flat_result = Some((scope, res, used_send));
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        flat_disconnected = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if let Some((scope, mut res, used_send)) = flat_result {
            self.flat_job = None;
            self.flat_mode = Some((scope, used_send));
            self.sort_order = SortOrder::DeltaDesc;
            sort_pane_rows(&mut res.rows, self.sort_order);
            self.compare = res;
            self.selected = 0;
            self.scroll = 0;
            self.filter.clear();
        } else if flat_disconnected {
            self.flat_job = None;
            self.notice = Some(self.catalog.t("status-worker-error"));
        }

        let mut tt_done: Option<(TimeTravelHistory, Option<usize>, Option<PathBuf>)> = None;
        let mut tt_disconnected = false;
        if let Some(job) = &self.tt_job {
            match job.rx.try_recv() {
                Ok(hist) => {
                    tt_done = Some((hist, job.group_idx, job.verified_live_target.clone()));
                }
                Err(mpsc::TryRecvError::Disconnected) => tt_disconnected = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some((history, group_idx, verified_live_target)) = tt_done {
            self.tt_job = None;
            // Discard stale Time Travel result if user switched groups OR opened another modal.
            if group_idx != self.active_group || !matches!(self.mode, Mode::Browse) {
                return;
            }
            if !history.errors.is_empty() {
                self.notice = Some(format!(
                    "Time Travel partial: {} read error(s) ({})",
                    history.errors.len(),
                    history.errors[0]
                ));
            }
            if history.versions.is_empty() {
                self.notice = Some(self.catalog.t("tt-empty"));
            } else {
                let active_idx = history.versions.len() - 1;
                let mut state = TimeTravelState {
                    history,
                    group_idx,
                    verified_live_target,
                    active_idx,
                    pinned: Vec::new(),
                    highlight: self.config.diff.timetravel_highlight,
                    content_cache: HashMap::new(),
                    rendered: Vec::new(),
                    scroll_y: 0,
                    scroll_x: 0,
                };
                state.rebuild_lines(
                    self.config.diff.max_file_bytes,
                    self.config.diff.max_lines,
                    true,
                );
                self.mode = Mode::TimeTravel(Box::new(state));
            }
        } else if tt_disconnected {
            self.tt_job = None;
            self.notice = Some(self.catalog.t("status-worker-error"));
        }
    }
}
