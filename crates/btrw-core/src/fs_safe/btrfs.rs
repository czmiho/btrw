use super::fstat_fd;
use std::os::fd::RawFd;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Portable Linux ioctl numbers; `libc::Ioctl` adapts between glibc (`c_ulong`) and musl (`c_int`).
const BTRFS_IOC_SUBVOL_GETFLAGS: libc::Ioctl = 0x8008_9419u32 as libc::Ioctl;
const BTRFS_SUBVOL_RDONLY: u64 = 1 << 1;

// _IOR(0x94, 60, struct btrfs_ioctl_get_subvol_info_args [504 bytes])
const BTRFS_IOC_GET_SUBVOL_INFO: libc::Ioctl = 0x81F8_943Cu32 as libc::Ioctl;
// _IOR(0x94, 31, struct btrfs_ioctl_fs_info_args [1024 bytes])
const BTRFS_IOC_FS_INFO: libc::Ioctl = 0x8400_941Fu32 as libc::Ioctl;
// _IOWR(0x94, 17, struct btrfs_ioctl_search_args [4096 bytes])
const BTRFS_IOC_TREE_SEARCH: libc::Ioctl = 0xD000_9411u32 as libc::Ioctl;
const BTRFS_EXTENT_DATA_KEY: u32 = 108;
const BTRFS_FILE_EXTENT_REG: u8 = 1;

#[repr(C)]
pub(super) struct BtrfsFsInfoArgs {
    max_id: u64,
    num_devices: u64,
    fsid: [u8; 16],
    nodesize: u32,
    sectorsize: u32,
    clone_alignment: u32,
    csum_type: u16,
    csum_size: u16,
    flags: u64,
    generation: u64,
    metadata_uuid: [u8; 16],
    reserved: [u8; 944],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct BtrfsSearchKey {
    tree_id: u64,
    min_objectid: u64,
    max_objectid: u64,
    min_offset: u64,
    max_offset: u64,
    min_transid: u64,
    max_transid: u64,
    min_type: u32,
    max_type: u32,
    nr_items: u32,
    unused: u32,
    unused1: u64,
    unused2: u64,
    unused3: u64,
    unused4: u64,
}

#[repr(C)]
pub(super) struct BtrfsSearchArgs {
    key: BtrfsSearchKey,
    buf: [u8; 4096 - 104],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtrfsExtentSig {
    pub file_offset: u64,
    pub compression: u8,
    pub encryption: u8,
    pub other_encoding: u16,
    pub disk_bytenr: u64,
    pub disk_num_bytes: u64,
    pub extent_offset: u64,
    pub num_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BtrfsTimespec {
    sec: u64,
    nsec: u32,
}

#[repr(C)]
pub(super) struct BtrfsSubvolInfoArgs {
    treeid: u64,
    name: [u8; 256],
    parent_id: u64,
    dirid: u64,
    generation: u64,
    flags: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    received_uuid: [u8; 16],
    ctransid: u64,
    otransid: u64,
    stransid: u64,
    rtransid: u64,
    ctime: BtrfsTimespec,
    otime: BtrfsTimespec,
    stime: BtrfsTimespec,
    rtime: BtrfsTimespec,
    reserved: [u64; 8],
}

/// Checks whether `fd` (file or directory) resides on a read-only Btrfs subvolume.
pub fn is_btrfs_fd_ro(fd: RawFd) -> bool {
    let mut flags: u64 = 0;
    let rc = unsafe { libc::ioctl(fd, BTRFS_IOC_SUBVOL_GETFLAGS, &mut flags as *mut u64) };
    rc >= 0 && (flags & BTRFS_SUBVOL_RDONLY) != 0
}

/// Checks whether `fd` points to a read-only Btrfs subvolume root (`ino == 256` and `BTRFS_SUBVOL_RDONLY`).
pub fn is_btrfs_ro_subvolume(fd: RawFd) -> bool {
    let Ok(sig) = fstat_fd(fd) else {
        return false;
    };
    if !sig.is_dir() || sig.ino != 256 {
        return false;
    }
    is_btrfs_fd_ro(fd)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BtrfsSubvolMeta {
    pub treeid: u64,
    pub uuid: [u8; 16],
    pub parent_uuid: [u8; 16],
    pub otime: Option<SystemTime>,
}

/// Reads Btrfs subvolume metadata (`treeid`, `uuid`, `parent_uuid`, `otime`)
/// via `BTRFS_IOC_GET_SUBVOL_INFO`.
/// Note: Linux 6.17+ can return positive `1` on success with populated `args`, so only `rc < 0` is an error.
pub fn btrfs_subvol_meta(fd: RawFd) -> Option<BtrfsSubvolMeta> {
    let mut args = std::mem::MaybeUninit::<BtrfsSubvolInfoArgs>::zeroed();
    let rc = unsafe { libc::ioctl(fd, BTRFS_IOC_GET_SUBVOL_INFO, args.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    let info = unsafe { args.assume_init() };
    let otime = if info.otime.sec == 0 {
        None
    } else {
        Some(UNIX_EPOCH + Duration::new(info.otime.sec, info.otime.nsec.min(999_999_999)))
    };
    Some(BtrfsSubvolMeta {
        treeid: info.treeid,
        uuid: info.uuid,
        parent_uuid: info.parent_uuid,
        otime,
    })
}

/// Reads the Btrfs subvolume creation time (`otime`) via `BTRFS_IOC_GET_SUBVOL_INFO`.
pub fn btrfs_subvol_otime(fd: RawFd) -> Option<SystemTime> {
    btrfs_subvol_meta(fd).and_then(|m| m.otime)
}

/// Returns the 16-byte Btrfs filesystem UUID (`FSID`) via `BTRFS_IOC_FS_INFO`,
/// or `None` if `fd` is not on a Btrfs filesystem.
pub fn btrfs_fsid(fd: RawFd) -> Option<[u8; 16]> {
    let mut args = std::mem::MaybeUninit::<BtrfsFsInfoArgs>::zeroed();
    let rc = unsafe { libc::ioctl(fd, BTRFS_IOC_FS_INFO, args.as_mut_ptr()) };
    if rc < 0 {
        return None;
    }
    let info = unsafe { args.assume_init() };
    Some(info.fsid)
}

/// Checks whether `fd` is a writable Btrfs subvolume root (`ino == 256` and NOT `BTRFS_SUBVOL_RDONLY`).
pub fn is_writable_btrfs_subvolume_root(fd: RawFd) -> bool {
    let Ok(sig) = fstat_fd(fd) else {
        return false;
    };
    if !sig.is_dir() || sig.ino != 256 {
        return false;
    }
    let mut flags: u64 = 0;
    let rc = unsafe { libc::ioctl(fd, BTRFS_IOC_SUBVOL_GETFLAGS, &mut flags as *mut u64) };
    if rc < 0 || (flags & BTRFS_SUBVOL_RDONLY) != 0 {
        return false;
    }
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::fstatvfs(fd, vfs.as_mut_ptr()) } != 0 {
        return false;
    }
    let vfs = unsafe { vfs.assume_init() };
    (vfs.f_flag & libc::ST_RDONLY) == 0
}

/// Reads exact Btrfs `BTRFS_EXTENT_DATA_KEY` metadata for `ino` via `BTRFS_IOC_TREE_SEARCH`.
/// Unlike `FIEMAP`, this includes `extent_offset` inside compressed extents (`zstd`/`lzo`/`zlib`),
/// giving 100% exact equality checks on compressed files with zero data reads from disk.
/// Returns `None` if not root/Btrfs, if any extent is inline/prealloc, or if truncated.
pub fn btrfs_extent_data_map(fd: RawFd, ino: u64) -> Option<Vec<BtrfsExtentSig>> {
    let mut out = Vec::new();
    let mut min_offset = 0u64;

    for _ in 0..256 {
        let mut args = BtrfsSearchArgs {
            key: BtrfsSearchKey {
                tree_id: 0, // subvolume tree of `fd`
                min_objectid: ino,
                max_objectid: ino,
                min_offset,
                max_offset: u64::MAX,
                min_transid: 0,
                max_transid: u64::MAX,
                min_type: BTRFS_EXTENT_DATA_KEY,
                max_type: BTRFS_EXTENT_DATA_KEY,
                nr_items: u32::MAX,
                ..Default::default()
            },
            buf: [0u8; 4096 - 104],
        };
        let rc = unsafe { libc::ioctl(fd, BTRFS_IOC_TREE_SEARCH, &mut args as *mut BtrfsSearchArgs) };
        if rc < 0 {
            return None;
        }
        let count = args.key.nr_items as usize;
        if count == 0 {
            return if out.is_empty() { None } else { Some(out) };
        }
        let mut pos = 0usize;
        let mut last_off = min_offset;
        for _ in 0..count {
            if pos + 32 > args.buf.len() {
                return None;
            }
            let file_offset = u64::from_ne_bytes(args.buf[pos + 16..pos + 24].try_into().ok()?);
            let item_type = u32::from_ne_bytes(args.buf[pos + 24..pos + 28].try_into().ok()?);
            let item_len = u32::from_ne_bytes(args.buf[pos + 28..pos + 32].try_into().ok()?) as usize;
            pos += 32;
            if pos + item_len > args.buf.len() {
                return None;
            }
            let item = &args.buf[pos..pos + item_len];
            pos += item_len;

            if item_type != BTRFS_EXTENT_DATA_KEY {
                continue;
            }
            // Regular extent item is 53 bytes (21-byte header + 4*u64); reject inline/prealloc.
            if item_len < 53 || item[20] != BTRFS_FILE_EXTENT_REG {
                return None;
            }
            let compression = item[16];
            let encryption = item[17];
            let other_encoding = u16::from_le_bytes([item[18], item[19]]);
            let disk_bytenr = u64::from_le_bytes(item[21..29].try_into().ok()?);
            let disk_num_bytes = u64::from_le_bytes(item[29..37].try_into().ok()?);
            let extent_offset = u64::from_le_bytes(item[37..45].try_into().ok()?);
            let num_bytes = u64::from_le_bytes(item[45..53].try_into().ok()?);

            out.push(BtrfsExtentSig {
                file_offset,
                compression,
                encryption,
                other_encoding,
                disk_bytenr,
                disk_num_bytes,
                extent_offset,
                num_bytes,
            });
            last_off = file_offset;
        }
        if last_off == u64::MAX {
            return Some(out);
        }
        min_offset = last_off + 1;
    }
    // Hit iteration cap without exhausting all items -> refuse partial map.
    None
}
