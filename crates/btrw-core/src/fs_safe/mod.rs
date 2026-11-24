mod btrfs;
mod fiemap;
mod mountinfo;

pub use btrfs::{
    BtrfsExtentSig, BtrfsSubvolMeta, btrfs_extent_data_map, btrfs_fsid, btrfs_subvol_meta,
    btrfs_subvol_otime, is_btrfs_fd_ro, is_btrfs_ro_subvolume, is_writable_btrfs_subvolume_root,
};
pub use fiemap::{files_content_equal, files_content_equal_cancellable, read_file_limited};
pub use mountinfo::{
    current_mountpoints, current_mountpoints_checked, decode_mount_escapes,
    decode_mount_escapes_bytes,
};

use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatSig {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
}

impl StatSig {
    pub fn from_stat(st: &libc::stat) -> Self {
        Self {
            dev: st.st_dev,
            ino: st.st_ino,
            nlink: st.st_nlink as u64,
            mode: st.st_mode,
            uid: st.st_uid,
            gid: st.st_gid,
            size: st.st_size.max(0) as u64,
            mtime_sec: st.st_mtime,
            mtime_nsec: st.st_mtime_nsec,
            ctime_sec: st.st_ctime,
            ctime_nsec: st.st_ctime_nsec,
        }
    }

    pub fn file_type(&self) -> u32 {
        self.mode & libc::S_IFMT
    }

    pub fn is_reg(&self) -> bool {
        self.file_type() == libc::S_IFREG
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == libc::S_IFDIR
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == libc::S_IFLNK
    }
}

fn cstring_from_os(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))
}

static STRICT_NOATIME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Enables or disables strict `O_NOATIME` mode. When enabled, unprivileged opens do not
/// fall back to opening without `O_NOATIME` on `EPERM`.
pub fn set_strict_noatime(strict: bool) {
    STRICT_NOATIME.store(strict, std::sync::atomic::Ordering::Relaxed);
}

fn openat_raw(dirfd: RawFd, name: &CString, flags: libc::c_int) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), flags) };
    if fd >= 0 {
        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    let err = io::Error::last_os_error();
    // When running unprivileged on files owned by another user (e.g. non-sudo --left/--right),
    // O_NOATIME fails with EPERM; retry without O_NOATIME unless strict mode is enabled.
    if (flags & libc::O_NOATIME) != 0
        && err.raw_os_error() == Some(libc::EPERM)
        && unsafe { libc::geteuid() } != 0
        && !STRICT_NOATIME.load(std::sync::atomic::Ordering::Relaxed)
    {
        let fd2 = unsafe { libc::openat(dirfd, name.as_ptr(), flags & !libc::O_NOATIME) };
        if fd2 >= 0 {
            return Ok(unsafe { OwnedFd::from_raw_fd(fd2) });
        }
        return Err(io::Error::last_os_error());
    }
    Err(err)
}

pub fn fstat_fd(fd: RawFd) -> io::Result<StatSig> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let rc = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        let st = unsafe { st.assume_init() };
        Ok(StatSig::from_stat(&st))
    }
}

pub fn lstat_at(dirfd: RawFd, name: &OsStr) -> io::Result<StatSig> {
    let cname = cstring_from_os(name)?;
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let rc = unsafe { libc::fstatat(dirfd, cname.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        let st = unsafe { st.assume_init() };
        Ok(StatSig::from_stat(&st))
    }
}

pub fn readlink_at(dirfd: RawFd, name: &OsStr) -> io::Result<PathBuf> {
    let cname = cstring_from_os(name)?;
    let mut buf = vec![0u8; 4096];
    let n = unsafe {
        libc::readlinkat(
            dirfd,
            cname.as_ptr(),
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(PathBuf::from(OsStr::from_bytes(&buf)))
}

/// Opens a directory safely without following symlinks and without updating atime.
/// Ancestors are traversed via `O_PATH | O_DIRECTORY | O_NOFOLLOW`.
pub fn open_dir_safe(path: &Path) -> io::Result<OwnedFd> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NOATIME | libc::O_CLOEXEC;

    let root_c = CString::new("/").unwrap();
    let mut fd = openat_raw(libc::AT_FDCWD, &root_c, path_flags)?;
    for comp in abs.components() {
        match comp {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(part) => {
                let cpart = cstring_from_os(part)?;
                fd = openat_raw(fd.as_raw_fd(), &cpart, path_flags)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported path component",
                ));
            }
        }
    }
    let dot = CString::new(".").unwrap();
    openat_raw(fd.as_raw_fd(), &dot, dir_flags)
}

/// Opens a relative subdirectory under `root` safely (no symlinks, no `..`, O_NOATIME).
pub fn open_rel_dir_safe(root: &Path, rel: &Path) -> io::Result<OwnedFd> {
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NOATIME | libc::O_CLOEXEC;
    let mut fd = open_dir_safe(root)?;
    for comp in rel.components() {
        match comp {
            Component::CurDir => continue,
            Component::Normal(part) => {
                let cpart = cstring_from_os(part)?;
                fd = openat_raw(fd.as_raw_fd(), &cpart, dir_flags)?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid relative directory component",
                ));
            }
        }
    }
    Ok(fd)
}

/// Lists directory entry names directly from an already-opened directory fd via
/// `dup` + `fdopendir` + `readdir`. Eliminates path-based TOCTOU and preserves `O_NOATIME`.
pub fn read_dir_names_fd(dir_fd: RawFd) -> io::Result<Vec<std::ffi::OsString>> {
    struct DirGuard(*mut libc::DIR);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
    }

    let dup_fd = unsafe { libc::fcntl(dir_fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let dirp = unsafe { libc::fdopendir(dup_fd) };
    if dirp.is_null() {
        let err = io::Error::last_os_error();
        unsafe {
            libc::close(dup_fd);
        }
        return Err(err);
    }
    let _guard = DirGuard(dirp);
    unsafe {
        libc::rewinddir(dirp);
    }

    let mut names = Vec::new();
    loop {
        unsafe {
            *libc::__errno_location() = 0;
        }
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            let errno = unsafe { *libc::__errno_location() };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        let cstr = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = cstr.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(bytes).to_os_string());
    }
    Ok(names)
}

/// Renders an `OsStr` losslessly for TUI display: valid UTF-8 is returned unchanged,
/// while invalid UTF-8 strings hex-escape invalid bytes (`\xNN`) AND escape literal `\` as `\\`
/// so literal `\xFF` and raw byte `0xFF` stay distinguishable.
pub fn format_os_display(s: &OsStr) -> String {
    if let Some(valid) = s.to_str() {
        return valid.to_string();
    }
    let mut out = String::new();
    let mut bytes = s.as_bytes();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                for ch in valid.chars() {
                    if ch == '\\' {
                        out.push_str("\\\\");
                    } else {
                        out.push(ch);
                    }
                }
                break;
            }
            Err(err) => {
                let (valid, rest) = bytes.split_at(err.valid_up_to());
                if !valid.is_empty() {
                    for ch in std::str::from_utf8(valid).unwrap_or_default().chars() {
                        if ch == '\\' {
                            out.push_str("\\\\");
                        } else {
                            out.push(ch);
                        }
                    }
                }
                let bad_len = err.error_len().unwrap_or(1);
                for &b in &rest[..bad_len] {
                    out.push_str(&format!("\\x{b:02X}"));
                }
                bytes = &rest[bad_len..];
            }
        }
    }
    out
}

/// Opens a regular file at `root/rel` safely with `O_NOFOLLOW | O_NOATIME | O_NONBLOCK`.
pub fn open_file_safe(root: &Path, rel: &Path) -> io::Result<(File, StatSig)> {
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let name = rel.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "relative file path has no name")
    })?;
    let dir_fd = open_rel_dir_safe(root, parent_rel)?;
    let cname = cstring_from_os(name)?;
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NOATIME | libc::O_NONBLOCK | libc::O_CLOEXEC;
    let fd = openat_raw(dir_fd.as_raw_fd(), &cname, flags)?;
    let sig = fstat_fd(fd.as_raw_fd())?;
    if !sig.is_reg() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((File::from(fd), sig))
}

/// Pure-Rust executable lookup. When running as `root` (`euid == 0`), searches ONLY trusted
/// system directories and never falls back to untrusted `$PATH`.
pub fn find_trusted_binary(program: &str) -> Option<PathBuf> {
    let trusted_dirs = [
        "/usr/bin",
        "/usr/sbin",
        "/bin",
        "/sbin",
        "/run/current-system/sw/bin",
    ];
    if unsafe { libc::geteuid() } == 0 {
        for dir in trusted_dirs {
            let full = Path::new(dir).join(program);
            if is_executable_file(&full) {
                return Some(full);
            }
        }
        return None;
    }
    find_in_path(program)
}

/// Ensures `dir_path` exists by walking from `/` with `O_NOFOLLOW`, creating any missing
/// directory components via `mkdirat` (mode `mode`) and optionally `fchown`ing every newly
/// created directory to `owner` (e.g. `SUDO_UID:SUDO_GID`).
pub fn ensure_dir_chain_owned(
    dir_path: &Path,
    mode: u32,
    owner: Option<(u32, u32)>,
) -> io::Result<OwnedFd> {
    let abs = if dir_path.is_absolute() {
        dir_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir_path)
    };
    let path_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let root_c = CString::new("/").unwrap();
    let fd = unsafe { libc::openat(libc::AT_FDCWD, root_c.as_ptr(), path_flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut cur = unsafe { OwnedFd::from_raw_fd(fd) };
    for comp in abs.components() {
        match comp {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(part) => {
                let cpart = cstring_from_os(part)?;
                let next = unsafe { libc::openat(cur.as_raw_fd(), cpart.as_ptr(), path_flags) };
                if next >= 0 {
                    cur = unsafe { OwnedFd::from_raw_fd(next) };
                } else {
                    let err = io::Error::last_os_error();
                    if err.kind() != io::ErrorKind::NotFound {
                        return Err(err);
                    }
                    let mk = unsafe { libc::mkdirat(cur.as_raw_fd(), cpart.as_ptr(), mode) };
                    let created = if mk == 0 {
                        true
                    } else {
                        let merr = io::Error::last_os_error();
                        if merr.kind() != io::ErrorKind::AlreadyExists {
                            return Err(merr);
                        }
                        false
                    };
                    let next2 = unsafe {
                        libc::openat(
                            cur.as_raw_fd(),
                            cpart.as_ptr(),
                            if created && owner.is_some() { dir_flags } else { path_flags },
                        )
                    };
                    if next2 < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let next_fd = unsafe { OwnedFd::from_raw_fd(next2) };
                    if created && let Some((uid, gid)) = owner {
                        let rc = unsafe { libc::fchown(next_fd.as_raw_fd(), uid, gid) };
                        if rc != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    cur = next_fd;
                }
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
    let final_fd = unsafe { libc::openat(cur.as_raw_fd(), dot.as_ptr(), dir_flags) };
    if final_fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(final_fd) })
    }
}

/// Pure-Rust executable lookup in `$PATH` (plus standard sbin fallbacks), avoiding external `which`.
pub fn find_in_path(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        return is_executable_file(candidate).then(|| candidate.to_path_buf());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let full = dir.join(program);
            if is_executable_file(&full) {
                return Some(full);
            }
        }
    }
    for fallback in ["/usr/bin", "/usr/sbin", "/bin", "/sbin"] {
        let full = Path::new(fallback).join(program);
        if is_executable_file(&full) {
            return Some(full);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

/// Formats a Unix timestamp (seconds) in local time as `YYYY-MM-DD HH:MM`.
pub fn format_datetime_local(secs: i64) -> String {
    let Some(tm) = localtime_tm(secs) else {
        return "—".to_string();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// Formats a `SystemTime` in local time as `YYYY-MM-DD HH:MM`.
pub fn format_system_time_local(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => format_datetime_local(d.as_secs() as i64),
        Err(_) => "—".to_string(),
    }
}

/// Formats a `SystemTime` in local time as `YYYY-MM-DD`.
pub fn format_day_local(time: SystemTime) -> String {
    let Ok(d) = time.duration_since(UNIX_EPOCH) else {
        return "unknown".to_string();
    };
    let Some(tm) = localtime_tm(d.as_secs() as i64) else {
        return "unknown".to_string();
    };
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

/// Formats a `SystemTime` in local time as `HH:MM`.
pub fn format_hm_local(time: SystemTime) -> String {
    let Ok(d) = time.duration_since(UNIX_EPOCH) else {
        return "--:--".to_string();
    };
    let Some(tm) = localtime_tm(d.as_secs() as i64) else {
        return "--:--".to_string();
    };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

fn localtime_tm(secs: i64) -> Option<libc::tm> {
    let t = secs as libc::time_t;
    let mut out = std::mem::MaybeUninit::<libc::tm>::zeroed();
    let ptr = unsafe { libc::localtime_r(&t, out.as_mut_ptr()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { out.assume_init() })
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::btrfs::{BtrfsFsInfoArgs, BtrfsSearchArgs, BtrfsSearchKey, BtrfsSubvolInfoArgs};

    #[test]
    fn btrfs_ioctl_structs_match_kernel_abi() {
        assert_eq!(std::mem::size_of::<BtrfsSubvolInfoArgs>(), 504);
        assert_eq!(std::mem::size_of::<BtrfsFsInfoArgs>(), 1024);
        assert_eq!(std::mem::size_of::<BtrfsSearchKey>(), 104);
        assert_eq!(std::mem::size_of::<BtrfsSearchArgs>(), 4096);
    }

    #[test]
    fn decode_mount_escapes_rejects_out_of_range_octal() {
        assert_eq!(decode_mount_escapes(r"/mnt/my\040disk"), "/mnt/my disk");
        // \777 = 511 > 255 must NOT overflow u8, left literal.
        assert_eq!(decode_mount_escapes(r"/mnt/bad\777dir"), r"/mnt/bad\777dir");
    }

    #[test]
    fn detects_middle_byte_change_above_2kib_edge_sample() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-mid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("l");
        let right = base.join("r");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();

        let mut a = vec![b'A'; 8192];
        let mut b = a.clone();
        // Keep first 1 KiB and last 1 KiB identical, flip a single byte in the middle.
        b[4096] = b'B';
        fs::write(left.join("big.bin"), &a).unwrap();
        fs::write(right.join("big.bin"), &b).unwrap();

        let (_, ls) = open_file_safe(&left, Path::new("big.bin")).unwrap();
        let (_, rs) = open_file_safe(&right, Path::new("big.bin")).unwrap();
        assert!(!files_content_equal(&left, &right, Path::new("big.bin"), &ls, &rs).unwrap());

        // Now make them identical and verify equality.
        a[4096] = b'B';
        fs::write(left.join("big.bin"), &a).unwrap();
        let (_, ls2) = open_file_safe(&left, Path::new("big.bin")).unwrap();
        assert!(files_content_equal(&left, &right, Path::new("big.bin"), &ls2, &rs).unwrap());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_symlink_and_parent_traversal() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-sym-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let real = base.join("real.txt");
        let link = base.join("link.txt");
        fs::write(&real, "secret").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(open_file_safe(&base, Path::new("link.txt")).is_err());
        assert!(open_file_safe(&base, Path::new("../etc/passwd")).is_err());
        assert!(open_rel_dir_safe(&base, Path::new("..")).is_err());
        assert!(find_in_path("sh").is_some());

        let _ = fs::remove_dir_all(&base);
    }
}
