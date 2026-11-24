mod mountinfo;
mod tree;

pub use mountinfo::{
    auto_snapshot_roots, mounted_btrfs_subvolumes, parse_btrfs_mountinfo,
    parse_top_level_btrfs_mounts, protected_snapshot_dirs, top_level_btrfs_mounts,
};
pub use tree::{
    infer_unmounted_group_name, order_groups_as_tree, parse_timestamp_from_name,
};

use crate::fs_safe::{
    btrfs_fsid, btrfs_subvol_meta, fstat_fd, is_btrfs_ro_subvolume,
    is_writable_btrfs_subvolume_root, open_dir_safe,
};
use crate::model::SnapshotInfo;
use crate::temp_snapshot::is_temp_snapshot_name;
use mountinfo::is_snapshot_container_name;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use tree::{classify_snapshot, infer_live_target};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedSubvol {
    /// Subvolume name/path without leading slash (e.g. "@", "@home", "@home-miho-chlivek").
    pub subvol_name: String,
    /// Live mountpoint path (e.g. "/", "/home", "/home/miho/chlivek").
    pub mountpoint: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SubvolumeGroup {
    pub live_target: PathBuf,
    pub subvol_name: Option<String>,
    /// True only when `live_target` was verified to be a writable Btrfs subvolume root
    /// (`ino == 256`, not RO, not a subdirectory bind mount) — required for F5 live overwrite.
    pub verified_live_root: bool,
    /// Chronologically sorted snapshots (oldest -> newest).
    pub snapshots: Vec<SnapshotInfo>,
}

#[derive(Debug, Clone)]
pub struct SubvolumeTreeEntry {
    pub group: SubvolumeGroup,
    /// Pre-formatted virtual tree label (e.g. `/`, `├─ home`, `│  └─ miho/chlivek`).
    pub tree_label: String,
    pub depth: usize,
}


/// Discovers read-only Btrfs snapshots under `base_dir` (up to depth 6),
/// maps them to mounted Btrfs subvolumes, and returns them ordered as a virtual
/// nesting tree (nearest ancestor subvolume -> children with relative labels).
pub fn discover_subvolume_tree(base_dir: &Path) -> Vec<SubvolumeTreeEntry> {
    order_groups_as_tree(discover_subvolume_groups(base_dir))
}


/// Discovers read-only Btrfs snapshots under `base_dir` (up to depth 6),
/// maps them to mounted Btrfs subvolumes, and returns all subvolume groups
/// (snapshotted subvolumes first, followed by other mounted subvolumes).
pub fn discover_subvolume_groups(base_dir: &Path) -> Vec<SubvolumeGroup> {
    let mounted = mounted_btrfs_subvolumes();
    let snapshots = discover_snapshots_with_mounts(base_dir, &mounted);
    build_subvolume_groups(snapshots, &mounted)
}

/// Discovers read-only Btrfs snapshots under `base_dir` (up to depth 6)
/// and sorts them chronologically (oldest -> newest) for the timeline.
pub fn discover_snapshots(base_dir: &Path) -> Vec<SnapshotInfo> {
    let mounted = mounted_btrfs_subvolumes();
    discover_snapshots_with_mounts(base_dir, &mounted)
}

fn discover_snapshots_with_mounts(base_dir: &Path, mounted: &[MountedSubvol]) -> Vec<SnapshotInfo> {
    let mut sorted_mounts = mounted.to_vec();
    // Longest subvol_name first so `@home-miho-chlivek.*` matches before `@home.*` and `@.*`.
    sorted_mounts.sort_by(|a, b| {
        b.subvol_name
            .len()
            .cmp(&a.subvol_name.len())
            .then_with(|| a.subvol_name.cmp(&b.subvol_name))
    });

    // Map live subvolume Btrfs (FSID, UUID) -> mountpoint so snapshots from any tool (btrbk,
    // Snapper `/.snapshots/N/snapshot`, Timeshift, manual) match via kernel `parent_uuid == uuid`
    // scoped to the exact same Btrfs filesystem FSID.
    let mut uuid_to_mount: HashMap<([u8; 16], [u8; 16]), PathBuf> = HashMap::new();
    let mut mount_fsids: HashMap<PathBuf, [u8; 16]> = HashMap::new();
    for m in mounted {
        if let Ok(fd) = open_dir_safe(&m.mountpoint) {
            let fsid = btrfs_fsid(fd.as_raw_fd());
            if let Some(id) = fsid {
                mount_fsids.insert(m.mountpoint.clone(), id);
            }
            if let (Some(id), Some(meta)) = (fsid, btrfs_subvol_meta(fd.as_raw_fd()))
                && meta.uuid != [0u8; 16]
            {
                uuid_to_mount
                    .entry((id, meta.uuid))
                    .or_insert_with(|| m.mountpoint.clone());
            }
        }
    }

    // Also index mounted top-level (`subvol=/`, subvolid=5) mountpoints themselves (for Snapper
    // layouts snapshotting the top-level subvolume directly, e.g. `/mnt/t/snapper-root/.snapshots`)
    // AND unmounted RW Btrfs subvolumes directly under them (e.g. `/mnt/btrfs-top/@unmounted`).
    let top_levels = top_level_btrfs_mounts();
    let mut unmounted_uuid_to_path: HashMap<([u8; 16], [u8; 16]), PathBuf> = HashMap::new();
    for top in &top_levels {
        if let Ok(fd) = open_dir_safe(top)
            && let (Some(id), Some(meta)) =
                (btrfs_fsid(fd.as_raw_fd()), btrfs_subvol_meta(fd.as_raw_fd()))
            && meta.uuid != [0u8; 16]
        {
            uuid_to_mount
                .entry((id, meta.uuid))
                .or_insert_with(|| top.clone());
        }
        let Ok(entries) = fs::read_dir(top) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        dirs.sort();
        for dir in dirs {
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| is_snapshot_container_name(n) || is_temp_snapshot_name(n))
            {
                continue;
            }
            if let Ok(fd) = open_dir_safe(&dir)
                && is_writable_btrfs_subvolume_root(fd.as_raw_fd())
                && let (Some(id), Some(meta)) =
                    (btrfs_fsid(fd.as_raw_fd()), btrfs_subvol_meta(fd.as_raw_fd()))
                && meta.uuid != [0u8; 16]
                && !uuid_to_mount.contains_key(&(id, meta.uuid))
            {
                unmounted_uuid_to_path
                    .entry((id, meta.uuid))
                    .or_insert(dir);
            }
        }
    }

    let search_roots: Vec<PathBuf> = if !base_dir.as_os_str().is_empty() && base_dir.is_dir() {
        vec![base_dir.to_path_buf()]
    } else {
        auto_snapshot_roots()
    };

    let mut found = Vec::new();
    // Deduplicate by (FSID, treeid) so distinct Btrfs filesystems with identical treeids never collide.
    let mut seen_subvols: HashSet<([u8; 16], u64)> = HashSet::new();
    for root in &search_roots {
        walk_snapshots(
            root,
            root,
            0,
            &sorted_mounts,
            &mount_fsids,
            &uuid_to_mount,
            &unmounted_uuid_to_path,
            &mut seen_subvols,
            &mut found,
        );
    }
    found.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.path.cmp(&b.path))
    });
    found
}

/// Combines discovered snapshots with all mounted Btrfs subvolumes.
pub fn build_subvolume_groups(
    snapshots: Vec<SnapshotInfo>,
    mounted: &[MountedSubvol],
) -> Vec<SubvolumeGroup> {
    let mut map: BTreeMap<PathBuf, SubvolumeGroup> = BTreeMap::new();

    for m in mounted {
        let verified = open_dir_safe(&m.mountpoint)
            .map(|fd| is_writable_btrfs_subvolume_root(fd.as_raw_fd()))
            .unwrap_or(false);
        map.entry(m.mountpoint.clone()).or_insert_with(|| SubvolumeGroup {
            live_target: m.mountpoint.clone(),
            subvol_name: Some(m.subvol_name.clone()),
            verified_live_root: verified,
            snapshots: Vec::new(),
        });
    }

    let top_levels: HashSet<PathBuf> = top_level_btrfs_mounts().into_iter().collect();
    for snap in snapshots {
        map.entry(snap.live_target.clone())
            .or_insert_with(|| {
                let is_top_mount = top_levels.contains(&snap.live_target);
                let verified = is_top_mount
                    && open_dir_safe(&snap.live_target)
                        .map(|fd| is_writable_btrfs_subvolume_root(fd.as_raw_fd()))
                        .unwrap_or(false);
                SubvolumeGroup {
                    subvol_name: if is_top_mount {
                        Some("/".to_string())
                    } else {
                        snap.live_target
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    },
                    live_target: snap.live_target.clone(),
                    verified_live_root: verified,
                    snapshots: Vec::new(),
                }
            })
            .snapshots
            .push(snap);
    }

    let mut groups: Vec<SubvolumeGroup> = map
        .into_values()
        .map(|mut g| {
            g.snapshots.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.path.cmp(&b.path))
            });
            g
        })
        .collect();

    // Put subvolumes that have snapshots first, then sort alphabetically by mountpoint.
    groups.sort_by(|a, b| {
        a.snapshots
            .is_empty()
            .cmp(&b.snapshots.is_empty())
            .then_with(|| a.live_target.cmp(&b.live_target))
    });
    groups
}

fn subvol_dedup_key(fsid: Option<[u8; 16]>, treeid: u64) -> Option<([u8; 16], u64)> {
    let fsid = fsid?;
    if treeid == 0 {
        return None;
    }
    Some((fsid, treeid))
}

#[allow(clippy::too_many_arguments)]
fn walk_snapshots(
    base_dir: &Path,
    current: &Path,
    depth: usize,
    sorted_mounts: &[MountedSubvol],
    mount_fsids: &HashMap<PathBuf, [u8; 16]>,
    uuid_to_mount: &HashMap<([u8; 16], [u8; 16]), PathBuf>,
    unmounted_uuid_to_path: &HashMap<([u8; 16], [u8; 16]), PathBuf>,
    seen_subvols: &mut HashSet<([u8; 16], u64)>,
    out: &mut Vec<SnapshotInfo>,
) {
    let Ok(fd) = open_dir_safe(current) else {
        return;
    };
    if current
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_temp_snapshot_name)
    {
        return;
    }
    if is_btrfs_ro_subvolume(fd.as_raw_fd()) {
        let fsid_opt = btrfs_fsid(fd.as_raw_fd());
        let meta = btrfs_subvol_meta(fd.as_raw_fd());
        // Never deduplicate under a synthetic all-zero FSID when FSID lookup failed.
        if let Some(m) = meta
            && let Some(key) = subvol_dedup_key(fsid_opt, m.treeid)
            && !seen_subvols.insert(key)
        {
            return;
        }
        // Match priority:
        // 1) Mounted live subvolume via Btrfs (FSID, parent_uuid) == (FSID, uuid) -> verified_parent=true
        // 2) Unmounted RW subvolume under top-level `subvol=/` via (FSID, parent_uuid) -> verified_parent=true
        // 3) Longest-prefix subvolume name fallback on the same FSID -> verified_parent=false
        // 4) Synthetic unmounted group name so orphan/unmounted snapshots remain browsable -> verified_parent=false
        let verified_match = fsid_opt.and_then(|fsid| {
            meta.filter(|m| m.parent_uuid != [0u8; 16]).and_then(|m| {
                uuid_to_mount
                    .get(&(fsid, m.parent_uuid))
                    .or_else(|| unmounted_uuid_to_path.get(&(fsid, m.parent_uuid)))
                    .cloned()
            })
        });
        let (live_target, verified_parent) = if let Some(target) = verified_match {
            (target, true)
        } else {
            let heuristic = infer_live_target(current, sorted_mounts).and_then(|candidate| {
                if let (Some(fsid), Some(&m_fsid)) = (fsid_opt, mount_fsids.get(&candidate))
                    && m_fsid != fsid
                {
                    return None;
                }
                Some(candidate)
            });
            let target = heuristic
                .unwrap_or_else(|| PathBuf::from(infer_unmounted_group_name(current)));
            (target, false)
        };

        let label = current
            .strip_prefix(base_dir)
            .unwrap_or(current)
            .display()
            .to_string();
        // Creation time priority: 1) Btrfs subvolume otime, 2) name timestamp, 3) dir mtime.
        let created_at = meta
            .and_then(|m| m.otime)
            .or_else(|| parse_timestamp_from_name(current))
            .or_else(|| {
                fstat_fd(fd.as_raw_fd()).ok().and_then(|st| {
                    if st.mtime_sec >= 0 {
                        Some(UNIX_EPOCH + Duration::from_secs(st.mtime_sec as u64))
                    } else {
                        None
                    }
                })
            });
        let kind = classify_snapshot(current);
        out.push(SnapshotInfo {
            path: current.to_path_buf(),
            live_target,
            label,
            created_at,
            kind,
            read_only: true,
            verified_parent,
        });
        // Never descend into a snapshot's stored filesystem.
        return;
    }

    if depth >= 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for dir in dirs {
        walk_snapshots(
            base_dir,
            &dir,
            depth + 1,
            sorted_mounts,
            mount_fsids,
            uuid_to_mount,
            unmounted_uuid_to_path,
            seen_subvols,
            out,
        );
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SnapshotKind;

    #[test]
    fn parses_btrbk_timestamps() {
        let t1 = parse_timestamp_from_name(Path::new("/mnt/@snapshots/@home.20261123T1400"))
            .expect("minute precision");
        let t2 = parse_timestamp_from_name(Path::new("/mnt/@snapshots/@home.20261123T140030"))
            .expect("second precision");
        assert!(t2 > t1);
        assert_eq!(t2.duration_since(t1).unwrap().as_secs(), 30);
        // Must not panic on multi-byte UTF-8 right after '.' or '-' and must keep scanning.
        assert!(parse_timestamp_from_name(Path::new("/mnt/@snapshots/záloha.český_dlouhý")).is_none());
        assert!(
            parse_timestamp_from_name(Path::new("/mnt/@snapshots/záloha.český-20261123T1400"))
                .is_some()
        );
        // btrbk short format (YYYYMMDD) and explicit UTC / ±HHMM offsets.
        assert!(parse_timestamp_from_name(Path::new("/mnt/@snapshots/@home.20261123")).is_some());
        let tz_utc = parse_timestamp_from_name(Path::new("/mnt/@snapshots/@home.20261123T1400Z"))
            .expect("UTC Z");
        let tz_plus =
            parse_timestamp_from_name(Path::new("/mnt/@snapshots/@home.20261123T1600+0200"))
                .expect("+0200");
        assert_eq!(tz_utc, tz_plus);
    }

    #[test]
    fn dedup_key_separates_distinct_fsids_and_skips_unknown_fsid() {
        let mut seen = HashSet::new();
        let fsid_a = [0xAA; 16];
        let fsid_b = [0xBB; 16];
        // Same treeid (257) on two different Btrfs FSIDs must produce distinct keys.
        let k_a = subvol_dedup_key(Some(fsid_a), 257).unwrap();
        let k_b = subvol_dedup_key(Some(fsid_b), 257).unwrap();
        assert_ne!(k_a, k_b);
        assert!(seen.insert(k_a));
        assert!(seen.insert(k_b));
        assert!(!seen.insert(k_a));
        // Unknown FSID (None) must never coalesce into a shared key.
        assert_eq!(subvol_dedup_key(None, 257), None);
    }

    #[test]
    fn classifies_pre_apt_as_event() {
        assert_eq!(
            classify_snapshot(Path::new("/mnt/@snapshots/system/pre-apt/@.20261123T1200")),
            SnapshotKind::Event
        );
        assert_eq!(
            classify_snapshot(Path::new("/mnt/@snapshots/timed/@home.20261123T1200")),
            SnapshotKind::Scheduled
        );
    }

    #[test]
    fn parses_mountinfo_and_matches_longest_subvol_prefix() {
        let sample = "\
36 2 0:31 /@ / rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=256,subvol=/@
58 36 0:31 / /mnt/btrfs-top rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=5,subvol=/
64 36 0:31 /@home /home rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=257,subvol=/@home
152 36 0:31 /@docker /var/lib/docker rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=261,subvol=/@docker
177 64 0:31 /@home-miho-chlivek /home/miho/chlivek rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=313,subvol=/@home-miho-chlivek
";
        let mut mounts = parse_btrfs_mountinfo(sample);
        assert_eq!(mounts.len(), 4);
        assert!(!mounts.iter().any(|m| m.mountpoint == Path::new("/mnt/btrfs-top")));

        mounts.sort_by(|a, b| b.subvol_name.len().cmp(&a.subvol_name.len()));
        assert_eq!(
            infer_live_target(Path::new("/mnt/@snapshots/@home-miho-chlivek.20261123T1200"), &mounts),
            Some(PathBuf::from("/home/miho/chlivek"))
        );
        assert_eq!(
            infer_live_target(Path::new("/mnt/@snapshots/@home.20261123T1200"), &mounts),
            Some(PathBuf::from("/home"))
        );
        assert_eq!(
            infer_live_target(Path::new("/mnt/@snapshots/@docker.20261123T1200"), &mounts),
            Some(PathBuf::from("/var/lib/docker"))
        );
        assert_eq!(
            infer_live_target(Path::new("/mnt/@snapshots/@.20261123T1200"), &mounts),
            Some(PathBuf::from("/"))
        );
    }

    #[test]
    fn mountinfo_rejects_subdirectory_bind_mount() {
        let sample = "\
64 36 0:31 /@home /home rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=257,subvol=/@home
99 36 0:31 /@home/miho/sub /mnt/bind-sub rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=257,subvol=/@home
";
        let mounts = parse_btrfs_mountinfo(sample);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].mountpoint, PathBuf::from("/home"));
    }

    #[test]
    fn builds_virtual_nesting_tree_with_relative_labels() {
        let mk = |path: &str, name: &str| SubvolumeGroup {
            live_target: PathBuf::from(path),
            subvol_name: Some(name.to_string()),
            verified_live_root: true,
            snapshots: Vec::new(),
        };
        let tree = order_groups_as_tree(vec![
            mk("/", "@"),
            mk("/home", "@home"),
            mk("/home/miho/chlivek", "@home-miho-chlivek"),
            mk("/home/miho/Downloads", "@home-miho-downloads"),
            mk("/var/lib/docker", "@docker"),
        ]);
        let labels: Vec<&str> = tree.iter().map(|e| e.tree_label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "/",
                "├─ home",
                "│  ├─ miho/Downloads",
                "│  └─ miho/chlivek",
                "└─ var/lib/docker",
            ]
        );
    }

    #[test]
    fn top_level_mounts_and_unmounted_subvolume_grouping() {
        let sample = "\
36 2 0:31 /@ / rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=256,subvol=/@
58 36 0:31 / /mnt/t/btrfs-top rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=5,subvol=/
59 36 0:31 /@root/sub /mnt/t/bind rw,noatime - btrfs /dev/nvme0n1p2 rw,subvolid=257,subvol=/@root
";
        let tops = parse_top_level_btrfs_mounts(sample);
        assert_eq!(tops, vec![PathBuf::from("/mnt/t/btrfs-top")]);

        assert_eq!(
            infer_unmounted_group_name(Path::new("/mnt/t/btrfs-top/@snapshots/@unmounted.s1")),
            "@unmounted"
        );
        assert_eq!(
            infer_unmounted_group_name(Path::new("/mnt/t/btrfs-top/@snapshots/@unmounted.s2")),
            "@unmounted"
        );
        assert_eq!(
            infer_unmounted_group_name(Path::new("/mnt/snaps/backup-20261123T1200")),
            "backup"
        );
        assert_eq!(
            infer_unmounted_group_name(Path::new("/mnt/snaps/custom-label")),
            "custom-label"
        );

        let mk_snap = |path: &str, live: &str, label: &str| SnapshotInfo {
            path: PathBuf::from(path),
            live_target: PathBuf::from(live),
            label: label.to_string(),
            created_at: None,
            kind: SnapshotKind::Manual,
            read_only: true,
            verified_parent: false,
        };
        let mounted = vec![MountedSubvol {
            subvol_name: "@root".to_string(),
            mountpoint: PathBuf::from("/mnt/t/root"),
        }];
        let snaps = vec![
            mk_snap(
                "/mnt/t/btrfs-top/@snapshots/@unmounted.s1",
                "/mnt/t/btrfs-top/@unmounted",
                "@unmounted.s1",
            ),
            mk_snap(
                "/mnt/t/btrfs-top/@snapshots/@unmounted.s2",
                "/mnt/t/btrfs-top/@unmounted",
                "@unmounted.s2",
            ),
            mk_snap("/snaps/@orphan.s1", "@orphan", "@orphan.s1"),
        ];
        let groups = build_subvolume_groups(snaps, &mounted);
        let unmounted_group = groups
            .iter()
            .find(|g| g.live_target == Path::new("/mnt/t/btrfs-top/@unmounted"))
            .expect("unmounted subvolume group present");
        assert_eq!(unmounted_group.subvol_name.as_deref(), Some("@unmounted"));
        assert_eq!(unmounted_group.snapshots.len(), 2);
        assert!(!unmounted_group.verified_live_root);

        let orphan_group = groups
            .iter()
            .find(|g| g.live_target == Path::new("@orphan"))
            .expect("orphan snapshot group present");
        assert_eq!(orphan_group.subvol_name.as_deref(), Some("@orphan"));
        assert_eq!(orphan_group.snapshots.len(), 1);
        assert!(!orphan_group.verified_live_root);

        // Tree ordering keeps orphan group at root level (depth 0), never nested under `/`.
        let tree = order_groups_as_tree(groups);
        let orphan_entry = tree
            .iter()
            .find(|e| e.group.live_target == Path::new("@orphan"))
            .unwrap();
        assert_eq!(orphan_entry.depth, 0);
        assert_eq!(orphan_entry.tree_label, "@orphan");
    }
}
