use crate::compare::{DirCompareResult, DirEntryInfo, PaneRow};
use crate::fs_safe::{
    files_content_equal_cancellable, find_trusted_binary, format_os_display, lstat_at,
    open_rel_dir_safe, read_dir_names_fd, readlink_at,
};
use crate::model::{ChangeKind, EntryKind};
use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::io::{self, BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const STREAM_MAGIC: &[u8; 13] = b"btrfs-stream\0";

// Linux kernel fs/btrfs/send.h command IDs.
const CMD_MKFILE: u16 = 3;
const CMD_MKDIR: u16 = 4;
const CMD_MKNOD: u16 = 5;
const CMD_MKFIFO: u16 = 6;
const CMD_MKSOCK: u16 = 7;
const CMD_SYMLINK: u16 = 8;
const CMD_RENAME: u16 = 9;
const CMD_LINK: u16 = 10;
const CMD_UNLINK: u16 = 11;
const CMD_RMDIR: u16 = 12;
const CMD_SET_XATTR: u16 = 13;
const CMD_REMOVE_XATTR: u16 = 14;
const CMD_WRITE: u16 = 15;
const CMD_CLONE: u16 = 16;
const CMD_TRUNCATE: u16 = 17;
const CMD_CHMOD: u16 = 18;
const CMD_CHOWN: u16 = 19;
const CMD_UTIMES: u16 = 20;
const CMD_END: u16 = 21;
const CMD_UPDATE_EXTENT: u16 = 22;
const CMD_FALLOCATE: u16 = 23;
const CMD_FILEATTR: u16 = 24;
const CMD_ENCODED_WRITE: u16 = 25;

// Linux kernel fs/btrfs/send.h attribute IDs.
const ATTR_PATH: u16 = 15;
const ATTR_PATH_TO: u16 = 16;
const ATTR_PATH_LINK: u16 = 17;

#[derive(Debug, Default)]
pub struct SendStreamSummary {
    pub content_paths: BTreeSet<PathBuf>,
    pub metadata_paths: BTreeSet<PathBuf>,
    pub deleted_paths: BTreeSet<PathBuf>,
    /// Non-orphan `(from, to)` renames observed in the stream so directory renames
    /// can expand unchanged descendants from both snapshot trees.
    pub renamed_pairs: Vec<(PathBuf, PathBuf)>,
    pub commands_parsed: usize,
}

/// Parses a binary Btrfs send stream (`--proto 1 --no-data`) directly instead of
/// piping to `btrfs receive` (zero disk writes, uniform 16-bit TLV headers, no text escaping).
///
/// Btrfs send emits low-level mutations using temporary orphan names (`o<ino>-<gen>-0`)
/// before renaming them into place. This state machine tracks created/renamed subtrees
/// to recover the final real relative paths touched between the two snapshots.
pub fn parse_send_stream<R, F>(
    mut reader: R,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<SendStreamSummary, String>
where
    R: Read,
    F: FnMut(usize, &Path),
{
    let mut header = [0u8; 17];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("failed to read btrfs send header: {e}"))?;
    if &header[..13] != STREAM_MAGIC {
        return Err("invalid btrfs send stream magic".to_string());
    }
    let version = u32::from_le_bytes([header[13], header[14], header[15], header[16]]);
    if version != 1 && version != 2 {
        return Err(format!("unsupported btrfs send stream version: {version}"));
    }

    let mut created: HashSet<PathBuf> = HashSet::new();
    // Tracks paths created in this stream as synthetic Btrfs orphan placeholders (`o<ino>-<gen>-<idx>`).
    let mut synthetic_orphans: HashSet<PathBuf> = HashSet::new();
    let mut content: HashSet<PathBuf> = HashSet::new();
    let mut metadata: HashSet<PathBuf> = HashSet::new();
    let mut deleted: HashSet<PathBuf> = HashSet::new();
    let mut renamed_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut commands = 0usize;
    let mut last_emit = Instant::now();
    let mut payload = Vec::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let mut cmd_hdr = [0u8; 10];
        match reader.read_exact(&mut cmd_hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err("truncated btrfs send stream (missing END command)".to_string());
            }
            Err(e) => return Err(format!("failed to read send command header: {e}")),
        }
        let payload_len = u32::from_le_bytes([cmd_hdr[0], cmd_hdr[1], cmd_hdr[2], cmd_hdr[3]]) as usize;
        let cmd = u16::from_le_bytes([cmd_hdr[4], cmd_hdr[5]]);
        // bytes 6..10 are CRC32C

        if payload_len > 64 * 1024 * 1024 {
            return Err(format!("btrfs send command payload too large: {payload_len}"));
        }
        payload.resize(payload_len, 0);
        if payload_len > 0 {
            reader
                .read_exact(&mut payload)
                .map_err(|e| format!("failed to read send command payload: {e}"))?;
        }

        commands += 1;
        if cmd == CMD_END {
            if payload_len != 0 {
                return Err(format!(
                    "corrupt btrfs send END command with non-zero payload ({payload_len} bytes)"
                ));
            }
            break;
        }

        let (path, path_to, path_link) = extract_paths(&payload)?;

        if last_emit.elapsed() >= Duration::from_millis(250) {
            if let Some(p) = path_to.as_ref().or(path.as_ref()) {
                on_progress(commands, p);
            }
            last_emit = Instant::now();
        }

        match cmd {
            CMD_MKFILE | CMD_MKDIR | CMD_MKNOD | CMD_MKFIFO | CMD_MKSOCK | CMD_SYMLINK
            | CMD_LINK => {
                if let Some(p) = path {
                    if is_orphan_temp_name(&p) {
                        synthetic_orphans.insert(p.clone());
                    }
                    created.insert(p.clone());
                    content.insert(p.clone());
                    deleted.remove(&p);
                }
                if cmd == CMD_LINK
                    && let Some(link_target) = path_link
                {
                    content.insert(link_target);
                }
            }
            CMD_WRITE | CMD_CLONE | CMD_TRUNCATE | CMD_UPDATE_EXTENT | CMD_FALLOCATE
            | CMD_ENCODED_WRITE => {
                if let Some(p) = path {
                    content.insert(p);
                }
            }
            CMD_CHMOD | CMD_CHOWN | CMD_UTIMES | CMD_SET_XATTR | CMD_REMOVE_XATTR
            | CMD_FILEATTR => {
                if let Some(p) = path {
                    metadata.insert(p);
                }
            }
            CMD_RENAME => {
                if let (Some(from), Some(to)) = (path, path_to) {
                    let was_orphan = synthetic_orphans.remove(&from);
                    let was_created = move_set_prefix(&mut created, &from, &to);
                    move_set_prefix(&mut content, &from, &to);
                    move_set_prefix(&mut metadata, &from, &to);
                    deleted.remove(&to);
                    content.insert(to.clone());
                    if !was_created && !was_orphan {
                        deleted.insert(from.clone());
                        renamed_pairs.push((from, to));
                    }
                }
            }
            CMD_UNLINK | CMD_RMDIR => {
                if let Some(p) = path {
                    let was_orphan = synthetic_orphans.remove(&p);
                    let was_created = created.remove(&p);
                    content.remove(&p);
                    metadata.remove(&p);
                    if !was_created && !was_orphan {
                        deleted.insert(p);
                    }
                }
            }
            _ => {}
        }
    }

    let filter_real = |set: HashSet<PathBuf>| -> BTreeSet<PathBuf> {
        set.into_iter()
            .filter(|p| !p.as_os_str().is_empty() && !synthetic_orphans.contains(p))
            .collect()
    };

    Ok(SendStreamSummary {
        content_paths: filter_real(content),
        metadata_paths: filter_real(metadata),
        deleted_paths: filter_real(deleted),
        renamed_pairs,
        commands_parsed: commands,
    })
}

fn extract_paths(
    payload: &[u8],
) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>), String> {
    let mut pos = 0usize;
    let mut path = None;
    let mut path_to = None;
    let mut path_link = None;

    while pos + 4 <= payload.len() {
        let attr_type = u16::from_le_bytes([payload[pos], payload[pos + 1]]);
        let attr_len = u16::from_le_bytes([payload[pos + 2], payload[pos + 3]]) as usize;
        pos += 4;
        if pos + attr_len > payload.len() {
            return Err("truncated TLV attribute in btrfs send stream".to_string());
        }
        let data = &payload[pos..pos + attr_len];
        pos += attr_len;

        if attr_type == ATTR_PATH {
            path = Some(PathBuf::from(OsStr::from_bytes(data)));
        } else if attr_type == ATTR_PATH_TO {
            path_to = Some(PathBuf::from(OsStr::from_bytes(data)));
        } else if attr_type == ATTR_PATH_LINK {
            path_link = Some(PathBuf::from(OsStr::from_bytes(data)));
        }
    }
    Ok((path, path_to, path_link))
}

fn move_set_prefix(set: &mut HashSet<PathBuf>, from: &Path, to: &Path) -> bool {
    let mut moved_exact = false;
    let to_move: Vec<PathBuf> = set
        .iter()
        .filter(|p| *p == from || p.starts_with(from))
        .cloned()
        .collect();
    for old in to_move {
        set.remove(&old);
        if old == from {
            moved_exact = true;
            set.insert(to.to_path_buf());
        } else if let Ok(suffix) = old.strip_prefix(from) {
            set.insert(to.join(suffix));
        }
    }
    moved_exact
}

/// Detects Btrfs send's synthetic temporary orphan names (`o<ino>-<gen>-<idx>`).
fn is_orphan_temp_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(rest) = name.strip_prefix('o') else {
        return false;
    };
    let mut parts = rest.split('-');
    let Some(a) = parts.next() else { return false };
    let Some(b) = parts.next() else { return false };
    let Some(c) = parts.next() else { return false };
    parts.next().is_none()
        && !a.is_empty()
        && !b.is_empty()
        && !c.is_empty()
        && a.bytes().all(|x| x.is_ascii_digit())
        && b.bytes().all(|x| x.is_ascii_digit())
        && c.bytes().all(|x| x.is_ascii_digit())
}

/// Computes exact differences between two **read-only** Btrfs snapshots using
/// `btrfs send --proto 1 --no-data -p <left_root> <right_root>`.
///
/// `--proto 1` guarantees uniform 16-bit TLV attribute lengths across `btrfs-progs` versions;
/// `--no-data` replaces `WRITE` payloads with `UPDATE_EXTENT` metadata (no file data read).
/// Candidate paths from the stream are then `lstat`-verified on both sides for exact sizes/deltas.
pub fn diff_snapshots_via_send<F>(
    left_root: &Path,
    right_root: &Path,
    scope_rel_dir: &Path,
    show_metadata_only: bool,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<DirCompareResult, String>
where
    F: FnMut(usize, &Path),
{
    let btrfs_bin =
        find_trusted_binary("btrfs").ok_or_else(|| "btrfs binary not found in PATH".to_string())?;
    let mut child = Command::new(btrfs_bin)
        .args(["send", "--proto", "1", "--no-data", "-p"])
        .arg(left_root)
        .arg(right_root)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn btrfs send: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "btrfs send stdout unavailable".to_string())?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "btrfs send stderr unavailable".to_string())?;

    // Drain stderr concurrently to prevent pipe-buffer deadlock when btrfs send logs warnings/errors.
    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        let mut r = stderr_pipe;
        let _ = r.read_to_string(&mut buf);
        buf
    });

    // Bind cancellation to the exact child instance via Linux `pidfd_open` + `pidfd_send_signal`
    // (eliminates any PID reuse race after `child.wait()`).
    let child_pid = child.id() as libc::pid_t;
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child_pid, 0 as libc::c_uint) as i32 };
    let finished = Arc::new(AtomicBool::new(false));
    let finished_w = finished.clone();
    let (parse_res, wait_res) = thread::scope(|s| {
        s.spawn(move || {
            while !finished_w.load(Ordering::Acquire) {
                if cancel.load(Ordering::Relaxed) {
                    if pidfd >= 0 {
                        unsafe {
                            libc::syscall(
                                libc::SYS_pidfd_send_signal,
                                pidfd,
                                libc::SIGKILL,
                                std::ptr::null::<libc::siginfo_t>(),
                                0 as libc::c_uint,
                            );
                        }
                    } else {
                        unsafe {
                            libc::kill(child_pid, libc::SIGKILL);
                        }
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(30));
            }
        });
        let res = parse_send_stream(
            BufReader::with_capacity(128 * 1024, stdout),
            cancel,
            &mut on_progress,
        );
        if cancel.load(Ordering::Relaxed) || res.is_err() {
            let _ = child.kill();
        }
        // If pidfd is unavailable, stop the watcher BEFORE `child.wait()` so a recycled
        // numeric PID can never be signalled after reaping.
        if pidfd < 0 {
            finished.store(true, Ordering::Release);
        }
        let wait = child.wait();
        finished.store(true, Ordering::Release);
        (res, wait)
    });
    if pidfd >= 0 {
        unsafe {
            libc::close(pidfd);
        }
    }

    let output_status = wait_res.map_err(|e| format!("btrfs send wait failed: {e}"))?;
    let stderr_buf = stderr_handle.join().unwrap_or_default();

    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }

    let summary = match parse_res {
        Ok(s) if output_status.success() => s,
        Ok(_) => {
            let msg = stderr_buf.trim();
            return Err(if msg.is_empty() {
                format!("btrfs send exited with {output_status}")
            } else {
                msg.to_string()
            });
        }
        Err(e) => {
            let msg = stderr_buf.trim();
            return Err(if msg.is_empty() {
                e
            } else {
                format!("{e} ({msg})")
            });
        }
    };

    materialize_send_summary(
        left_root,
        right_root,
        scope_rel_dir,
        show_metadata_only,
        &summary,
        cancel,
    )
}

/// Expands renamed directory subtrees and verifies all candidate paths from a parsed
/// `SendStreamSummary` against `left_root` and `right_root`.
pub fn materialize_send_summary(
    left_root: &Path,
    right_root: &Path,
    scope_rel_dir: &Path,
    show_metadata_only: bool,
    summary: &SendStreamSummary,
    cancel: &AtomicBool,
) -> Result<DirCompareResult, String> {
    let mut all_candidates: BTreeSet<PathBuf> = BTreeSet::new();
    all_candidates.extend(summary.content_paths.iter().cloned());
    all_candidates.extend(summary.deleted_paths.iter().cloned());
    if show_metadata_only {
        all_candidates.extend(summary.metadata_paths.iter().cloned());
    }

    let mut errors = Vec::new();

    // Expand renamed directory subtrees on BOTH sides before scope filtering so
    // unchanged descendants inside renamed directories are never missed.
    for (from, to) in &summary.renamed_pairs {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        expand_if_dir(left_root, from, &mut all_candidates, &mut errors, cancel)?;
        expand_if_dir(right_root, to, &mut all_candidates, &mut errors, cancel)?;
    }

    let mut rows = Vec::new();

    for rel in all_candidates {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if !scope_rel_dir.as_os_str().is_empty()
            && rel != scope_rel_dir
            && !rel.starts_with(scope_rel_dir)
        {
            continue;
        }

        let left_res = stat_entry(left_root, &rel);
        let right_res = stat_entry(right_root, &rel);

        let (left, right, stat_err) = match (left_res, right_res) {
            (Ok(l), Ok(r)) => (l, r, None),
            (Err(e), Ok(r)) => (None, r, Some(format!("left: {e}"))),
            (Ok(l), Err(e)) => (l, None, Some(format!("right: {e}"))),
            (Err(el), Err(er)) => (None, None, Some(format!("left: {el}; right: {er}"))),
        };

        let mut error = stat_err;
        let status = if let Some(ref err_msg) = error {
            errors.push(format!("{}: {err_msg}", format_os_display(rel.as_os_str())));
            Some(ChangeKind::Error)
        } else {
            match (&left, &right) {
                (None, Some(_)) => Some(ChangeKind::Added),
                (Some(_), None) => Some(ChangeKind::Deleted),
                (Some(l), Some(r)) => {
                    if l.sig.file_type() != r.sig.file_type() {
                        Some(ChangeKind::TypeChanged)
                    } else if l.kind == EntryKind::Symlink {
                        if l.target_raw != r.target_raw {
                            Some(ChangeKind::Modified)
                        } else if show_metadata_only && summary.metadata_paths.contains(&rel) {
                            Some(ChangeKind::MetadataOnly)
                        } else {
                            None
                        }
                    } else if l.kind == EntryKind::File {
                        let check_content =
                            l.sig.size != r.sig.size || summary.content_paths.contains(&rel);
                        if check_content {
                            match files_content_equal_cancellable(
                                left_root, right_root, &rel, &l.sig, &r.sig, cancel,
                            ) {
                                Ok(true) => {
                                    if show_metadata_only && summary.metadata_paths.contains(&rel) {
                                        Some(ChangeKind::MetadataOnly)
                                    } else {
                                        None
                                    }
                                }
                                Ok(false) => Some(ChangeKind::Modified),
                                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                                    return Err("cancelled".to_string());
                                }
                                Err(e) => {
                                    error = Some(e.to_string());
                                    errors.push(format!(
                                        "{}: {e}",
                                        format_os_display(rel.as_os_str())
                                    ));
                                    Some(ChangeKind::Error)
                                }
                            }
                        } else if show_metadata_only && summary.metadata_paths.contains(&rel) {
                            Some(ChangeKind::MetadataOnly)
                        } else {
                            None
                        }
                    } else if show_metadata_only && summary.metadata_paths.contains(&rel) {
                        Some(ChangeKind::MetadataOnly)
                    } else {
                        None
                    }
                }
                (None, None) => None,
            }
        };

        if status.is_some() {
            let delta_bytes = {
                let reg_or_none = |e: Option<&DirEntryInfo>| {
                    e.map(|i| i.kind == EntryKind::File).unwrap_or(true)
                };
                if reg_or_none(left.as_ref()) && reg_or_none(right.as_ref()) {
                    let l = left.as_ref().and_then(|e| e.size).unwrap_or(0) as i128;
                    let r = right.as_ref().and_then(|e| e.size).unwrap_or(0) as i128;
                    Some(r - l)
                } else {
                    None
                }
            };
            rows.push(PaneRow {
                name: format_os_display(rel.as_os_str()),
                rel_path: rel,
                left,
                right,
                status,
                delta_bytes,
                error,
            });
        }
    }

    Ok(DirCompareResult {
        rows,
        excluded: Vec::new(),
        errors,
    })
}

fn expand_if_dir(
    root: &Path,
    rel_dir: &Path,
    out: &mut BTreeSet<PathBuf>,
    errors: &mut Vec<String>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let dir_fd = match open_rel_dir_safe(root, rel_dir) {
        Ok(fd) => fd,
        Err(e) if is_not_found_or_not_dir(&e) => return Ok(()),
        Err(e) => {
            errors.push(format!(
                "{}: {e}",
                format_os_display(rel_dir.as_os_str())
            ));
            return Ok(());
        }
    };
    drop(dir_fd);

    let mut stack = vec![rel_dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let fd = match open_rel_dir_safe(root, &cur) {
            Ok(fd) => fd,
            Err(e) if is_not_found_or_not_dir(&e) => continue,
            Err(e) => {
                errors.push(format!("{}: {e}", format_os_display(cur.as_os_str())));
                continue;
            }
        };
        let names = match read_dir_names_fd(fd.as_raw_fd()) {
            Ok(list) => list,
            Err(e) => {
                errors.push(format!("{}: {e}", format_os_display(cur.as_os_str())));
                continue;
            }
        };
        for name in names {
            let child_rel = cur.join(&name);
            out.insert(child_rel.clone());
            if let Ok(sig) = lstat_at(fd.as_raw_fd(), &name)
                && sig.is_dir()
            {
                stack.push(child_rel);
            }
        }
    }
    Ok(())
}

fn is_not_found_or_not_dir(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
        || matches!(e.raw_os_error(), Some(libc::ENOTDIR) | Some(libc::ELOOP))
}

fn stat_entry(root: &Path, rel: &Path) -> io::Result<Option<DirEntryInfo>> {
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let Some(name_os) = rel.file_name() else {
        return Ok(None);
    };
    let dir_fd = match open_rel_dir_safe(root, parent_rel) {
        Ok(fd) => fd,
        Err(e) if is_not_found_or_not_dir(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let sig = match lstat_at(dir_fd.as_raw_fd(), name_os) {
        Ok(s) => s,
        Err(e) if is_not_found_or_not_dir(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
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
        let raw = readlink_at(dir_fd.as_raw_fd(), name_os)?.into_os_string();
        let disp = format_os_display(&raw);
        (Some(disp), Some(raw))
    } else {
        (None, None)
    };
    Ok(Some(DirEntryInfo {
        name: format_os_display(name_os),
        rel_path: rel.to_path_buf(),
        kind,
        size: if kind == EntryKind::File {
            Some(sig.size)
        } else {
            None
        },
        target,
        target_raw,
        sig,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_cmd(buf: &mut Vec<u8>, cmd: u16, attrs: &[(u16, &[u8])]) {
        let mut payload = Vec::new();
        for (attr_type, data) in attrs {
            payload.extend_from_slice(&attr_type.to_le_bytes());
            payload.extend_from_slice(&(data.len() as u16).to_le_bytes());
            payload.extend_from_slice(data);
        }
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&cmd.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // dummy crc
        buf.extend_from_slice(&payload);
    }

    #[test]
    fn parses_synthetic_v1_stream_and_resolves_orphan_renames() {
        let mut stream = Vec::new();
        stream.extend_from_slice(STREAM_MAGIC);
        stream.extend_from_slice(&1u32.to_le_bytes());

        // New file created via orphan temp name, written, then renamed.
        push_cmd(&mut stream, CMD_MKFILE, &[(ATTR_PATH, b"o257-10-0")]);
        push_cmd(&mut stream, CMD_UPDATE_EXTENT, &[(ATTR_PATH, b"o257-10-0")]);
        push_cmd(
            &mut stream,
            CMD_RENAME,
            &[(ATTR_PATH, b"o257-10-0"), (ATTR_PATH_TO, b"dir/created.txt")],
        );
        // Existing file modified in place.
        push_cmd(&mut stream, CMD_UPDATE_EXTENT, &[(ATTR_PATH, b"dir/modified.txt")]);
        // Metadata-only change.
        push_cmd(&mut stream, CMD_CHMOD, &[(ATTR_PATH, b"dir/meta.txt")]);
        // Existing file renamed.
        push_cmd(
            &mut stream,
            CMD_RENAME,
            &[(ATTR_PATH, b"dir/old.txt"), (ATTR_PATH_TO, b"dir/new.txt")],
        );
        // Deleted file.
        push_cmd(&mut stream, CMD_UNLINK, &[(ATTR_PATH, b"dir/gone.txt")]);
        push_cmd(&mut stream, CMD_END, &[]);

        let cancel = AtomicBool::new(false);
        let summary = parse_send_stream(stream.as_slice(), &cancel, |_, _| {}).expect("parse");

        assert!(summary.content_paths.contains(Path::new("dir/created.txt")));
        assert!(summary.content_paths.contains(Path::new("dir/modified.txt")));
        assert!(summary.content_paths.contains(Path::new("dir/new.txt")));
        assert!(!summary.content_paths.contains(Path::new("o257-10-0")));
        assert!(summary.metadata_paths.contains(Path::new("dir/meta.txt")));
        assert!(summary.deleted_paths.contains(Path::new("dir/old.txt")));
        assert!(summary.deleted_paths.contains(Path::new("dir/gone.txt")));
    }

    #[test]
    fn rejects_invalid_magic() {
        let cancel = AtomicBool::new(false);
        let err = parse_send_stream(&b"not-a-btrfs-stream!!"[..], &cancel, |_, _| {}).unwrap_err();
        assert!(err.contains("magic"));
    }

    #[test]
    fn handles_directory_rename_prefix_and_ephemeral_unlink() {
        let mut stream = Vec::new();
        stream.extend_from_slice(STREAM_MAGIC);
        stream.extend_from_slice(&1u32.to_le_bytes());

        // Create file inside dir_a, then rename dir_a -> dir_b.
        push_cmd(&mut stream, CMD_MKFILE, &[(ATTR_PATH, b"dir_a/child.txt")]);
        push_cmd(
            &mut stream,
            CMD_RENAME,
            &[(ATTR_PATH, b"dir_a"), (ATTR_PATH_TO, b"dir_b")],
        );
        // Create ephemeral file and immediately unlink it -> should NOT appear in deleted_paths.
        push_cmd(&mut stream, CMD_MKFILE, &[(ATTR_PATH, b"ephemeral.tmp")]);
        push_cmd(&mut stream, CMD_UNLINK, &[(ATTR_PATH, b"ephemeral.tmp")]);
        push_cmd(&mut stream, CMD_END, &[]);

        let cancel = AtomicBool::new(false);
        let summary = parse_send_stream(stream.as_slice(), &cancel, |_, _| {}).expect("parse");

        assert!(summary.content_paths.contains(Path::new("dir_b/child.txt")));
        assert!(!summary.content_paths.contains(Path::new("dir_a/child.txt")));
        assert!(summary.deleted_paths.contains(Path::new("dir_a")));
        assert!(!summary.deleted_paths.contains(Path::new("ephemeral.tmp")));
        assert!(!summary.content_paths.contains(Path::new("ephemeral.tmp")));
    }

    #[test]
    fn rejects_truncated_stream_missing_end() {
        let mut stream = Vec::new();
        stream.extend_from_slice(STREAM_MAGIC);
        stream.extend_from_slice(&1u32.to_le_bytes());
        push_cmd(&mut stream, CMD_MKFILE, &[(ATTR_PATH, b"partial.txt")]);
        // Intentionally omit CMD_END.

        let cancel = AtomicBool::new(false);
        let err = parse_send_stream(stream.as_slice(), &cancel, |_, _| {}).unwrap_err();
        assert!(err.contains("END"));
    }

    #[test]
    fn expands_renamed_directory_unchanged_children_and_preserves_literal_orphan_name() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-send-ren-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(left.join("old_dir")).unwrap();
        std::fs::create_dir_all(right.join("new_dir")).unwrap();
        std::fs::write(left.join("old_dir/unchanged.txt"), "same").unwrap();
        std::fs::write(right.join("new_dir/unchanged.txt"), "same").unwrap();
        // A real user file whose name happens to look like an orphan placeholder.
        std::fs::write(left.join("o999-1-0"), "old").unwrap();
        std::fs::write(right.join("o999-1-0"), "new").unwrap();

        let mut stream = Vec::new();
        stream.extend_from_slice(STREAM_MAGIC);
        stream.extend_from_slice(&1u32.to_le_bytes());
        push_cmd(
            &mut stream,
            CMD_RENAME,
            &[(ATTR_PATH, b"old_dir"), (ATTR_PATH_TO, b"new_dir")],
        );
        push_cmd(&mut stream, CMD_UPDATE_EXTENT, &[(ATTR_PATH, b"o999-1-0")]);
        push_cmd(&mut stream, CMD_END, &[]);

        let cancel = AtomicBool::new(false);
        let summary = parse_send_stream(stream.as_slice(), &cancel, |_, _| {}).expect("parse");
        assert!(summary.content_paths.contains(Path::new("o999-1-0")));

        let res = materialize_send_summary(&left, &right, Path::new(""), false, &summary, &cancel)
            .expect("materialize");
        assert!(
            res.rows
                .iter()
                .any(|r| r.rel_path == Path::new("old_dir/unchanged.txt")
                    && r.status == Some(ChangeKind::Deleted)),
            "unchanged child under old_dir must be reported deleted"
        );
        assert!(
            res.rows
                .iter()
                .any(|r| r.rel_path == Path::new("new_dir/unchanged.txt")
                    && r.status == Some(ChangeKind::Added)),
            "unchanged child under new_dir must be reported added"
        );
        assert!(
            res.rows
                .iter()
                .any(|r| r.rel_path == Path::new("o999-1-0")
                    && r.status == Some(ChangeKind::Modified)),
            "literal file named o999-1-0 must not be filtered out"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_corrupt_end_frame_with_nonzero_payload_and_tracks_hardlink() {
        let mut bad = Vec::new();
        bad.extend_from_slice(STREAM_MAGIC);
        bad.extend_from_slice(&1u32.to_le_bytes());
        push_cmd(&mut bad, CMD_END, &[(ATTR_PATH, b"junk")]);
        let cancel = AtomicBool::new(false);
        assert!(parse_send_stream(bad.as_slice(), &cancel, |_, _| {}).is_err());

        let mut hl = Vec::new();
        hl.extend_from_slice(STREAM_MAGIC);
        hl.extend_from_slice(&1u32.to_le_bytes());
        push_cmd(
            &mut hl,
            CMD_LINK,
            &[(ATTR_PATH, b"alias.txt"), (ATTR_PATH_LINK, b"orig.txt")],
        );
        push_cmd(&mut hl, CMD_END, &[]);
        let summary = parse_send_stream(hl.as_slice(), &cancel, |_, _| {}).expect("parse");
        assert!(summary.content_paths.contains(Path::new("alias.txt")));
        assert!(summary.content_paths.contains(Path::new("orig.txt")));
    }
}
