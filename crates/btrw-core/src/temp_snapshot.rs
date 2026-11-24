use crate::fs_safe::{
    btrfs_fsid, btrfs_subvol_meta, decode_mount_escapes, find_trusted_binary, fstat_fd,
    is_btrfs_ro_subvolume, open_dir_safe,
};
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub const TEMP_WORK_DIR: &str = ".btrw-work";
pub const TEMP_SNAP_PREFIX: &str = ".btrw-tmp-";
const REGISTRY_FILE: &str = ".registry";

static SNAP_SEQ: AtomicU64 = AtomicU64::new(1);
static ACTIVE_TEMP_SNAPS: OnceLock<Mutex<Vec<RegisteredTempSnap>>> = OnceLock::new();
static REGISTRY_IO_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn registry_io_lock() -> &'static Mutex<()> {
    REGISTRY_IO_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredTempSnap {
    path: PathBuf,
    fsid: Option<[u8; 16]>,
    treeid: Option<u64>,
}

fn active_registry() -> &'static Mutex<Vec<RegisteredTempSnap>> {
    ACTIVE_TEMP_SNAPS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Returns true if `name` is an internal temporary snapshot or work directory created by `btrw` (Btrfs Rewind).
pub fn is_temp_snapshot_name(name: &str) -> bool {
    name == TEMP_WORK_DIR || name.starts_with(TEMP_SNAP_PREFIX)
}

/// Parses the creator PID from `.btrw-tmp-<pid>-<seq>`.
pub fn parse_temp_snapshot_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(TEMP_SNAP_PREFIX)?;
    let pid_str = rest.split('-').next()?;
    pid_str.parse::<u32>().ok()
}

fn is_pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// RAII guard for a short-lived read-only Btrfs snapshot of a live subvolume.
///
/// 3-layer leak protection against leftover temporary snapshots:
/// 1. `Drop` verifies `(fsid, treeid)` and deletes the snapshot immediately on normal completion,
///    error, `Esc`, or panic unwind.
/// 2. Global `ACTIVE_TEMP_SNAPS` registry is drained on clean shutdown (retaining entries if delete fails).
/// 3. Snapshots are confined inside a dedicated `0700` `.btrw-work/` directory on the
///    same Btrfs `FSID` (never arbitrary user directories), and stale dead-PID snapshots inside
///    that dedicated work dir are swept on the next run and hidden from discovery.
pub struct TempRoSnapshot {
    entry: RegisteredTempSnap,
    deleted: bool,
}

impl TempRoSnapshot {
    /// Creates a temporary read-only Btrfs snapshot of `live_root` inside a dedicated `0700`
    /// `.btrw-work` directory on the same Btrfs filesystem (`FSID`).
    pub fn create(live_root: &Path, snapshots_dir: &Path) -> Result<Self, String> {
        let btrfs_bin = find_trusted_binary("btrfs")
            .ok_or_else(|| "btrfs binary not found in PATH".to_string())?;
        let (parent_dir, live_fsid) = choose_temp_parent_by_fsid(live_root, snapshots_dir)?;
        let work_dir = ensure_private_work_dir(&parent_dir)?;

        // Only sweep dead-PID leftovers inside our own dedicated 0700 work directory.
        cleanup_stale_in_work_dir(&work_dir, live_fsid, &btrfs_bin);

        let pid = std::process::id();
        let seq = SNAP_SEQ.fetch_add(1, Ordering::Relaxed);
        let temp_path = work_dir.join(format!("{TEMP_SNAP_PREFIX}{pid}-{seq}"));

        let output = Command::new(&btrfs_bin)
            .args(["subvolume", "snapshot", "-r", "--"])
            .arg(live_root)
            .arg(&temp_path)
            .env("LC_ALL", "C")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to spawn btrfs subvolume snapshot: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("btrfs subvolume snapshot failed: {}", err.trim()));
        }

        let (fsid, treeid) = open_dir_safe(&temp_path)
            .ok()
            .map(|fd| {
                (
                    btrfs_fsid(fd.as_raw_fd()),
                    btrfs_subvol_meta(fd.as_raw_fd()).map(|m| m.treeid),
                )
            })
            .unwrap_or((Some(live_fsid), None));

        let entry = RegisteredTempSnap {
            path: temp_path,
            fsid,
            treeid,
        };
        if let (Some(f), Some(tid)) = (fsid, treeid)
            && let Some(name) = entry.path.file_name().and_then(|n| n.to_str())
        {
            if let Err(e) = persist_registry_add(&work_dir, f, tid, pid, name) {
                rollback_just_created_snapshot(&entry, &btrfs_bin);
                return Err(format!("failed to persist temp snapshot registry: {e}"));
            }
        } else {
            rollback_just_created_snapshot(&entry, &btrfs_bin);
            return Err("cannot determine Btrfs (FSID, treeid) of created temp snapshot".to_string());
        }
        if let Ok(mut reg) = active_registry().lock() {
            reg.push(entry.clone());
        }

        Ok(Self {
            entry,
            deleted: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.entry.path
    }

    pub fn delete_now(&mut self) {
        if self.deleted {
            return;
        }
        if delete_verified_subvolume(&self.entry) {
            self.deleted = true;
            if let Some(work_dir) = self.entry.path.parent()
                && let Some(name) = self.entry.path.file_name().and_then(|n| n.to_str())
            {
                let _ = persist_registry_remove(work_dir, name);
            }
            if let Ok(mut reg) = active_registry().lock() {
                reg.retain(|e| e.path != self.entry.path);
            }
        }
    }
}

impl Drop for TempRoSnapshot {
    fn drop(&mut self) {
        self.delete_now();
    }
}

/// Deletes all currently registered active temporary snapshots (retaining any that failed to delete).
pub fn cleanup_all_active_temp_snapshots() {
    let entries: Vec<RegisteredTempSnap> = if let Ok(reg) = active_registry().lock() {
        reg.clone()
    } else {
        return;
    };
    let mut still_active = Vec::new();
    for entry in entries {
        if delete_verified_subvolume(&entry) {
            if let Some(work_dir) = entry.path.parent()
                && let Some(name) = entry.path.file_name().and_then(|n| n.to_str())
            {
                let _ = persist_registry_remove(work_dir, name);
            }
        } else {
            still_active.push(entry);
        }
    }
    if let Ok(mut reg) = active_registry().lock() {
        *reg = still_active;
    }
}

fn ensure_private_work_dir(parent_dir: &Path) -> Result<PathBuf, String> {
    let work_dir = parent_dir.join(TEMP_WORK_DIR);
    let cpath = CString::new(work_dir.as_os_str().as_bytes())
        .map_err(|_| "NUL in work dir path".to_string())?;
    let rc = unsafe { libc::mkdir(cpath.as_ptr(), 0o700) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(format!("cannot create temp work dir: {err}"));
        }
    }
    // Verify work_dir is a real directory (not a symlink), owned by current euid, and mode 0700.
    let fd = open_dir_safe(&work_dir)
        .map_err(|e| format!("cannot open temp work dir safely: {e}"))?;
    let st = fstat_fd(fd.as_raw_fd()).map_err(|e| format!("cannot stat temp work dir: {e}"))?;
    let euid = unsafe { libc::geteuid() };
    if st.uid != euid {
        return Err(format!(
            "refusing temp work dir owned by uid {} (expected {})",
            st.uid, euid
        ));
    }
    if (st.mode & 0o777) != 0o700 && unsafe { libc::fchmod(fd.as_raw_fd(), 0o700) } != 0 {
        return Err(format!(
            "cannot enforce 0700 on temp work dir: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(work_dir)
}

fn format_fsid_hex(fsid: [u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in fsid {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_fsid_hex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistedEntry {
    fsid: [u8; 16],
    treeid: u64,
    pid: u32,
    name: String,
}

fn verify_work_dir_trusted(work_dir: &Path) -> Result<OwnedFd, String> {
    let fd = open_dir_safe(work_dir)
        .map_err(|e| format!("cannot open temp work dir safely: {e}"))?;
    let st = fstat_fd(fd.as_raw_fd()).map_err(|e| format!("cannot stat temp work dir: {e}"))?;
    let euid = unsafe { libc::geteuid() };
    if st.uid != euid {
        return Err(format!(
            "refusing temp work dir owned by uid {} (expected {})",
            st.uid, euid
        ));
    }
    if (st.mode & 0o777) != 0o700 {
        return Err(format!(
            "refusing temp work dir with mode {:03o} (expected 700)",
            st.mode & 0o777
        ));
    }
    Ok(fd)
}

fn read_registry(work_dir: &Path) -> Vec<PersistedEntry> {
    use std::io::Read;
    let Ok(dir_fd) = verify_work_dir_trusted(work_dir) else {
        return Vec::new();
    };
    let reg_c = CString::new(REGISTRY_FILE).unwrap();
    let fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            reg_c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Vec::new();
    }
    let mut file = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let Ok(st) = fstat_fd(file.as_raw_fd()) else {
        return Vec::new();
    };
    let euid = unsafe { libc::geteuid() };
    if !st.is_reg() || st.uid != euid || (st.mode & 0o777) != 0o600 {
        return Vec::new();
    }
    let mut raw = String::new();
    if file.read_to_string(&mut raw).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 4
            && let (Some(fsid), Ok(treeid), Ok(pid)) = (
                parse_fsid_hex(parts[0]),
                parts[1].parse::<u64>(),
                parts[2].parse::<u32>(),
            )
        {
            let name = parts[3];
            // Validate strictly single normal path component.
            if !name.is_empty() && !name.contains('/') && name != "." && name != ".." && treeid != 0 {
                out.push(PersistedEntry {
                    fsid,
                    treeid,
                    pid,
                    name: name.to_string(),
                });
            }
        }
    }
    out
}

fn write_registry(work_dir: &Path, entries: &[PersistedEntry]) -> Result<(), String> {
    let dir_fd = open_dir_safe(work_dir).map_err(|e| format!("open work dir: {e}"))?;
    let tmp_name = format!(".registry.tmp.{}", std::process::id());
    let tmp_c = CString::new(tmp_name.as_bytes()).unwrap();
    let reg_c = CString::new(REGISTRY_FILE).unwrap();
    let fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            tmp_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(format!(
            "create temp registry failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut file = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    for e in entries {
        let line = format!("{} {} {} {}\n", format_fsid_hex(e.fsid), e.treeid, e.pid, e.name);
        if let Err(err) = file.write_all(line.as_bytes()) {
            drop(file);
            unsafe {
                libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
            }
            return Err(format!("write registry: {err}"));
        }
    }
    let _ = file.sync_all();
    drop(file);
    let rc = unsafe {
        libc::renameat(
            dir_fd.as_raw_fd(),
            tmp_c.as_ptr(),
            dir_fd.as_raw_fd(),
            reg_c.as_ptr(),
        )
    };
    if rc != 0 {
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
        }
        return Err(format!(
            "rename registry failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn persist_registry_add(
    work_dir: &Path,
    fsid: [u8; 16],
    treeid: u64,
    pid: u32,
    name: &str,
) -> Result<(), String> {
    let _guard = registry_io_lock()
        .lock()
        .map_err(|_| "registry lock poisoned".to_string())?;
    let mut entries = read_registry(work_dir);
    entries.retain(|e| e.name != name);
    entries.push(PersistedEntry {
        fsid,
        treeid,
        pid,
        name: name.to_string(),
    });
    write_registry(work_dir, &entries)
}

fn persist_registry_remove(work_dir: &Path, name: &str) -> Result<(), String> {
    let _guard = registry_io_lock()
        .lock()
        .map_err(|_| "registry lock poisoned".to_string())?;
    let mut entries = read_registry(work_dir);
    let before = entries.len();
    entries.retain(|e| e.name != name);
    if entries.len() != before {
        write_registry(work_dir, &entries)?;
    }
    Ok(())
}

/// Best-effort startup sweep of explicitly registered orphaned temp snapshots under
/// `snapshots_dir/.btrw-work` and any writable top-level `subvol=/` mount.
pub fn cleanup_orphaned_on_startup(snapshots_dir: &Path) -> usize {
    let Some(btrfs_bin) = find_trusted_binary("btrfs") else {
        return 0;
    };
    let mut total = 0usize;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if !snapshots_dir.as_os_str().is_empty() {
        candidates.push(snapshots_dir.join(TEMP_WORK_DIR));
    }
    if let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") {
        for line in mountinfo.lines() {
            let Some((pre, post)) = line.split_once(" - ") else {
                continue;
            };
            let pre_fields: Vec<&str> = pre.split_whitespace().collect();
            let post_fields: Vec<&str> = post.split_whitespace().collect();
            if pre_fields.len() < 5 || post_fields.len() < 3 || post_fields[0] != "btrfs" {
                continue;
            }
            let subvol = post_fields[2]
                .split(',')
                .find_map(|o| o.strip_prefix("subvol="))
                .map(decode_mount_escapes)
                .unwrap_or_else(|| decode_mount_escapes(pre_fields[3]));
            if subvol == "/" {
                candidates.push(PathBuf::from(decode_mount_escapes(pre_fields[4])).join(TEMP_WORK_DIR));
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    for work_dir in candidates {
        if let Ok(fd) = open_dir_safe(&work_dir)
            && let Some(fsid) = btrfs_fsid(fd.as_raw_fd())
        {
            drop(fd);
            total += cleanup_stale_in_work_dir(&work_dir, fsid, &btrfs_bin);
        }
    }
    total
}

/// Sweeps ONLY explicitly registered orphaned RO subvolumes inside our private `work_dir`
/// whose creator PID is dead and whose `(fsid, treeid)` match the persisted registry.
/// Unrecorded RO subvolumes are NEVER touched.
pub fn cleanup_stale_in_work_dir(
    work_dir: &Path,
    expected_fsid: [u8; 16],
    btrfs_bin: &Path,
) -> usize {
    let Ok(_guard) = registry_io_lock().lock() else {
        return 0;
    };
    let entries = read_registry(work_dir);
    if entries.is_empty() {
        return 0;
    }
    let mut remaining = Vec::new();
    let mut removed = 0usize;
    for rec in entries {
        if rec.fsid != expected_fsid || is_pid_alive(rec.pid) {
            remaining.push(rec);
            continue;
        }
        let snap = RegisteredTempSnap {
            path: work_dir.join(&rec.name),
            fsid: Some(rec.fsid),
            treeid: Some(rec.treeid),
        };
        if delete_verified_subvolume_with_bin(&snap, btrfs_bin) {
            removed += 1;
        } else {
            remaining.push(rec);
        }
    }
    let _ = write_registry(work_dir, &remaining);
    removed
}

/// Rollback helper used ONLY inside `TempRoSnapshot::create` when metadata/registry
/// persistence fails right after we created `entry.path` inside our private `0700` work dir.
fn rollback_just_created_snapshot(entry: &RegisteredTempSnap, btrfs_bin: &Path) {
    if delete_verified_subvolume_with_bin(entry, btrfs_bin) {
        return;
    }
    if let Ok(fd) = open_dir_safe(&entry.path)
        && is_btrfs_ro_subvolume(fd.as_raw_fd())
    {
        drop(fd);
        let _ = Command::new(btrfs_bin)
            .args(["subvolume", "delete", "--"])
            .arg(&entry.path)
            .env("LC_ALL", "C")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn delete_verified_subvolume(entry: &RegisteredTempSnap) -> bool {
    let Some(btrfs_bin) = find_trusted_binary("btrfs") else {
        return false;
    };
    delete_verified_subvolume_with_bin(entry, &btrfs_bin)
}

fn delete_verified_subvolume_with_bin(entry: &RegisteredTempSnap, btrfs_bin: &Path) -> bool {
    if !entry.path.exists() {
        return true;
    }
    // Verify identity (RO subvolume + matching FSID and treeid) before deleting!
    let Ok(fd) = open_dir_safe(&entry.path) else {
        return false;
    };
    if !is_btrfs_ro_subvolume(fd.as_raw_fd()) {
        return false;
    }
    let actual_fsid = btrfs_fsid(fd.as_raw_fd());
    if let Some(exp_fsid) = entry.fsid
        && actual_fsid != Some(exp_fsid)
    {
        return false;
    }
    let Some(verified_fsid) = entry.fsid.or(actual_fsid) else {
        return false;
    };
    let actual_treeid = btrfs_subvol_meta(fd.as_raw_fd()).map(|m| m.treeid);
    let Some(treeid) = entry.treeid.or(actual_treeid) else {
        return false;
    };
    if let Some(exp_treeid) = entry.treeid
        && actual_treeid != Some(exp_treeid)
    {
        return false;
    }
    drop(fd);

    // `btrfs subvolume delete --subvolid <id> <dir>` requires `<dir>` to be a Btrfs subvolume
    // root (`ino == 256`), NOT the plain `.btrw-work` subdirectory inside it.
    let Some(subvol_root) = find_enclosing_btrfs_subvol_root(entry.path.parent(), verified_fsid)
    else {
        return false;
    };
    Command::new(btrfs_bin)
        .args(["subvolume", "delete", "--subvolid", &treeid.to_string(), "--"])
        .arg(&subvol_root)
        .env("LC_ALL", "C")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Walks up from `start` until finding the enclosing Btrfs subvolume root (`ino == 256`)
/// on the exact `expected_fsid`.
fn find_enclosing_btrfs_subvol_root(start: Option<&Path>, expected_fsid: [u8; 16]) -> Option<PathBuf> {
    let mut cur = start?.to_path_buf();
    for _ in 0..32 {
        if let Ok(fd) = open_dir_safe(&cur)
            && let Ok(st) = fstat_fd(fd.as_raw_fd())
            && btrfs_fsid(fd.as_raw_fd()) == Some(expected_fsid)
            && st.is_dir()
            && st.ino == 256
        {
            return Some(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// Picks a writable parent directory on the **same Btrfs FSID** as `live_root`
/// (note: Btrfs assigns distinct synthetic `st_dev` per subvolume, so `FSID` must be used!).
fn choose_temp_parent_by_fsid(
    live_root: &Path,
    snapshots_dir: &Path,
) -> Result<(PathBuf, [u8; 16]), String> {
    let live_fd = open_dir_safe(live_root).map_err(|e| format!("cannot open live root: {e}"))?;
    let live_st = fstat_fd(live_fd.as_raw_fd()).map_err(|e| format!("cannot stat live root: {e}"))?;
    let live_fsid = btrfs_fsid(live_fd.as_raw_fd())
        .ok_or_else(|| "live root is not on a Btrfs filesystem".to_string())?;

    // 1. Prefer top-level `subvol=/` mount on the same Btrfs FSID (outside live subvolume).
    if let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") {
        for line in mountinfo.lines() {
            let Some((pre, post)) = line.split_once(" - ") else {
                continue;
            };
            let pre_fields: Vec<&str> = pre.split_whitespace().collect();
            let post_fields: Vec<&str> = post.split_whitespace().collect();
            if pre_fields.len() < 5 || post_fields.len() < 3 || post_fields[0] != "btrfs" {
                continue;
            }
            let subvol = post_fields[2]
                .split(',')
                .find_map(|o| o.strip_prefix("subvol="))
                .map(decode_mount_escapes)
                .unwrap_or_else(|| decode_mount_escapes(pre_fields[3]));
            if subvol == "/" {
                let mp = PathBuf::from(decode_mount_escapes(pre_fields[4]));
                if same_writable_btrfs_outside(&mp, live_fsid, live_st.dev) {
                    return Ok((mp, live_fsid));
                }
            }
        }
    }

    // 2. Next prefer `snapshots_dir` if provided, writable, on the same Btrfs FSID, and outside live subvolume.
    if !snapshots_dir.as_os_str().is_empty()
        && snapshots_dir.is_dir()
        && same_writable_btrfs_outside(snapshots_dir, live_fsid, live_st.dev)
    {
        return Ok((snapshots_dir.to_path_buf(), live_fsid));
    }

    Err("no writable directory outside live subvolume found on target Btrfs FSID".to_string())
}

fn same_writable_btrfs_outside(dir: &Path, expected_fsid: [u8; 16], live_subvol_dev: u64) -> bool {
    let Ok(fd) = open_dir_safe(dir) else {
        return false;
    };
    let Ok(st) = fstat_fd(fd.as_raw_fd()) else {
        return false;
    };
    // Must be on a DIFFERENT subvolume (st_dev != live_subvol_dev) so we never nest inside the live subvolume.
    if st.dev == live_subvol_dev {
        return false;
    }
    if is_btrfs_ro_subvolume(fd.as_raw_fd()) {
        return false;
    }
    if btrfs_fsid(fd.as_raw_fd()) != Some(expected_fsid) {
        return false;
    }
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::fstatvfs(fd.as_raw_fd(), vfs.as_mut_ptr()) } != 0 {
        return false;
    }
    let vfs = unsafe { vfs.assume_init() };
    (vfs.f_flag & libc::ST_RDONLY) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_and_parses_temp_snapshot_names() {
        assert!(is_temp_snapshot_name(".btrw-work"));
        assert!(is_temp_snapshot_name(".btrw-tmp-12345-1"));
        assert_eq!(parse_temp_snapshot_pid(".btrw-tmp-12345-1"), Some(12345));
        assert!(!is_temp_snapshot_name("@home.20261124T0800"));
        assert_eq!(parse_temp_snapshot_pid("@home.20261124T0800"), None);
    }

    #[test]
    fn dead_pid_detection_works() {
        assert!(is_pid_alive(std::process::id()));
        assert!(!is_pid_alive(999_999_999));
    }

    #[test]
    fn unrecorded_entry_in_work_dir_is_never_deleted() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-unrec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let work = base.join(TEMP_WORK_DIR);
        let unrecorded = work.join(".btrw-tmp-999999999-1");
        fs::create_dir_all(&unrecorded).unwrap();

        let removed = cleanup_stale_in_work_dir(&work, [1u8; 16], Path::new("/bin/true"));
        assert_eq!(removed, 0);
        assert!(unrecorded.exists(), "unrecorded entry must never be deleted");

        let _ = fs::remove_dir_all(&base);
    }
}
