use super::{
    App, ExportFocus, ExportPatchJob, ExportPatchState, ExportScope, Mode, TimeTravelState,
    info_line,
};
use btrw_core::compare::{
    DiffLine, DiffLineKind, DirCompareResult, FileDiffOutcome, PaneRow, diff_file,
    diff_file_side_by_side, export_patch_cancellable, scan_flat_changes,
};
use btrw_core::config::DiffFormat;
use btrw_core::fs_safe::open_file_safe;
use btrw_core::restore::RestorePlan;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

impl App {
    pub fn open_diff_for_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let (Some(left), Some(right)) = (self.left_source(), self.right_source()) else {
            return;
        };
        let outcome = diff_file(
            left.path(),
            right.path(),
            &row,
            self.config.diff.max_file_bytes,
            self.config.diff.max_lines,
        );
        let lines = match outcome {
            FileDiffOutcome::Unified(lines) => lines,
            FileDiffOutcome::Identical => vec![info_line(self.catalog.t("diff-identical"))],
            FileDiffOutcome::Binary => vec![info_line(self.catalog.t("diff-binary"))],
            FileDiffOutcome::TooLarge => vec![info_line(self.catalog.t("diff-too-large"))],
            FileDiffOutcome::TooManyLines => vec![info_line(self.catalog.t("diff-too-many-lines"))],
            FileDiffOutcome::NonRegular => vec![info_line(self.catalog.t("diff-non-regular"))],
            FileDiffOutcome::Error(detail) => vec![info_line(
                self.catalog.t_args("diff-error", &[("detail", detail.into())]),
            )],
        };
        let side_rows = diff_file_side_by_side(
            left.path(),
            right.path(),
            &row,
            self.config.diff.max_file_bytes,
            self.config.diff.max_lines,
        )
        .ok();
        let side_by_side = side_rows.is_some() && self.config.diff.format == DiffFormat::SideBySide;
        let path_str = row.rel_path.display().to_string();
        let title = self
            .catalog
            .t_args("diff-title", &[("path", path_str.into())]);
        self.mode = Mode::DiffView {
            title,
            lines,
            side_rows,
            side_by_side,
            scroll_y: 0,
            scroll_x: 0,
            return_to_restore: None,
        };
    }

    /// Opens diff preview specifically between `plan.source_root/plan.rel_path` and `plan.dest_path`,
    /// returning back to `RestoreConfirm(plan)` on close.
    pub fn open_diff_for_restore_plan(&mut self, plan: RestorePlan) {
        let src_entry = btrw_core::compare::DirEntryInfo {
            name: plan
                .rel_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            rel_path: plan.rel_path.clone(),
            kind: btrw_core::model::EntryKind::File,
            size: Some(plan.source_sig.size),
            target: None,
            target_raw: None,
            sig: plan.source_sig,
        };
        let dst_entry = plan.dest_sig.map(|sig| btrw_core::compare::DirEntryInfo {
            name: plan
                .dest_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            rel_path: plan.dest_path.clone(),
            kind: btrw_core::model::EntryKind::File,
            size: Some(sig.size),
            target: None,
            target_raw: None,
            sig,
        });
        let left_bytes = btrw_core::fs_safe::read_file_limited(
            &plan.source_root,
            &plan.rel_path,
            self.config.diff.max_file_bytes,
        );
        let right_bytes = if dst_entry.is_some() {
            let dst_parent = plan.dest_path.parent().unwrap_or_else(|| std::path::Path::new("/"));
            let dst_name = plan
                .dest_path
                .file_name()
                .map(std::path::Path::new)
                .unwrap_or_else(|| std::path::Path::new(""));
            btrw_core::fs_safe::read_file_limited(
                dst_parent,
                dst_name,
                self.config.diff.max_file_bytes,
            )
        } else {
            Ok(Vec::new())
        };
        let _ = src_entry;

        let lines = match (left_bytes, right_bytes) {
            (Ok(lb), Ok(rb)) => {
                if lb.contains(&0) || rb.contains(&0) {
                    vec![info_line(self.catalog.t("diff-binary"))]
                } else if let (Ok(ls), Ok(rs)) = (String::from_utf8(lb), String::from_utf8(rb)) {
                    if ls == rs {
                        vec![info_line(self.catalog.t("diff-identical"))]
                    } else if ls.lines().count() + rs.lines().count() > self.config.diff.max_lines {
                        vec![info_line(self.catalog.t("diff-too-many-lines"))]
                    } else {
                        let diff = similar::TextDiff::from_lines(&ls, &rs);
                        let unified = diff
                            .unified_diff()
                            .context_radius(3)
                            .header(
                                &format!("snapshot/{}", plan.rel_path.display()),
                                &plan.dest_path.display().to_string(),
                            )
                            .to_string();
                        unified
                            .lines()
                            .map(|line| {
                                let kind = if line.starts_with("--- ") || line.starts_with("+++ ") {
                                    DiffLineKind::Header
                                } else if line.starts_with("@@") {
                                    DiffLineKind::Hunk
                                } else if line.starts_with('+') {
                                    DiffLineKind::Added
                                } else if line.starts_with('-') {
                                    DiffLineKind::Removed
                                } else {
                                    DiffLineKind::Context
                                };
                                DiffLine {
                                    kind,
                                    text: btrw_core::compare::escape_control_chars(line),
                                }
                            })
                            .collect()
                    }
                } else {
                    vec![info_line(self.catalog.t("diff-binary"))]
                }
            }
            (Err(e), _) | (_, Err(e)) => vec![info_line(
                self.catalog
                    .t_args("diff-error", &[("detail", e.to_string().into())]),
            )],
        };

        let title = self.catalog.t_args(
            "diff-title",
            &[("path", plan.dest_path.display().to_string().into())],
        );
        self.mode = Mode::DiffView {
            title,
            lines,
            side_rows: None,
            side_by_side: false,
            scroll_y: 0,
            scroll_x: 0,
            return_to_restore: Some(Box::new(plan)),
        };
    }

    pub fn open_export_patch(&mut self) {
        let selected_row = self.selected_row().cloned();
        let default_name = selected_row
            .as_ref()
            .and_then(|r| r.rel_path.file_name())
            .map(|n| format!("{}.patch", n.to_string_lossy()))
            .unwrap_or_else(|| "btrw.patch".to_string());
        let cursor = default_name.chars().count();

        let base_live = self
            .active_group_target()
            .cloned()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));
        let mut current_dir = if !self.rel_dir.as_os_str().is_empty() {
            base_live.join(&self.rel_dir)
        } else {
            std::env::current_dir().unwrap_or(base_live)
        };
        while !current_dir.is_dir() {
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                current_dir = PathBuf::from("/tmp");
                break;
            }
        }

        let mut state = ExportPatchState {
            scope: ExportScope::SelectedObject,
            selected_row,
            custom_roots: None,
            current_dir,
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: default_name,
            cursor,
            focus: ExportFocus::Filename,
        };
        let protected = self.protected_snapshot_dirs();
        state.reload_subdirs(&protected);
        self.mode = Mode::ExportPatch(Box::new(state));
    }

    pub fn open_export_patch_for_timetravel(&mut self, tt: &TimeTravelState) {
        let Some((a, b)) = tt.pinned_pair() else {
            self.notice = Some(self.catalog.t("tt-need-two-pinned"));
            return;
        };
        let va = &tt.history.versions[a];
        let vb = &tt.history.versions[b];
        let rel = &tt.history.rel_path;

        let mk_entry = |root: &PathBuf| -> Result<Option<btrw_core::compare::DirEntryInfo>, String> {
            match open_file_safe(root, rel) {
                Ok((_, sig)) => Ok(Some(btrw_core::compare::DirEntryInfo {
                    name: rel
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    rel_path: rel.clone(),
                    kind: btrw_core::model::EntryKind::File,
                    size: Some(sig.size),
                    target: None,
                    target_raw: None,
                    sig,
                })),
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        || e.raw_os_error() == Some(libc::ENOTDIR) =>
                {
                    Ok(None)
                }
                Err(e) => Err(format!("{}/{}: {e}", root.display(), rel.display())),
            }
        };
        let left_entry = match mk_entry(&va.root_path) {
            Ok(v) => v,
            Err(detail) => {
                self.notice = Some(
                    self.catalog
                        .t_args("export-err", &[("detail", detail.into())]),
                );
                return;
            }
        };
        let right_entry = match mk_entry(&vb.root_path) {
            Ok(v) => v,
            Err(detail) => {
                self.notice = Some(
                    self.catalog
                        .t_args("export-err", &[("detail", detail.into())]),
                );
                return;
            }
        };
        let row = PaneRow {
            name: rel.display().to_string(),
            rel_path: rel.clone(),
            left: left_entry,
            right: right_entry,
            status: Some(btrw_core::model::ChangeKind::Modified),
            delta_bytes: None,
            error: None,
        };

        let default_name = rel
            .file_name()
            .map(|n| format!("{}.patch", n.to_string_lossy()))
            .unwrap_or_else(|| "timetravel.patch".to_string());
        let cursor = default_name.chars().count();

        let mut current_dir = self.default_save_dir(rel);
        while !current_dir.is_dir() {
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                current_dir = PathBuf::from("/tmp");
                break;
            }
        }

        let mut state = ExportPatchState {
            scope: ExportScope::SelectedObject,
            selected_row: Some(row),
            custom_roots: Some((va.root_path.clone(), vb.root_path.clone())),
            current_dir,
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: default_name,
            cursor,
            focus: ExportFocus::Filename,
        };
        let protected = self.protected_snapshot_dirs();
        state.reload_subdirs(&protected);
        self.mode = Mode::ExportPatch(Box::new(state));
    }


    pub fn run_export_patch_state(&mut self, state: &ExportPatchState) {
        let Some(output_path) = state.full_dest_path() else {
            return;
        };
        let (lpath, rpath) = if let Some((l, r)) = &state.custom_roots {
            (l.clone(), r.clone())
        } else {
            let (Some(left), Some(right)) = (self.left_source(), self.right_source()) else {
                return;
            };
            (left.path().clone(), right.path().clone())
        };
        self.cancel_export_job();
        let show_meta = self.config.diff.show_metadata_only;
        let max_bytes = self.config.diff.max_file_bytes;
        let max_lines = self.config.diff.max_lines;
        let scope = state.scope;
        let selected_row = state.selected_row.clone();
        let rel_dir = self.rel_dir.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            // Always use tree walk (`scan_flat_changes`) for exported patches so completeness
            // is guaranteed even with hardlinks or renamed directories.
            let collect_dir = |scope_rel: &std::path::Path| -> DirCompareResult {
                scan_flat_changes(&lpath, &rpath, scope_rel, show_meta, &cancel_w, |_, _| {})
            };

            let (rows, scan_errors): (Vec<PaneRow>, Vec<String>) = match scope {
                ExportScope::SelectedObject => match &selected_row {
                    Some(row) if !row.is_dir() => (vec![row.clone()], Vec::new()),
                    Some(row) => {
                        let res = collect_dir(&row.rel_path);
                        (res.rows, res.errors)
                    }
                    None => {
                        let res = collect_dir(&rel_dir);
                        (res.rows, res.errors)
                    }
                },
                ExportScope::CurrentDir => {
                    let res = collect_dir(&rel_dir);
                    (res.rows, res.errors)
                }
                ExportScope::WholeSnapshot => {
                    let res = collect_dir(std::path::Path::new(""));
                    (res.rows, res.errors)
                }
            };

            if cancel_w.load(Ordering::Relaxed) {
                return;
            }
            if !scan_errors.is_empty() {
                let reason = format!(
                    "incomplete scan ({} read errors, e.g. {})",
                    scan_errors.len(),
                    scan_errors[0]
                );
                let _ = tx.send(Err(reason));
                return;
            }

            let res = export_patch_cancellable(
                &lpath,
                &rpath,
                &rows,
                &output_path,
                max_bytes,
                max_lines,
                &cancel_w,
            )
            .map(|summary| (summary, output_path));
            // Always deliver `Ok` if the file was already published on disk so the user is informed.
            if res.is_ok() || !cancel_w.load(Ordering::Relaxed) {
                let _ = tx.send(res);
            }
        });

        self.export_job = Some(ExportPatchJob { cancel, rx });
    }
}
