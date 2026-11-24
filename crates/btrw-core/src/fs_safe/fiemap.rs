use super::btrfs::{btrfs_extent_data_map, btrfs_fsid, is_btrfs_fd_ro};
use super::{StatSig, open_file_safe};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const FS_IOC_FIEMAP: libc::Ioctl = 0xC020_660Bu32 as libc::Ioctl;
const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;

const FIEMAP_EXTENT_UNKNOWN: u32 = 0x0000_0002;
const FIEMAP_EXTENT_DELALLOC: u32 = 0x0000_0004;
const FIEMAP_EXTENT_ENCODED: u32 = 0x0000_0008;
const FIEMAP_EXTENT_DATA_INLINE: u32 = 0x0000_0200;
const FIEMAP_EXTENT_UNWRITTEN: u32 = 0x0000_0800;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

#[repr(C)]
struct FiemapBuffer {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
    fm_extents: [FiemapExtent; 64],
}


#[derive(Debug, PartialEq, Eq)]
enum FiemapResult {
    Complete(Vec<(u64, u64, u64)>),
    HasEncoded,
    Unavailable,
}

/// Queries physical extent layout via `FS_IOC_FIEMAP` (without `FIEMAP_FLAG_SYNC` to stay
/// strictly read-only). Only returns `Complete` when `FIEMAP_EXTENT_LAST` is observed and
/// no extent is `ENCODED` (compressed), `INLINE`, `UNKNOWN`, `DELALLOC`, or `UNWRITTEN`.
fn fiemap_extents(fd: RawFd) -> FiemapResult {
    let mut extents = Vec::new();
    let mut start: u64 = 0;
    let mut saw_last = false;
    let mut has_encoded = false;

    for _ in 0..256 {
        let mut buf = FiemapBuffer {
            fm_start: start,
            fm_length: u64::MAX - start,
            fm_flags: 0,
            fm_mapped_extents: 0,
            fm_extent_count: 64,
            fm_reserved: 0,
            fm_extents: [FiemapExtent::default(); 64],
        };
        let rc = unsafe { libc::ioctl(fd, FS_IOC_FIEMAP, &mut buf as *mut FiemapBuffer) };
        if rc < 0 {
            return FiemapResult::Unavailable;
        }
        let count = buf.fm_mapped_extents as usize;
        if count == 0 {
            break;
        }
        for ext in &buf.fm_extents[..count.min(64)] {
            // Reject any extent whose on-disk mapping is incomplete or unflushed (e.g. DELALLOC
            // in RAM on a live file) across the ENTIRE file before accepting HasEncoded or Complete.
            if (ext.fe_flags
                & (FIEMAP_EXTENT_DATA_INLINE
                    | FIEMAP_EXTENT_UNKNOWN
                    | FIEMAP_EXTENT_DELALLOC
                    | FIEMAP_EXTENT_UNWRITTEN))
                != 0
            {
                return FiemapResult::Unavailable;
            }
            if (ext.fe_flags & FIEMAP_EXTENT_ENCODED) != 0 {
                has_encoded = true;
            } else if !has_encoded {
                extents.push((ext.fe_logical, ext.fe_physical, ext.fe_length));
            }
            start = ext.fe_logical.saturating_add(ext.fe_length);
            if (ext.fe_flags & FIEMAP_EXTENT_LAST) != 0 {
                saw_last = true;
            }
        }
        if saw_last {
            break;
        }
    }
    if !saw_last {
        return FiemapResult::Unavailable;
    }
    if has_encoded {
        FiemapResult::HasEncoded
    } else if !extents.is_empty() {
        FiemapResult::Complete(extents)
    } else {
        FiemapResult::Unavailable
    }
}

/// Compares two regular files without relying on error-prone `mtime` heuristics:
/// 1. `st_size` mismatch -> changed immediately.
/// 2. Same `(st_dev, st_ino)` -> identical.
/// 3. On the same Btrfs `FSID`:
///    - Complete unencoded `FIEMAP` match -> guaranteed identical with 0 data reads.
///    - Encoded (compressed `zstd`/`lzo`/`zlib`) extents with zero `DELALLOC`/`UNWRITTEN` ->
///      exact `BTRFS_IOC_TREE_SEARCH` `EXTENT_DATA` comparison (0 data reads).
/// 4. 1 KiB head+tail SHA-256 sample -> fast mismatch reject (full coverage for files <= 2 KiB).
/// 5. Full streaming SHA-256 -> exact fallback.
pub fn files_content_equal(
    left_root: &Path,
    right_root: &Path,
    rel: &Path,
    left_sig: &StatSig,
    right_sig: &StatSig,
) -> io::Result<bool> {
    let cancel = std::sync::atomic::AtomicBool::new(false);
    files_content_equal_cancellable(left_root, right_root, rel, left_sig, right_sig, &cancel)
}

pub fn files_content_equal_cancellable(
    left_root: &Path,
    right_root: &Path,
    rel: &Path,
    left_sig: &StatSig,
    right_sig: &StatSig,
    cancel: &std::sync::atomic::AtomicBool,
) -> io::Result<bool> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    if left_sig.size != right_sig.size {
        return Ok(false);
    }
    if left_sig.size == 0 {
        return Ok(true);
    }
    if left_sig.dev == right_sig.dev && left_sig.ino == right_sig.ino {
        return Ok(true);
    }

    let (mut lf, l_now) = open_file_safe(left_root, rel)?;
    let (mut rf, r_now) = open_file_safe(right_root, rel)?;
    if l_now.dev != left_sig.dev
        || l_now.ino != left_sig.ino
        || r_now.dev != right_sig.dev
        || r_now.ino != right_sig.ino
    {
        return Err(io::Error::other(
            "file replaced concurrently during comparison",
        ));
    }
    if l_now.size != r_now.size {
        return Ok(false);
    }
    if l_now.size == 0 {
        return Ok(true);
    }

    // Extent comparison is only valid when both files live on the exact same Btrfs FSID.
    // On a writable LIVE subvolume, an unflushed in-RAM overwrite of a compressed extent
    // may still report the old on-disk extent in FIEMAP/TREE_SEARCH until `sync`; however,
    // VFS immediately updates `mtime`/`ctime` in RAM. Therefore only take the zero-read
    // extent shortcut when BOTH sides are RO snapshots, OR when `mtime`+`ctime` match exactly
    // and are older than 2 seconds.
    let same_btrfs_fsid = match (btrfs_fsid(lf.as_raw_fd()), btrfs_fsid(rf.as_raw_fd())) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if same_btrfs_fsid {
        let both_ro = is_btrfs_fd_ro(lf.as_raw_fd()) && is_btrfs_fd_ro(rf.as_raw_fd());
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX);
        let live_clean_timestamps = l_now.mtime_sec == r_now.mtime_sec
            && l_now.mtime_nsec == r_now.mtime_nsec
            && l_now.ctime_sec == r_now.ctime_sec
            && l_now.ctime_nsec == r_now.ctime_nsec
            && l_now.ctime_sec <= now_secs.saturating_sub(2);
        if both_ro || live_clean_timestamps {
            match (fiemap_extents(lf.as_raw_fd()), fiemap_extents(rf.as_raw_fd())) {
                (FiemapResult::Complete(le), FiemapResult::Complete(re)) if le == re => {
                    return Ok(true);
                }
                (FiemapResult::HasEncoded, FiemapResult::HasEncoded) => {
                    if let (Some(le), Some(re)) = (
                        btrfs_extent_data_map(lf.as_raw_fd(), l_now.ino),
                        btrfs_extent_data_map(rf.as_raw_fd(), r_now.ino),
                    ) && le == re
                    {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
    }

    // Edge sample: first 1 KiB + last 1 KiB.
    if hash_sample(&mut lf, l_now.size)? != hash_sample(&mut rf, r_now.size)? {
        return Ok(false);
    }
    if l_now.size <= 2048 {
        return Ok(true);
    }

    // Full streaming SHA-256 fallback (cancellable).
    Ok(hash_full(&mut lf, cancel)? == hash_full(&mut rf, cancel)?)
}

fn hash_sample(file: &mut File, size: u64) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024];
    file.seek(SeekFrom::Start(0))?;
    let n = read_up_to(file, &mut buf)?;
    hasher.update(&buf[..n]);
    if size > 1024 {
        let tail_start = size.saturating_sub(1024).max(1024);
        file.seek(SeekFrom::Start(tail_start))?;
        let n2 = read_up_to(file, &mut buf)?;
        hasher.update(&buf[..n2]);
    }
    Ok(hasher.finalize().into())
}

fn hash_full(file: &mut File, cancel: &std::sync::atomic::AtomicBool) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    file.seek(SeekFrom::Start(0))?;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = file.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

/// Reads a regular file safely up to `max_bytes`.
pub fn read_file_limited(root: &Path, rel: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let (mut file, sig) = open_file_safe(root, rel)?;
    if sig.size > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("file exceeds limit ({} > {})", sig.size, max_bytes),
        ));
    }
    let mut out = Vec::with_capacity(sig.size as usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if (out.len() + n) as u64 > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "file grew beyond limit while reading",
            ));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}
