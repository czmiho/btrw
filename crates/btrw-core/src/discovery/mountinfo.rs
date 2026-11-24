use super::MountedSubvol;
use crate::fs_safe::decode_mount_escapes;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Parses `/proc/self/mountinfo` and returns all mounted live Btrfs subvolumes
/// (excluding top-level `subvol=/` and pure snapshot container mounts).
pub fn mounted_btrfs_subvolumes() -> Vec<MountedSubvol> {
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    parse_btrfs_mountinfo(&content)
}

/// Auto-discovers candidate snapshot root directories from `/proc/self/mountinfo`:
/// - `@snapshots`, `.snapshots`, `snapshots` under any top-level `subvol=/` mount
/// - any directly mounted snapshot container (e.g. `/.snapshots`, `/home/.snapshots`)
/// - `<mount>/.snapshots` under any mounted live Btrfs subvolume (Snapper layout)
pub fn auto_snapshot_roots() -> Vec<PathBuf> {
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut push_if_dir = |p: PathBuf| {
        if p.is_dir() && seen.insert(p.clone()) {
            roots.push(p);
        }
    };

    for line in content.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let pre_fields: Vec<&str> = pre.split_whitespace().collect();
        let post_fields: Vec<&str> = post.split_whitespace().collect();
        if pre_fields.len() < 5 || post_fields.len() < 3 || post_fields[0] != "btrfs" {
            continue;
        }
        let mountpoint = PathBuf::from(decode_mount_escapes(pre_fields[4]));
        let subvol_opt = post_fields[2]
            .split(',')
            .find_map(|opt| opt.strip_prefix("subvol="))
            .map(decode_mount_escapes)
            .unwrap_or_else(|| decode_mount_escapes(pre_fields[3]));
        let trimmed = subvol_opt.trim_start_matches('/');

        if trimmed.is_empty() {
            // Top-level subvolid=5 mount (e.g. /mnt/btrfs-top): check standard snapshot containers.
            for sub in ["@snapshots", ".snapshots", "snapshots"] {
                push_if_dir(mountpoint.join(sub));
            }
        } else if is_snapshot_container_name(trimmed)
            || mountpoint
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_snapshot_container_name)
        {
            push_if_dir(mountpoint);
        } else {
            push_if_dir(mountpoint.join(".snapshots"));
        }
    }
    roots
}

/// Returns all snapshot root directories that must be protected from writes:
/// `configured_dir` (if non-empty) plus all auto-discovered snapshot containers.
pub fn protected_snapshot_dirs(configured_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    if !configured_dir.as_os_str().is_empty() {
        let p = configured_dir.to_path_buf();
        if seen.insert(p.clone()) {
            dirs.push(p);
        }
    }
    for p in auto_snapshot_roots() {
        if seen.insert(p.clone()) {
            dirs.push(p);
        }
    }
    dirs
}

/// Parses `/proc/self/mountinfo` and returns all mounted top-level (`subvol=/`) Btrfs mountpoints.
pub fn top_level_btrfs_mounts() -> Vec<PathBuf> {
    let Ok(content) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    parse_top_level_btrfs_mounts(&content)
}

pub fn parse_top_level_btrfs_mounts(content: &str) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    let mut seen = HashSet::new();
    for line in content.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let pre_fields: Vec<&str> = pre.split_whitespace().collect();
        let post_fields: Vec<&str> = post.split_whitespace().collect();
        if pre_fields.len() < 5 || post_fields.len() < 3 || post_fields[0] != "btrfs" {
            continue;
        }
        let mountpoint = PathBuf::from(decode_mount_escapes(pre_fields[4]));
        let subvol_opt = post_fields[2]
            .split(',')
            .find_map(|opt| opt.strip_prefix("subvol="))
            .map(decode_mount_escapes)
            .unwrap_or_else(|| decode_mount_escapes(pre_fields[3]));
        let mount_root = decode_mount_escapes(pre_fields[3]);
        if mount_root != subvol_opt {
            continue;
        }
        if subvol_opt.trim_start_matches('/').is_empty() && seen.insert(mountpoint.clone()) {
            mounts.push(mountpoint);
        }
    }
    mounts
}

pub(super) fn is_snapshot_container_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".snapshots"
        || lower == "@snapshots"
        || lower == "snapshots"
        || lower.ends_with("/.snapshots")
        || lower.ends_with("/@snapshots")
}

pub fn parse_btrfs_mountinfo(content: &str) -> Vec<MountedSubvol> {
    let mut seen: BTreeMap<PathBuf, MountedSubvol> = BTreeMap::new();
    for line in content.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let pre_fields: Vec<&str> = pre.split_whitespace().collect();
        let post_fields: Vec<&str> = post.split_whitespace().collect();
        if pre_fields.len() < 5 || post_fields.len() < 3 {
            continue;
        }
        let fs_type = post_fields[0];
        if fs_type != "btrfs" {
            continue;
        }
        let mountpoint = PathBuf::from(decode_mount_escapes(pre_fields[4]));
        let super_opts = post_fields[2];

        // Extract `subvol=...` option (preferred) or root field `pre_fields[3]`.
        let subvol_opt = super_opts
            .split(',')
            .find_map(|opt| opt.strip_prefix("subvol="))
            .map(decode_mount_escapes)
            .unwrap_or_else(|| decode_mount_escapes(pre_fields[3]));

        // Reject bind mounts of subdirectories (root field `pre_fields[3]` differs from `subvol=`).
        let mount_root = decode_mount_escapes(pre_fields[3]);
        if mount_root != subvol_opt {
            continue;
        }

        let trimmed = subvol_opt.trim_start_matches('/').to_string();
        // Skip the top-level Btrfs root (`subvol=/`, subvolid=5) and pure snapshot container mounts.
        if trimmed.is_empty()
            || is_snapshot_container_name(&trimmed)
            || mountpoint
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_snapshot_container_name)
        {
            continue;
        }

        seen.entry(mountpoint.clone()).or_insert(MountedSubvol {
            subvol_name: trimmed,
            mountpoint,
        });
    }
    seen.into_values().collect()
}

