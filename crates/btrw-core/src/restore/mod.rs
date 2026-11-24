mod ops;
#[cfg(test)]
mod tests;

use crate::config::RestoreConfig;
use crate::fs_safe::{
    StatSig, btrfs_fsid, fstat_fd, is_btrfs_ro_subvolume, lstat_at, open_file_safe,
};
use ops::{
    copy_data_and_metadata, create_backup_in_dir, cstring_from_os, open_dir_nofollow,
    unique_temp_name,
};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub source_root: PathBuf,
    pub rel_path: PathBuf,
    pub source_label: String,
    pub dest_path: PathBuf,
    pub source_sig: StatSig,
    pub dest_sig: Option<StatSig>,
    /// When set (automatic F5 live restore), the destination directory fd must stay on the
    /// exact same `(st_dev, Btrfs FSID)` as this verified live subvolume root (no mount crossings).
    pub expected_live_root: Option<PathBuf>,
    /// Captured `(st_dev, Btrfs FSID)` of `expected_live_root` at confirmation time.
    pub expected_live_root_sig: Option<(u64, Option<[u8; 16]>)>,
}

#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub dest_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub used_reflink: bool,
    pub bytes_written: u64,
}


/// auto-discovered snapshot roots) and not on a read-only filesystem or inside a read-only
/// Btrfs subvolume.
fn verify_dest_writable(dest_path: &Path, protected_dirs: &[PathBuf]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot resolve cwd: {e}"))?;
    let abs_dest = if dest_path.is_absolute() {
        dest_path.to_path_buf()
    } else {
        cwd.join(dest_path)
    };
    let canon_dest_ancestor = {
        let mut cur = Some(abs_dest.as_path());
        let mut found = None;
        while let Some(p) = cur {
            if p.exists() {
                if let Ok(c) = std::fs::canonicalize(p) {
                    found = Some(c);
                }
                break;
            }
            cur = p.parent();
        }
        found
    };
    for snapshots_dir in protected_dirs {
        if snapshots_dir.as_os_str().is_empty() {
            continue;
        }
        let abs_snaps = if snapshots_dir.is_absolute() {
            snapshots_dir.to_path_buf()
        } else {
            cwd.join(snapshots_dir)
        };
        if abs_dest == abs_snaps || abs_dest.starts_with(&abs_snaps) {
            return Err("refusing to write inside snapshots directory".to_string());
        }
        if let (Ok(canon_snaps), Some(canon_anc)) =
            (std::fs::canonicalize(&abs_snaps), &canon_dest_ancestor)
            && (canon_anc == &canon_snaps || canon_anc.starts_with(&canon_snaps))
        {
            return Err("refusing to write inside snapshots directory".to_string());
        }
    }

    // Check every existing ancestor up to `/` for RO Btrfs subvolume or ST_RDONLY mount.
    let mut cur = abs_dest.parent();
    while let Some(dir) = cur {
        if dir.exists() {
            let fd = open_dir_nofollow(dir)
                .map_err(|e| format!("cannot open destination ancestor {}: {e}", dir.display()))?;
            if is_btrfs_ro_subvolume(fd.as_raw_fd()) {
                return Err(format!(
                    "refusing to write into read-only Btrfs subvolume: {}",
                    dir.display()
                ));
            }
            let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
            if unsafe { libc::fstatvfs(fd.as_raw_fd(), vfs.as_mut_ptr()) } == 0 {
                let vfs = unsafe { vfs.assume_init() };
                if (vfs.f_flag & libc::ST_RDONLY) != 0 {
                    return Err(format!(
                        "destination filesystem is mounted read-only: {}",
                        dir.display()
                    ));
                }
            }
        }
        cur = dir.parent();
    }
    Ok(())
}

/// Executes a verified `RestorePlan`, rejecting the write if either the source file or the
/// destination file changed (or appeared/disappeared) since the confirmation dialog was opened,
/// or if `dest_path` falls inside any directory in `protected_dirs`.
pub fn restore_file_checked_with_protected(
    plan: &RestorePlan,
    protected_dirs: &[PathBuf],
    cfg: &RestoreConfig,
) -> Result<RestoreOutcome, String> {
    restore_file_inner(
        &plan.source_root,
        &plan.rel_path,
        &plan.dest_path,
        protected_dirs,
        cfg,
        Some(plan.source_sig),
        Some(plan.dest_sig),
        plan.expected_live_root.as_deref(),
        plan.expected_live_root_sig,
    )
}

/// Executes a verified `RestorePlan`, rejecting the write if either the source file or the
/// destination file changed (or appeared/disappeared) since the confirmation dialog was opened.
pub fn restore_file_checked(
    plan: &RestorePlan,
    snapshots_dir: &Path,
    cfg: &RestoreConfig,
) -> Result<RestoreOutcome, String> {
    let protected: Vec<PathBuf> = if snapshots_dir.as_os_str().is_empty() {
        Vec::new()
    } else {
        vec![snapshots_dir.to_path_buf()]
    };
    restore_file_checked_with_protected(plan, &protected, cfg)
}

/// Safely restores a single regular file from `source_root/rel_path` to `dest_path`.
///
/// Safety guarantees:
/// - Source opened with `O_NOFOLLOW | O_NOATIME | O_NONBLOCK`, verified regular file.
/// - Refuses to write inside `snapshots_dir`, read-only mounts, or RO Btrfs subvolumes.
/// - Refuses to overwrite a symlink or directory at `dest_path`.
/// - Optional `.bak.<timestamp>` backup of existing destination file (must fsync before overwrite).
/// - Atomic replacement via `O_CREAT | O_EXCL | O_NOFOLLOW` temp file in the same directory + `fsync` + `linkat`/`renameat`.
/// - Uses `FICLONE` reflink when enabled and supported, with verified streaming copy fallback.
pub fn restore_file(
    source_root: &Path,
    rel_path: &Path,
    dest_path: &Path,
    snapshots_dir: &Path,
    cfg: &RestoreConfig,
) -> Result<RestoreOutcome, String> {
    let protected: Vec<PathBuf> = if snapshots_dir.as_os_str().is_empty() {
        Vec::new()
    } else {
        vec![snapshots_dir.to_path_buf()]
    };
    restore_file_inner(
        source_root,
        rel_path,
        dest_path,
        &protected,
        cfg,
        None,
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_file_inner(
    source_root: &Path,
    rel_path: &Path,
    dest_path: &Path,
    protected_dirs: &[PathBuf],
    cfg: &RestoreConfig,
    expected_src: Option<StatSig>,
    expected_dest: Option<Option<StatSig>>,
    expected_live_root: Option<&Path>,
    expected_live_root_sig: Option<(u64, Option<[u8; 16]>)>,
) -> Result<RestoreOutcome, String> {
    verify_dest_writable(dest_path, protected_dirs)?;

    let (mut src_file, src_sig) = open_file_safe(source_root, rel_path)
        .map_err(|e| format!("cannot open source file: {e}"))?;
    if let Some(exp) = expected_src
        && src_sig != exp
    {
        return Err("source file changed since confirmation dialog opened; please retry".to_string());
    }

    let dest_parent = dest_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dest_name = dest_path
        .file_name()
        .ok_or_else(|| "destination path has no file name".to_string())?;

    // If a live root boundary is specified (F5), walk EVERY component of `rel_path`'s parent
    // relative to `expected_live_root` and verify `(st_dev, Btrfs FSID)` + mountpoints (fail-closed)
    // BEFORE creating any missing subdirectory.
    let dir_fd = if let Some(live_root) = expected_live_root {
        let mountpoints = crate::fs_safe::current_mountpoints_checked()
            .map_err(|e| format!("cannot read /proc/self/mountinfo (refusing F5): {e}"))?;
        let lfd = open_dir_nofollow(live_root)
            .map_err(|e| format!("cannot open verified live root: {e}"))?;
        let lst = fstat_fd(lfd.as_raw_fd())
            .map_err(|e| format!("cannot stat verified live root: {e}"))?;
        let lfsid = btrfs_fsid(lfd.as_raw_fd());
        if let Some((exp_dev, exp_fsid)) = expected_live_root_sig
            && (lst.dev != exp_dev || lfsid != exp_fsid)
        {
            return Err(
                "verified live root identity changed since confirmation; refusing F5".to_string(),
            );
        }
        let parent_rel = rel_path.parent().unwrap_or_else(|| Path::new(""));
        let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut cur = lfd;
        let mut cur_path = live_root.to_path_buf();
        let owner = if cfg.preserve_ownership && unsafe { libc::geteuid() } == 0 {
            Some((src_sig.uid, src_sig.gid))
        } else {
            None
        };
        for comp in parent_rel.components() {
            match comp {
                std::path::Component::CurDir => continue,
                std::path::Component::Normal(part) => {
                    cur_path.push(part);
                    if mountpoints.contains(&cur_path) {
                        return Err(
                            "refusing F5 live restore across mountpoint boundary".to_string(),
                        );
                    }
                    let cpart = cstring_from_os(part)?;
                    let next = unsafe { libc::openat(cur.as_raw_fd(), cpart.as_ptr(), dir_flags) };
                    let next_fd = if next >= 0 {
                        unsafe { OwnedFd::from_raw_fd(next) }
                    } else {
                        let err = io::Error::last_os_error();
                        if err.kind() != io::ErrorKind::NotFound {
                            return Err(format!("cannot open destination dir: {err}"));
                        }
                        if let Some(Some(_)) = expected_dest {
                            return Err(
                                "destination directory disappeared since confirmation dialog opened; please re-confirm"
                                    .to_string(),
                            );
                        }
                        let mk = unsafe { libc::mkdirat(cur.as_raw_fd(), cpart.as_ptr(), 0o755) };
                        let created = if mk == 0 {
                            true
                        } else {
                            let merr = io::Error::last_os_error();
                            if merr.kind() != io::ErrorKind::AlreadyExists {
                                return Err(format!("cannot create destination dir: {merr}"));
                            }
                            false
                        };
                        let n2 =
                            unsafe { libc::openat(cur.as_raw_fd(), cpart.as_ptr(), dir_flags) };
                        if n2 < 0 {
                            return Err(format!(
                                "cannot open created destination dir: {}",
                                io::Error::last_os_error()
                            ));
                        }
                        let fd2 = unsafe { OwnedFd::from_raw_fd(n2) };
                        if created && let Some((uid, gid)) = owner {
                            let rc = unsafe { libc::fchown(fd2.as_raw_fd(), uid, gid) };
                            if rc != 0 {
                                return Err(format!(
                                    "fchown destination dir failed: {}",
                                    io::Error::last_os_error()
                                ));
                            }
                        }
                        fd2
                    };
                    let st = fstat_fd(next_fd.as_raw_fd())
                        .map_err(|e| format!("cannot stat destination dir: {e}"))?;
                    if st.dev != lst.dev || btrfs_fsid(next_fd.as_raw_fd()) != lfsid {
                        return Err(
                            "refusing F5 live restore across mountpoint or subvolume boundary"
                                .to_string(),
                        );
                    }
                    cur = next_fd;
                }
                _ => return Err("invalid relative path component for F5 restore".to_string()),
            }
        }
        cur
    } else {
        match open_dir_nofollow(dest_parent) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(Some(_)) = expected_dest {
                    return Err(
                        "destination directory disappeared since confirmation dialog opened; please re-confirm"
                            .to_string(),
                    );
                }
                let owner = if cfg.preserve_ownership && unsafe { libc::geteuid() } == 0 {
                    Some((src_sig.uid, src_sig.gid))
                } else {
                    None
                };
                crate::fs_safe::ensure_dir_chain_owned(dest_parent, 0o755, owner)
                    .map_err(|err| format!("cannot create destination directory: {err}"))?
            }
            Err(e) => return Err(format!("cannot open destination directory: {e}")),
        }
    };

    // Check if destination already exists; never overwrite symlinks or directories.
    let existing_dest = match lstat_at(dir_fd.as_raw_fd(), dest_name) {
        Ok(sig) => {
            if sig.is_symlink() {
                return Err("refusing to overwrite a symlink at destination".to_string());
            }
            if sig.is_dir() {
                return Err("refusing to overwrite a directory at destination".to_string());
            }
            if !sig.is_reg() {
                return Err("refusing to overwrite a special file at destination".to_string());
            }
            Some(sig)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot stat destination: {e}")),
    };

    if let Some(exp_dest) = expected_dest
        && existing_dest != exp_dest
    {
        return Err(
            "destination file changed since confirmation dialog opened; please re-confirm"
                .to_string(),
        );
    }

    // Optional backup before overwrite: open once, verify fd matches `existing_dest`, back up from THAT fd.
    let backup_path = if let Some(exp_sig) = existing_dest
        && cfg.create_backup
    {
        Some(create_backup_in_dir(
            dir_fd.as_raw_fd(),
            dest_parent,
            dest_name,
            exp_sig,
            cfg.use_reflink,
        )?)
    } else {
        None
    };

    // Create exclusive temp file in the same destination directory for atomic publish.
    let tmp_name = unique_temp_name(dest_name);
    let tmp_c = cstring_from_os(OsStr::new(&tmp_name))?;
    let tmp_fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            tmp_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if tmp_fd < 0 {
        return Err(format!(
            "cannot create temp file in destination dir: {}",
            io::Error::last_os_error()
        ));
    }
    let tmp_owned = unsafe { OwnedFd::from_raw_fd(tmp_fd) };
    let mut tmp_file = File::from(tmp_owned);

    let write_res = copy_data_and_metadata(&mut src_file, &mut tmp_file, &src_sig, cfg);

    let (used_reflink, bytes_written) = match write_res {
        Ok(v) => v,
        Err(e) => {
            drop(tmp_file);
            unsafe {
                libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
            }
            return Err(e);
        }
    };

    if let Err(e) = tmp_file.sync_all() {
        drop(tmp_file);
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
        }
        return Err(format!("fsync failed: {e}"));
    }
    drop(tmp_file);

    let dest_c = cstring_from_os(dest_name)?;
    if existing_dest.is_none() {
        // New file: atomic publish with `linkat` so a concurrently created file is never overwritten.
        let rc = unsafe {
            libc::linkat(
                dir_fd.as_raw_fd(),
                tmp_c.as_ptr(),
                dir_fd.as_raw_fd(),
                dest_c.as_ptr(),
                0,
            )
        };
        let err = if rc != 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
        }
        if let Some(e) = err {
            return Err(format!(
                "atomic create failed (destination appeared concurrently?): {e}"
            ));
        }
    } else {
        // Re-verify destination right before renameat so a concurrent modification after
        // backup cannot be silently overwritten.
        if let Some(exp_sig) = existing_dest {
            match lstat_at(dir_fd.as_raw_fd(), dest_name) {
                Ok(now_sig) if now_sig == exp_sig => {}
                _ => {
                    unsafe {
                        libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
                    }
                    return Err(
                        "destination file changed before atomic publish; please re-confirm"
                            .to_string(),
                    );
                }
            }
        }
        // Overwrite existing verified regular file atomically via `renameat`.
        let rc = unsafe {
            libc::renameat(
                dir_fd.as_raw_fd(),
                tmp_c.as_ptr(),
                dir_fd.as_raw_fd(),
                dest_c.as_ptr(),
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            unsafe {
                libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
            }
            return Err(format!("atomic rename failed: {err}"));
        }
    }

    // Best-effort fsync on parent directory for durability.
    unsafe {
        libc::fsync(dir_fd.as_raw_fd());
    }

    Ok(RestoreOutcome {
        dest_path: dest_path.to_path_buf(),
        backup_path,
        used_reflink,
        bytes_written,
    })
}

