use super::{DirCompareResult, DirEntryInfo, PaneRow, metadata_differs};
use crate::fs_safe::{
    current_mountpoints, files_content_equal_cancellable, format_os_display, fstat_fd, lstat_at,
    open_dir_safe, open_rel_dir_safe, read_dir_names_fd, readlink_at,
};
use crate::model::{ChangeKind, EntryKind};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};


pub fn compare_directory(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    show_metadata_only: bool,
) -> DirCompareResult {
    let cancel = AtomicBool::new(false);
    compare_directory_cancellable(left_root, right_root, rel_dir, show_metadata_only, &cancel)
}

pub fn compare_directory_cancellable(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    show_metadata_only: bool,
    cancel: &AtomicBool,
) -> DirCompareResult {
    let mountpoints = current_mountpoints();
    let mut errors = Vec::new();
    let mut excluded_names = BTreeSet::new();

    if cancel.load(Ordering::Relaxed) {
        return DirCompareResult::default();
    }
    let left_map = read_single_dir(
        left_root,
        rel_dir,
        &mountpoints,
        &mut excluded_names,
        &mut errors,
    );
    if cancel.load(Ordering::Relaxed) {
        return DirCompareResult::default();
    }
    let right_map = read_single_dir(
        right_root,
        rel_dir,
        &mountpoints,
        &mut excluded_names,
        &mut errors,
    );

    let all_names: BTreeSet<OsString> = left_map
        .keys()
        .chain(right_map.keys())
        .cloned()
        .collect();

    let mut rows = Vec::with_capacity(all_names.len());
    for os_name in all_names {
        if cancel.load(Ordering::Relaxed) {
            return DirCompareResult::default();
        }
        if excluded_names.contains(&os_name) {
            continue;
        }
        let left = left_map.get(&os_name).cloned();
        let right = right_map.get(&os_name).cloned();
        let rel_path = if rel_dir.as_os_str().is_empty() {
            PathBuf::from(&os_name)
        } else {
            rel_dir.join(&os_name)
        };
        let display_name = format_os_display(&os_name);

        let mut error = None;
        let status = match (&left, &right) {
            (None, Some(_)) => Some(ChangeKind::Added),
            (Some(_), None) => Some(ChangeKind::Deleted),
            (Some(l), Some(r)) => {
                if l.sig.file_type() != r.sig.file_type() {
                    Some(ChangeKind::TypeChanged)
                } else if l.kind == EntryKind::Symlink {
                    if l.target_raw != r.target_raw {
                        Some(ChangeKind::Modified)
                    } else if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                        Some(ChangeKind::MetadataOnly)
                    } else {
                        None
                    }
                } else if l.kind == EntryKind::File {
                    match files_content_equal_cancellable(
                        left_root, right_root, &rel_path, &l.sig, &r.sig, cancel,
                    ) {
                        Ok(true) => {
                            if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                                Some(ChangeKind::MetadataOnly)
                            } else {
                                None
                            }
                        }
                        Ok(false) => Some(ChangeKind::Modified),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            return DirCompareResult::default();
                        }
                        Err(e) => {
                            error = Some(e.to_string());
                            errors.push(format!("{display_name}: {e}"));
                            Some(ChangeKind::Error)
                        }
                    }
                } else if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                    Some(ChangeKind::MetadataOnly)
                } else {
                    None
                }
            }
            (None, None) => None,
        };

        let delta_bytes = compute_delta(left.as_ref(), right.as_ref());

        rows.push(PaneRow {
            name: display_name,
            rel_path,
            left,
            right,
            status,
            delta_bytes,
            error,
        });
    }

    // MC ordering: directories first, then files, alphabetical case-insensitive.
    rows.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.rel_path.as_os_str().cmp(b.rel_path.as_os_str()))
    });

    DirCompareResult {
        rows,
        excluded: excluded_names
            .iter()
            .map(|n| format_os_display(n))
            .collect(),
        errors,
    }
}

fn compute_delta(left: Option<&DirEntryInfo>, right: Option<&DirEntryInfo>) -> Option<i128> {
    let is_reg_or_none = |e: Option<&DirEntryInfo>| {
        e.map(|i| i.kind == EntryKind::File).unwrap_or(true)
    };
    if !is_reg_or_none(left) || !is_reg_or_none(right) {
        return None;
    }
    let l = left.and_then(|e| e.size).unwrap_or(0) as i128;
    let r = right.and_then(|e| e.size).unwrap_or(0) as i128;
    Some(r - l)
}

fn read_single_dir(
    root: &Path,
    rel_dir: &Path,
    mountpoints: &HashSet<PathBuf>,
    excluded: &mut BTreeSet<OsString>,
    errors: &mut Vec<String>,
) -> BTreeMap<OsString, DirEntryInfo> {
    let root_dev = match open_dir_safe(root).and_then(|fd| fstat_fd(fd.as_raw_fd())) {
        Ok(sig) => sig.dev,
        Err(e) => {
            errors.push(format!("{}: {e}", root.display()));
            return BTreeMap::new();
        }
    };
    read_single_dir_with_dev(root, root_dev, rel_dir, mountpoints, excluded, errors)
}


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirStats {
    pub changed_count: usize,
    pub left_bytes: u64,
    pub right_bytes: u64,
    pub scanned_entries: usize,
    pub done: bool,
    pub errors: Vec<String>,
}

/// Recursively traverses `rel_dir` on both `left_root` and `right_root`, accumulating
/// changed file count and total regular-file bytes on both sides.
/// Calls `on_progress` at most ~4 times per second (every 250 ms) and once on completion.
pub fn scan_directory_stats<F>(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    cancel: &AtomicBool,
    on_progress: F,
) -> DirStats
where
    F: FnMut(&DirStats),
{
    let mountpoints = current_mountpoints();
    scan_directory_stats_with_mounts(left_root, right_root, rel_dir, &mountpoints, cancel, on_progress)
}

pub fn scan_directory_stats_with_mounts<F>(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    mountpoints: &HashSet<PathBuf>,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> DirStats
where
    F: FnMut(&DirStats),
{
    let mut stats = DirStats::default();
    let left_dev = match open_dir_safe(left_root).and_then(|fd| fstat_fd(fd.as_raw_fd())) {
        Ok(s) => Some(s.dev),
        Err(e) => {
            stats.errors.push(format!("{}: {e}", left_root.display()));
            None
        }
    };
    let right_dev = match open_dir_safe(right_root).and_then(|fd| fstat_fd(fd.as_raw_fd())) {
        Ok(s) => Some(s.dev),
        Err(e) => {
            stats.errors.push(format!("{}: {e}", right_root.display()));
            None
        }
    };

    let mut last_emit = Instant::now();
fn side_is_dir_or_root(root: &Path, rel_dir: &Path) -> bool {
    if rel_dir.as_os_str().is_empty() {
        return true;
    }
    open_rel_dir_safe(root, rel_dir).is_ok()
}

    // Each stack entry tracks `(rel_dir, descend_left, descend_right)` so a TypeChanged
    // directory<->file entry only descends on the side that is actually a directory.
    let mut stack = vec![(
        rel_dir.to_path_buf(),
        side_is_dir_or_root(left_root, rel_dir),
        side_is_dir_or_root(right_root, rel_dir),
    )];

    while let Some((cur_rel, descend_left, descend_right)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return stats;
        }
        let mut dir_excluded = BTreeSet::new();
        let left_map = if descend_left
            && let Some(dev) = left_dev
        {
            read_single_dir_with_dev(
                left_root,
                dev,
                &cur_rel,
                mountpoints,
                &mut dir_excluded,
                &mut stats.errors,
            )
        } else {
            BTreeMap::new()
        };
        let right_map = if descend_right
            && let Some(dev) = right_dev
        {
            read_single_dir_with_dev(
                right_root,
                dev,
                &cur_rel,
                mountpoints,
                &mut dir_excluded,
                &mut stats.errors,
            )
        } else {
            BTreeMap::new()
        };

        let all_names: BTreeSet<OsString> =
            left_map.keys().chain(right_map.keys()).cloned().collect();
        for os_name in all_names {
            if cancel.load(Ordering::Relaxed) {
                return stats;
            }
            // Skip entries excluded on EITHER side (e.g. live mountpoint / nested subvolume).
            if dir_excluded.contains(&os_name) {
                continue;
            }
            stats.scanned_entries += 1;
            let l = left_map.get(&os_name);
            let r = right_map.get(&os_name);
            let child_rel = if cur_rel.as_os_str().is_empty() {
                PathBuf::from(&os_name)
            } else {
                cur_rel.join(&os_name)
            };

            if let Some(le) = l
                && le.kind == EntryKind::File
            {
                stats.left_bytes = stats.left_bytes.saturating_add(le.size.unwrap_or(0));
            }
            if let Some(re) = r
                && re.kind == EntryKind::File
            {
                stats.right_bytes = stats.right_bytes.saturating_add(re.size.unwrap_or(0));
            }

            let l_is_dir = l.is_some_and(|e| e.kind == EntryKind::Directory);
            let r_is_dir = r.is_some_and(|e| e.kind == EntryKind::Directory);
            if l_is_dir || r_is_dir {
                if l_is_dir && r_is_dir {
                    stack.push((child_rel, true, true));
                } else {
                    stats.changed_count += 1;
                    stack.push((child_rel, l_is_dir, r_is_dir));
                }
            } else {
                let changed = match (l, r) {
                    (None, Some(_)) | (Some(_), None) => true,
                    (Some(le), Some(re)) => {
                        if le.sig.file_type() != re.sig.file_type() {
                            true
                        } else if le.kind == EntryKind::Symlink {
                            le.target_raw != re.target_raw
                        } else if le.kind == EntryKind::File {
                            match files_content_equal_cancellable(
                                left_root,
                                right_root,
                                &child_rel,
                                &le.sig,
                                &re.sig,
                                cancel,
                            ) {
                                Ok(eq) => !eq,
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                                    return stats;
                                }
                                Err(e) => {
                                    stats.errors.push(format!(
                                        "{}: {e}",
                                        format_os_display(child_rel.as_os_str())
                                    ));
                                    true
                                }
                            }
                        } else {
                            false
                        }
                    }
                    (None, None) => false,
                };
                if changed {
                    stats.changed_count += 1;
                }
            }

            if last_emit.elapsed() >= Duration::from_millis(250) {
                on_progress(&stats);
                last_emit = Instant::now();
            }
        }
    }

    stats.done = true;
    on_progress(&stats);
    stats
}

/// Recursively collects all changed entries under `rel_dir` (flat list).
/// Calls `on_progress(scanned_entries, current_rel)` every ~250 ms.
pub fn scan_flat_changes<F>(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    show_metadata_only: bool,
    cancel: &AtomicBool,
    on_progress: F,
) -> DirCompareResult
where
    F: FnMut(usize, &Path),
{
    let mountpoints = current_mountpoints();
    scan_flat_changes_with_mounts(
        left_root,
        right_root,
        rel_dir,
        show_metadata_only,
        &mountpoints,
        cancel,
        on_progress,
    )
}

pub fn scan_flat_changes_with_mounts<F>(
    left_root: &Path,
    right_root: &Path,
    rel_dir: &Path,
    show_metadata_only: bool,
    mountpoints: &HashSet<PathBuf>,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> DirCompareResult
where
    F: FnMut(usize, &Path),
{
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut excluded_labels = BTreeSet::new();

    let left_dev = match open_dir_safe(left_root).and_then(|fd| fstat_fd(fd.as_raw_fd())) {
        Ok(s) => Some(s.dev),
        Err(e) => {
            errors.push(format!("{}: {e}", left_root.display()));
            None
        }
    };
    let right_dev = match open_dir_safe(right_root).and_then(|fd| fstat_fd(fd.as_raw_fd())) {
        Ok(s) => Some(s.dev),
        Err(e) => {
            errors.push(format!("{}: {e}", right_root.display()));
            None
        }
    };

    let mut scanned = 0usize;
    let mut last_emit = Instant::now();
fn side_is_dir_or_root(root: &Path, rel_dir: &Path) -> bool {
    if rel_dir.as_os_str().is_empty() {
        return true;
    }
    open_rel_dir_safe(root, rel_dir).is_ok()
}

    // Each stack entry tracks `(rel_dir, descend_left, descend_right)` so a TypeChanged
    // directory<->file entry only descends on the side that is actually a directory.
    let mut stack = vec![(
        rel_dir.to_path_buf(),
        side_is_dir_or_root(left_root, rel_dir),
        side_is_dir_or_root(right_root, rel_dir),
    )];

    while let Some((cur_rel, descend_left, descend_right)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut dir_excluded = BTreeSet::new();
        let left_map = if descend_left
            && let Some(dev) = left_dev
        {
            read_single_dir_with_dev(
                left_root,
                dev,
                &cur_rel,
                mountpoints,
                &mut dir_excluded,
                &mut errors,
            )
        } else {
            BTreeMap::new()
        };
        let right_map = if descend_right
            && let Some(dev) = right_dev
        {
            read_single_dir_with_dev(
                right_root,
                dev,
                &cur_rel,
                mountpoints,
                &mut dir_excluded,
                &mut errors,
            )
        } else {
            BTreeMap::new()
        };

        for ex in &dir_excluded {
            let ex_rel = if cur_rel.as_os_str().is_empty() {
                PathBuf::from(ex)
            } else {
                cur_rel.join(ex)
            };
            excluded_labels.insert(format_os_display(ex_rel.as_os_str()));
        }

        let all_names: BTreeSet<OsString> =
            left_map.keys().chain(right_map.keys()).cloned().collect();
        for os_name in all_names {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            // Skip entries excluded on EITHER side (same rule as compare_directory).
            if dir_excluded.contains(&os_name) {
                continue;
            }
            scanned += 1;
            let left = left_map.get(&os_name).cloned();
            let right = right_map.get(&os_name).cloned();
            let child_rel = if cur_rel.as_os_str().is_empty() {
                PathBuf::from(&os_name)
            } else {
                cur_rel.join(&os_name)
            };

            if last_emit.elapsed() >= Duration::from_millis(250) {
                on_progress(scanned, &child_rel);
                last_emit = Instant::now();
            }

            let l_is_dir = left.as_ref().is_some_and(|e| e.kind == EntryKind::Directory);
            let r_is_dir = right.as_ref().is_some_and(|e| e.kind == EntryKind::Directory);
            if l_is_dir || r_is_dir {
                stack.push((child_rel.clone(), l_is_dir, r_is_dir));
                if l_is_dir && r_is_dir {
                    let meta_changed = show_metadata_only
                        && match (&left, &right) {
                            (Some(l), Some(r)) => metadata_differs(&l.sig, &r.sig),
                            _ => false,
                        };
                    if !meta_changed {
                        continue;
                    }
                }
            }

            let mut error = None;
            let status = match (&left, &right) {
                (None, Some(_)) => Some(ChangeKind::Added),
                (Some(_), None) => Some(ChangeKind::Deleted),
                (Some(l), Some(r)) => {
                    if l.sig.file_type() != r.sig.file_type() {
                        Some(ChangeKind::TypeChanged)
                    } else if l.kind == EntryKind::Symlink {
                        if l.target_raw != r.target_raw {
                            Some(ChangeKind::Modified)
                        } else if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                            Some(ChangeKind::MetadataOnly)
                        } else {
                            None
                        }
                    } else if l.kind == EntryKind::File {
                        match files_content_equal_cancellable(
                            left_root, right_root, &child_rel, &l.sig, &r.sig, cancel,
                        ) {
                            Ok(true) => {
                                if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                                    Some(ChangeKind::MetadataOnly)
                                } else {
                                    None
                                }
                            }
                            Ok(false) => Some(ChangeKind::Modified),
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                                break;
                            }
                            Err(e) => {
                                error = Some(e.to_string());
                                errors.push(format!(
                                    "{}: {e}",
                                    format_os_display(child_rel.as_os_str())
                                ));
                                Some(ChangeKind::Error)
                            }
                        }
                    } else if show_metadata_only && metadata_differs(&l.sig, &r.sig) {
                        Some(ChangeKind::MetadataOnly)
                    } else {
                        None
                    }
                }
                (None, None) => None,
            };

            if status.is_some() {
                let delta_bytes = compute_delta(left.as_ref(), right.as_ref());
                let display = format_os_display(child_rel.as_os_str());
                rows.push(PaneRow {
                    name: display,
                    rel_path: child_rel,
                    left,
                    right,
                    status,
                    delta_bytes,
                    error,
                });
            }
        }
    }

    DirCompareResult {
        rows,
        excluded: excluded_labels.into_iter().collect(),
        errors,
    }
}

fn read_single_dir_with_dev(
    root: &Path,
    root_dev: u64,
    rel_dir: &Path,
    mountpoints: &HashSet<PathBuf>,
    excluded: &mut BTreeSet<OsString>,
    errors: &mut Vec<String>,
) -> BTreeMap<OsString, DirEntryInfo> {
    let mut map = BTreeMap::new();
    let dir_fd = match open_rel_dir_safe(root, rel_dir) {
        Ok(fd) => fd,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("{}/{}: {e}", root.display(), rel_dir.display()));
            }
            return map;
        }
    };

    let dir_path = if rel_dir.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel_dir)
    };
    let names = match read_dir_names_fd(dir_fd.as_raw_fd()) {
        Ok(list) => list,
        Err(e) => {
            errors.push(format!("{}: {e}", dir_path.display()));
            return map;
        }
    };

    for os_name in names {
        let display_name = format_os_display(&os_name);
        let sig = match lstat_at(dir_fd.as_raw_fd(), &os_name) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{display_name}: {e}"));
                continue;
            }
        };

        let full_path = dir_path.join(&os_name);
        if mountpoints.contains(&full_path)
            || sig.dev != root_dev
            || (sig.is_dir() && sig.ino == 256)
        {
            excluded.insert(os_name);
            continue;
        }

        let kind = if sig.is_reg() {
            EntryKind::File
        } else if sig.is_dir() {
            EntryKind::Directory
        } else if sig.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        };

        let (target, target_raw) = if kind == EntryKind::Symlink {
            match readlink_at(dir_fd.as_raw_fd(), &os_name) {
                Ok(p) => {
                    let raw = p.into_os_string();
                    let disp = format_os_display(&raw);
                    (Some(disp), Some(raw))
                }
                Err(e) => {
                    errors.push(format!("{display_name} (readlink): {e}"));
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        let rel_path = if rel_dir.as_os_str().is_empty() {
            PathBuf::from(&os_name)
        } else {
            rel_dir.join(&os_name)
        };

        map.insert(
            os_name,
            DirEntryInfo {
                name: display_name,
                rel_path,
                kind,
                size: if kind == EntryKind::File {
                    Some(sig.size)
                } else {
                    None
                },
                target,
                target_raw,
                sig,
            },
        );
    }

    map
}

