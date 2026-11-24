use crate::config::{ReflinkMode, RestoreConfig};
use crate::fs_safe::{StatSig, format_datetime_local, fstat_fd};
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// Linux ioctl FICLONE = _IOW(0x94, 9, int)
const FICLONE: libc::Ioctl = 0x4004_9409u32 as libc::Ioctl;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn cstring_from_os(s: &OsStr) -> Result<CString, String> {
    CString::new(s.as_bytes()).map_err(|_| "path contains NUL byte".to_string())
}

// Opens a directory with O_NOFOLLOW on every path component, intentionally omitting O_NOATIME
// so non-root users can traverse root-owned ancestors (e.g. `/tmp`) when checking write safety.
pub(super) fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    let root_c = CString::new("/").unwrap();
    let mut fd = unsafe { libc::openat(libc::AT_FDCWD, root_c.as_ptr(), path_flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut owned = unsafe { OwnedFd::from_raw_fd(fd) };
    for comp in abs.components() {
        match comp {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::Normal(part) => {
                let cpart = CString::new(part.as_bytes())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
                fd = unsafe { libc::openat(owned.as_raw_fd(), cpart.as_ptr(), path_flags) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                owned = unsafe { OwnedFd::from_raw_fd(fd) };
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported path component",
                ));
            }
        }
    }
    let dot = CString::new(".").unwrap();
    let final_fd = unsafe { libc::openat(owned.as_raw_fd(), dot.as_ptr(), dir_flags) };
    if final_fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(final_fd) })
    }
}

/// Validates that `dest_path` is strictly outside all `protected_dirs` (configured and

pub(super) fn copy_data_and_metadata(
    src_file: &mut File,
    dst_file: &mut File,
    src_sig: &StatSig,
    cfg: &RestoreConfig,
) -> Result<(bool, u64), String> {
    let mut used_reflink = false;
    let bytes_written = if cfg.use_reflink == ReflinkMode::Auto && src_sig.size > 0 {
        let rc = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
        if rc == 0 {
            used_reflink = true;
            src_sig.size
        } else {
            stream_copy_verified(src_file, dst_file, src_sig.size)?
        }
    } else {
        stream_copy_verified(src_file, dst_file, src_sig.size)?
    };

    let is_root = unsafe { libc::geteuid() } == 0;

    // Preserve ownership BEFORE permissions/xattrs (fchown may clear SUID/SGID bits and capabilities).
    if cfg.preserve_ownership {
        let rc = unsafe { libc::fchown(dst_file.as_raw_fd(), src_sig.uid, src_sig.gid) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            // Under root, ownership preservation failure is always an error; unprivileged tests may see EPERM.
            if is_root || err.raw_os_error() != Some(libc::EPERM) {
                return Err(format!("fchown failed: {err}"));
            }
        }
    }

    if cfg.preserve_xattrs {
        copy_xattrs_verified(src_file.as_raw_fd(), dst_file.as_raw_fd())?;
    }

    // Apply permissions AFTER xattrs/ACLs so a parent default ACL cannot alter the restored mode bits.
    if cfg.preserve_permissions {
        let mode = src_sig.mode & 0o7777;
        let rc = unsafe { libc::fchmod(dst_file.as_raw_fd(), mode) };
        if rc != 0 {
            return Err(format!("fchmod failed: {}", io::Error::last_os_error()));
        }
    }

    // Verify resulting metadata on the fd before publishing!
    let final_sig = fstat_fd(dst_file.as_raw_fd())
        .map_err(|e| format!("fstat verification failed before publish: {e}"))?;
    if cfg.preserve_ownership && is_root && (final_sig.uid != src_sig.uid || final_sig.gid != src_sig.gid) {
        return Err(format!(
            "ownership mismatch after restore (expected {}:{}, got {}:{})",
            src_sig.uid, src_sig.gid, final_sig.uid, final_sig.gid
        ));
    }
    if cfg.preserve_permissions && (final_sig.mode & 0o7777) != (src_sig.mode & 0o7777) {
        return Err(format!(
            "mode mismatch after restore (expected {:04o}, got {:04o})",
            src_sig.mode & 0o7777,
            final_sig.mode & 0o7777
        ));
    }

    Ok((used_reflink, bytes_written))
}

fn stream_copy_verified(src: &mut File, dst: &mut File, expected_size: u64) -> Result<u64, String> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|e| format!("read from source failed: {e}"))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])
            .map_err(|e| format!("write to temp file failed: {e}"))?;
        total += n as u64;
    }
    if total != expected_size {
        return Err(format!(
            "source size changed during restore (expected {expected_size}, copied {total})"
        ));
    }
    Ok(total)
}

pub(super) fn create_backup_in_dir(
    dir_fd: RawFd,
    dest_parent: &Path,
    dest_name: &OsStr,
    expected_sig: StatSig,
    reflink_mode: ReflinkMode,
) -> Result<PathBuf, String> {
    let orig_c = cstring_from_os(dest_name)?;
    let orig_fd = unsafe {
        libc::openat(
            dir_fd,
            orig_c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if orig_fd < 0 {
        return Err(format!(
            "cannot open existing file for backup: {}",
            io::Error::last_os_error()
        ));
    }
    let mut orig_file = File::from(unsafe { OwnedFd::from_raw_fd(orig_fd) });
    let orig_sig = fstat_fd(orig_file.as_raw_fd())
        .map_err(|e| format!("cannot stat existing file for backup: {e}"))?;
    if orig_sig != expected_sig {
        return Err(
            "destination file changed before backup could be created; please re-confirm"
                .to_string(),
        );
    }

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let stamp = format_datetime_local(now_secs)
        .replace(['-', ':'], "")
        .replace(' ', "T");

    for attempt in 0..100u32 {
        // Preserve raw destination filename bytes (no lossy UTF-8 conversion).
        let mut cand_bytes = dest_name.as_bytes().to_vec();
        cand_bytes.extend_from_slice(b".bak.");
        cand_bytes.extend_from_slice(stamp.as_bytes());
        if attempt > 0 {
            cand_bytes.extend_from_slice(format!("_{attempt}").as_bytes());
        }
        let candidate = OsString::from_vec(cand_bytes);
        let cand_c = cstring_from_os(&candidate)?;
        let bak_fd = unsafe {
            libc::openat(
                dir_fd,
                cand_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if bak_fd < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(format!("cannot create backup file: {err}"));
        }
        let mut bak_file = File::from(unsafe { OwnedFd::from_raw_fd(bak_fd) });
        let bak_cfg = RestoreConfig {
            create_backup: false,
            use_reflink: reflink_mode,
            preserve_ownership: true,
            preserve_permissions: true,
            preserve_xattrs: true,
        };
        if let Err(e) = copy_data_and_metadata(&mut orig_file, &mut bak_file, &orig_sig, &bak_cfg) {
            drop(bak_file);
            unsafe {
                libc::unlinkat(dir_fd, cand_c.as_ptr(), 0);
            }
            return Err(format!("backup write failed: {e}"));
        }
        if let Err(e) = bak_file.sync_all() {
            drop(bak_file);
            unsafe {
                libc::unlinkat(dir_fd, cand_c.as_ptr(), 0);
            }
            return Err(format!("backup fsync failed (aborting overwrite): {e}"));
        }
        drop(bak_file);
        // Ensure backup directory entry is durable before overwriting the target.
        if unsafe { libc::fsync(dir_fd) } != 0 {
            let err = io::Error::last_os_error();
            unsafe {
                libc::unlinkat(dir_fd, cand_c.as_ptr(), 0);
            }
            return Err(format!("backup directory fsync failed (aborting overwrite): {err}"));
        }
        return Ok(dest_parent.join(candidate));
    }
    Err("could not allocate unique backup filename".to_string())
}

pub(super) fn unique_temp_name(dest_name: &OsStr) -> String {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = dest_name.to_string_lossy();
    format!(".{base}.btrw.{}.{seq}.tmp", std::process::id())
}

fn copy_xattrs_verified(src_fd: RawFd, dst_fd: RawFd) -> Result<(), String> {
    let list_len = unsafe { libc::flistxattr(src_fd, std::ptr::null_mut(), 0) };
    if list_len < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(());
        }
        return Err(format!("flistxattr failed: {err}"));
    }
    if list_len == 0 {
        let acl_key = CString::new("system.posix_acl_access").unwrap();
        let _ = unsafe { libc::fremovexattr(dst_fd, acl_key.as_ptr()) };
        return Ok(());
    }
    let mut names = vec![0u8; list_len as usize];
    let n = unsafe {
        libc::flistxattr(
            src_fd,
            names.as_mut_ptr().cast::<libc::c_char>(),
            names.len(),
        )
    };
    if n < 0 {
        return Err(format!(
            "flistxattr read failed: {}",
            io::Error::last_os_error()
        ));
    }
    let mut src_had_acl = false;
    for raw_name in names[..n as usize].split(|&b| b == 0) {
        if raw_name.is_empty() {
            continue;
        }
        if raw_name == b"system.posix_acl_access" {
            src_had_acl = true;
        }
        let Ok(cname) = CString::new(raw_name) else {
            continue;
        };
        let val_len = unsafe { libc::fgetxattr(src_fd, cname.as_ptr(), std::ptr::null_mut(), 0) };
        if val_len < 0 {
            return Err(format!(
                "fgetxattr {} failed: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
        }
        let mut val = vec![0u8; val_len as usize];
        if val_len > 0 {
            let got = unsafe {
                libc::fgetxattr(
                    src_fd,
                    cname.as_ptr(),
                    val.as_mut_ptr().cast::<libc::c_void>(),
                    val.len(),
                )
            };
            if got < 0 {
                return Err(format!(
                    "fgetxattr {} read failed: {}",
                    String::from_utf8_lossy(raw_name),
                    io::Error::last_os_error()
                ));
            }
            val.truncate(got as usize);
        }
        let rc = unsafe {
            libc::fsetxattr(
                dst_fd,
                cname.as_ptr(),
                val.as_ptr().cast::<libc::c_void>(),
                val.len(),
                0,
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOTSUP) {
                continue;
            }
            return Err(format!(
                "fsetxattr {} failed: {err}",
                String::from_utf8_lossy(raw_name)
            ));
        }
    }
    if !src_had_acl {
        let acl_key = CString::new("system.posix_acl_access").unwrap();
        let _ = unsafe { libc::fremovexattr(dst_fd, acl_key.as_ptr()) };
    }
    Ok(())
}

