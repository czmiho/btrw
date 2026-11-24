use super::{App, Mode, SaveAsFocus, SaveAsState, TimeTravelState, source_header_label};
use btrw_core::fs_safe::{StatSig, open_file_safe};
use btrw_core::model::{PanelSide, Source};
use btrw_core::restore::{RestorePlan, restore_file_checked_with_protected};
use std::path::PathBuf;

impl App {
    pub(super) fn default_save_dir(&self, rel_path: &std::path::Path) -> PathBuf {
        let base_live = self
            .active_group_target()
            .filter(|p| p.is_absolute() && p.is_dir())
            .cloned()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));
        match rel_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => base_live.join(p),
            _ => base_live,
        }
    }

    pub fn open_restore_live_from_timetravel(&mut self, tt: &TimeTravelState) {
        let Some(ver) = tt.history.versions.get(tt.active_idx) else {
            return;
        };
        if ver.is_live {
            self.notice = Some(self.catalog.t("tt-already-live"));
            return;
        }
        if !ver.verified_parent {
            self.notice = Some(self.catalog.t("restore-need-live"));
            return;
        }
        if tt.group_idx != self.active_group {
            self.notice = Some(self.catalog.t("restore-need-live"));
            return;
        }
        let Some(live_target) = tt.verified_live_target.clone() else {
            self.notice = Some(self.catalog.t("restore-need-live"));
            return;
        };
        let rel_path = tt.history.rel_path.clone();
        let Ok((_, source_sig)) = open_file_safe(&ver.root_path, &rel_path) else {
            self.notice = Some(self.catalog.t("restore-need-file"));
            return;
        };
        let dest_path = live_target.join(&rel_path);
        let dest_sig = match self.inspect_dest_for_plan(&dest_path) {
            Ok(s) => s,
            Err(detail) => {
                self.notice = Some(
                    self.catalog
                        .t_args("restore-err", &[("detail", detail.into())]),
                );
                return;
            }
        };
        let live_root_sig = btrw_core::fs_safe::open_dir_safe(&live_target)
            .ok()
            .and_then(|fd| {
                use std::os::fd::AsRawFd;
                let st = btrw_core::fs_safe::fstat_fd(fd.as_raw_fd()).ok()?;
                let fsid = btrw_core::fs_safe::btrfs_fsid(fd.as_raw_fd());
                Some((st.dev, fsid))
            });
        let source_label = format!("{} ({})", ver.label, ver.short_label());
        self.mode = Mode::RestoreConfirm(Box::new(RestorePlan {
            source_root: ver.root_path.clone(),
            rel_path,
            source_label,
            dest_path,
            source_sig,
            dest_sig,
            expected_live_root: Some(live_target),
            expected_live_root_sig: live_root_sig,
        }));
    }

    pub fn open_save_as_from_timetravel(&mut self, tt: &TimeTravelState) {
        let Some(ver) = tt.history.versions.get(tt.active_idx) else {
            return;
        };
        let rel_path = tt.history.rel_path.clone();
        let Ok((_, source_sig)) = open_file_safe(&ver.root_path, &rel_path) else {
            self.notice = Some(self.catalog.t("restore-need-file"));
            return;
        };
        let file_name = rel_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "restored.file".to_string());
        let cursor = file_name.chars().count();
        let mut current_dir = self.default_save_dir(&rel_path);
        while !current_dir.is_dir() {
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                current_dir = PathBuf::from("/");
                break;
            }
        }
        let source_label = format!("{} ({})", ver.label, ver.short_label());
        let original_os_name = rel_path.file_name().map(|n| n.to_os_string());
        let mut state = SaveAsState {
            source_root: ver.root_path.clone(),
            source_label,
            rel_path,
            source_sig,
            current_dir,
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: file_name,
            original_os_name,
            filename_edited: false,
            cursor,
            focus: SaveAsFocus::Filename,
        };
        let protected = self.protected_snapshot_dirs();
        state.reload_subdirs(&protected);
        self.mode = Mode::SaveAs(Box::new(state));
    }


    fn resolve_restore_source(&self) -> Option<(PathBuf, String, PathBuf, StatSig, bool)> {
        let row = self.selected_row()?;
        if row.is_dir() {
            return None;
        }
        let (left, right) = (self.left_source()?, self.right_source()?);
        let candidates: Vec<&Source> = match self.active_side {
            PanelSide::Left => vec![left, right],
            PanelSide::Right => vec![right, left],
        };
        // Prefer an explicit RO snapshot first; fall back to CustomDir in non-Btrfs mode.
        for src in &candidates {
            if let Source::Snapshot(info) = src
                && let Ok((_, sig)) = open_file_safe(src.path(), &row.rel_path)
            {
                return Some((
                    src.path().clone(),
                    source_header_label(src),
                    row.rel_path.clone(),
                    sig,
                    info.verified_parent,
                ));
            }
        }
        for src in &candidates {
            if matches!(src, Source::CustomDir { .. })
                && let Ok((_, sig)) = open_file_safe(src.path(), &row.rel_path)
            {
                return Some((
                    src.path().clone(),
                    source_header_label(src),
                    row.rel_path.clone(),
                    sig,
                    false,
                ));
            }
        }
        None
    }

    fn inspect_dest_for_plan(&self, dest_path: &std::path::Path) -> Result<Option<StatSig>, String> {
        let parent = dest_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let Some(name) = dest_path.file_name() else {
            return Err(self.catalog.t("restore-err-no-filename"));
        };
        if !parent.exists() {
            return Ok(None);
        }
        match open_file_safe(parent, std::path::Path::new(name)) {
            Ok((_, sig)) => Ok(Some(sig)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                let path_str = dest_path.display().to_string();
                if let Ok(meta) = std::fs::symlink_metadata(dest_path) {
                    let ft = meta.file_type();
                    if ft.is_symlink() {
                        return Err(self
                            .catalog
                            .t_args("restore-err-dest-symlink", &[("path", path_str.into())]));
                    }
                    if ft.is_dir() {
                        return Err(self
                            .catalog
                            .t_args("restore-err-dest-dir", &[("path", path_str.into())]));
                    }
                    if !ft.is_file() {
                        return Err(self
                            .catalog
                            .t_args("restore-err-dest-not-regular", &[("path", path_str.into())]));
                    }
                }
                let reason = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    self.catalog.t("restore-err-perm-denied")
                } else {
                    e.to_string()
                };
                Err(self.catalog.t_args(
                    "restore-err-dest-inspect",
                    &[("path", path_str.into()), ("reason", reason.into())],
                ))
            }
        }
    }

    pub(super) fn localize_restore_error(&self, detail: &str) -> String {
        if detail == "refusing to write inside snapshots directory" {
            self.catalog.t("restore-err-inside-snapshots")
        } else if let Some(path) =
            detail.strip_prefix("refusing to write into read-only Btrfs subvolume: ")
        {
            self.catalog
                .t_args("restore-err-ro-subvol", &[("path", path.into())])
        } else if let Some(path) =
            detail.strip_prefix("destination filesystem is mounted read-only: ")
        {
            self.catalog
                .t_args("restore-err-ro-fs", &[("path", path.into())])
        } else if detail == "refusing to overwrite a symlink at destination" {
            self.catalog.t("restore-err-dest-symlink-generic")
        } else if detail == "refusing to overwrite a directory at destination" {
            self.catalog.t("restore-err-dest-dir-generic")
        } else if detail == "refusing to overwrite a special file at destination" {
            self.catalog.t("restore-err-dest-special-generic")
        } else if detail.starts_with("destination file changed since confirmation")
            || detail.starts_with("destination directory disappeared since confirmation")
        {
            self.catalog.t("restore-err-dest-changed")
        } else if detail.starts_with("source file changed since confirmation") {
            self.catalog.t("restore-err-src-changed")
        } else if detail.starts_with("refusing F5 live restore across mountpoint") {
            self.catalog.t("restore-err-mount-boundary")
        } else {
            detail.to_string()
        }
    }

    pub fn open_restore_live(&mut self) {
        let Some(live_target) = self.verified_live_target().cloned() else {
            self.notice = Some(self.catalog.t("restore-need-live"));
            return;
        };
        let Some((source_root, source_label, rel_path, source_sig, verified_parent)) =
            self.resolve_restore_source()
        else {
            self.notice = Some(self.catalog.t("restore-need-file"));
            return;
        };
        if !verified_parent {
            self.notice = Some(self.catalog.t("restore-need-live"));
            return;
        }
        let dest_path = live_target.join(&rel_path);
        let dest_sig = match self.inspect_dest_for_plan(&dest_path) {
            Ok(s) => s,
            Err(detail) => {
                self.notice = Some(
                    self.catalog
                        .t_args("restore-err", &[("detail", detail.into())]),
                );
                return;
            }
        };
        let live_root_sig = btrw_core::fs_safe::open_dir_safe(&live_target)
            .ok()
            .and_then(|fd| {
                use std::os::fd::AsRawFd;
                let st = btrw_core::fs_safe::fstat_fd(fd.as_raw_fd()).ok()?;
                let fsid = btrw_core::fs_safe::btrfs_fsid(fd.as_raw_fd());
                Some((st.dev, fsid))
            });
        self.mode = Mode::RestoreConfirm(Box::new(RestorePlan {
            source_root,
            rel_path,
            source_label,
            dest_path,
            source_sig,
            dest_sig,
            expected_live_root: Some(live_target),
            expected_live_root_sig: live_root_sig,
        }));
    }

    pub fn open_restore_save_as(&mut self) {
        let Some((source_root, source_label, rel_path, source_sig, _)) =
            self.resolve_restore_source()
        else {
            self.notice = Some(self.catalog.t("restore-need-file"));
            return;
        };
        let file_name = rel_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "restored.file".to_string());
        let cursor = file_name.chars().count();

        // Default directory: original live directory where the file resides (or nearest existing ancestor).
        let base_live = self
            .active_group_target()
            .cloned()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));
        let mut current_dir = match rel_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => base_live.join(p),
            _ => base_live,
        };
        while !current_dir.is_dir() {
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                current_dir = PathBuf::from("/");
                break;
            }
        }

        let original_os_name = rel_path.file_name().map(|n| n.to_os_string());
        let mut state = SaveAsState {
            source_root,
            source_label,
            rel_path,
            source_sig,
            current_dir,
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: file_name,
            original_os_name,
            filename_edited: false,
            cursor,
            focus: SaveAsFocus::Filename,
        };
        let protected = self.protected_snapshot_dirs();
        state.reload_subdirs(&protected);
        self.mode = Mode::SaveAs(Box::new(state));
    }

    pub fn open_restore_confirm_for_path(
        &mut self,
        source_root: PathBuf,
        source_label: String,
        rel_path: PathBuf,
        source_sig: StatSig,
        dest_path: PathBuf,
    ) {
        let dest_sig = match self.inspect_dest_for_plan(&dest_path) {
            Ok(s) => s,
            Err(detail) => {
                self.mode = Mode::Browse;
                self.notice = Some(
                    self.catalog
                        .t_args("restore-err", &[("detail", detail.into())]),
                );
                return;
            }
        };
        self.mode = Mode::RestoreConfirm(Box::new(RestorePlan {
            source_root,
            rel_path,
            source_label,
            dest_path,
            source_sig,
            dest_sig,
            expected_live_root: None,
            expected_live_root_sig: None,
        }));
    }

    pub fn execute_restore(&mut self, plan: &RestorePlan) {
        let protected = self.protected_snapshot_dirs();
        match restore_file_checked_with_protected(plan, &protected, &self.config.restore) {
            Ok(outcome) => {
                let mut details = Vec::new();
                if outcome.used_reflink {
                    details.push("FICLONE reflink".to_string());
                } else {
                    details.push(btrw_core::compare::human_size(outcome.bytes_written));
                }
                if let Some(bak) = outcome.backup_path {
                    details.push(format!("bak: {}", bak.display()));
                }
                self.mode = Mode::Browse;
                self.reload_dir();
                self.notice = Some(self.catalog.t_args(
                    "restore-ok",
                    &[
                        ("dest", outcome.dest_path.display().to_string().into()),
                        ("info", details.join(", ").into()),
                    ],
                ));
            }
            Err(detail) => {
                self.mode = Mode::Browse;
                let localized = self.localize_restore_error(&detail);
                self.notice = Some(
                    self.catalog
                        .t_args("restore-err", &[("detail", localized.into())]),
                );
            }
        }
    }

}
