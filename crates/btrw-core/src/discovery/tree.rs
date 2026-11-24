use super::{MountedSubvol, SubvolumeGroup, SubvolumeTreeEntry};
use crate::model::SnapshotKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Orders subvolume groups into a virtual nesting tree based on mountpoint ancestry.
pub fn order_groups_as_tree(groups: Vec<SubvolumeGroup>) -> Vec<SubvolumeTreeEntry> {
    let n = groups.len();
    if n == 0 {
        return Vec::new();
    }

    // Find nearest strict ancestor subvolume for each group.
    let mut parent_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let mut best: Option<(usize, usize)> = None; // (parent_idx, path_len)
        for j in 0..n {
            if i == j {
                continue;
            }
            let cand = &groups[j].live_target;
            let child = &groups[i].live_target;
            if child != cand && child.starts_with(cand) {
                let len = cand.as_os_str().len();
                if best.is_none_or(|(_, best_len)| len > best_len) {
                    best = Some((j, len));
                }
            }
        }
        parent_of[i] = best.map(|(idx, _)| idx);
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for (idx, p) in parent_of.iter().enumerate() {
        match p {
            Some(parent_idx) => children[*parent_idx].push(idx),
            None => roots.push(idx),
        }
    }

    // Check whether a node or any descendant has snapshots (so snapshotted branches sort first).
    fn subtree_has_snaps(idx: usize, groups: &[SubvolumeGroup], children: &[Vec<usize>]) -> bool {
        !groups[idx].snapshots.is_empty()
            || children[idx]
                .iter()
                .any(|&c| subtree_has_snaps(c, groups, children))
    }

    let sort_indices = |list: &mut Vec<usize>, children: &[Vec<usize>]| {
        list.sort_by(|&a, &b| {
            let a_has = subtree_has_snaps(a, &groups, children);
            let b_has = subtree_has_snaps(b, &groups, children);
            b_has
                .cmp(&a_has)
                .then_with(|| groups[a].live_target.cmp(&groups[b].live_target))
        });
    };

    let children_snapshot = children.clone();
    sort_indices(&mut roots, &children_snapshot);
    for ch in &mut children {
        sort_indices(ch, &children_snapshot);
    }

    let mut out = Vec::with_capacity(n);

    #[allow(clippy::too_many_arguments)]
    fn emit(
        idx: usize,
        parent_target: Option<&Path>,
        prefix: &str,
        is_last: bool,
        depth: usize,
        groups: &[SubvolumeGroup],
        children: &[Vec<usize>],
        out: &mut Vec<SubvolumeTreeEntry>,
    ) {
        let group = groups[idx].clone();
        let rel_name = match parent_target {
            Some(parent) => group
                .live_target
                .strip_prefix(parent)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| group.live_target.display().to_string()),
            None => group.live_target.display().to_string(),
        };

        let tree_label = if depth == 0 {
            rel_name
        } else {
            let branch = if is_last { "└─ " } else { "├─ " };
            format!("{prefix}{branch}{rel_name}")
        };

        let target_clone = group.live_target.clone();
        out.push(SubvolumeTreeEntry {
            group,
            tree_label,
            depth,
        });

        let child_prefix = if depth == 0 {
            String::new()
        } else if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}│  ")
        };

        let ch = &children[idx];
        for (pos, &child_idx) in ch.iter().enumerate() {
            let child_is_last = pos + 1 == ch.len();
            emit(
                child_idx,
                Some(&target_clone),
                &child_prefix,
                child_is_last,
                depth + 1,
                groups,
                children,
                out,
            );
        }
    }

    for (pos, &root_idx) in roots.iter().enumerate() {
        emit(
            root_idx,
            None,
            "",
            pos + 1 == roots.len(),
            0,
            &groups,
            &children,
            &mut out,
        );
    }

    out
}


/// Derives a clean group name for snapshots of unmounted/orphan subvolumes when no
/// live mountpoint or top-level RW subvolume matched (e.g. `@unmounted.s1` -> `@unmounted`).
pub fn infer_unmounted_group_name(path: &Path) -> String {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy()) else {
        return "unmounted".to_string();
    };
    if let Some((prefix, _)) = name.split_once('.')
        && !prefix.is_empty()
    {
        return prefix.to_string();
    }
    for (idx, ch) in name.char_indices() {
        if ch == '-' && idx > 0 {
            let rest = &name[idx + 1..];
            if rest.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                return name[..idx].to_string();
            }
        }
    }
    name.into_owned()
}

pub(super) fn infer_live_target(path: &Path, sorted_mounts: &[MountedSubvol]) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();

    // 1. Longest-prefix match against all mounted Btrfs subvolumes.
    for m in sorted_mounts {
        if name == m.subvol_name
            || name
                .strip_prefix(&m.subvol_name)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('-') || rest.starts_with('_'))
        {
            return Some(m.mountpoint.clone());
        }
    }

    // 2. Fallback conventions when mountinfo isn't available or snapshot lives under `system/`.
    let full = path.to_string_lossy();
    if name.starts_with("@home.") {
        Some(PathBuf::from("/home"))
    } else if name.starts_with("@.") || full.contains("/system/") {
        Some(PathBuf::from("/"))
    } else {
        None
    }
}

pub(super) fn classify_snapshot(path: &Path) -> SnapshotKind {
    let full = path.to_string_lossy().to_ascii_lowercase();
    if full.contains("pre-apt")
        || full.contains("post-apt")
        || full.contains("pre-")
        || full.contains("post-")
    {
        SnapshotKind::Event
    } else if parse_timestamp_from_name(path).is_some() {
        SnapshotKind::Scheduled
    } else {
        SnapshotKind::Manual
    }
}

/// Parses btrbk-style timestamps (`YYYYMMDDTHHMM`, `YYYYMMDDTHHMMSS`, optionally with
/// `Z` or `±HHMM` timezone offset) from basename. Unqualified timestamps are interpreted
/// in local time (matching `btrbk`'s default `timestamp_format`).
/// Safe against multi-byte UTF-8 characters anywhere in the filename (validates ASCII before slicing).
pub fn parse_timestamp_from_name(path: &Path) -> Option<SystemTime> {
    let name = path.file_name()?.to_str()?;
    for (idx, ch) in name.char_indices() {
        if ch != '.' && ch != '-' {
            continue;
        }
        let rest = &name[idx + 1..];
        let ts = rest.split('_').next().unwrap_or(rest);
        if !ts.is_ascii() {
            continue;
        }
        // Support btrbk `short` format (`YYYYMMDD`, local midnight).
        if ts.len() == 8 && ts.bytes().all(|b| b.is_ascii_digit()) {
            if let (Ok(y), Ok(m), Ok(d)) = (
                ts[0..4].parse::<i32>(),
                ts[4..6].parse::<u32>(),
                ts[6..8].parse::<u32>(),
            ) && let Some(secs) = ymd_hms_to_unix_local(y, m, d, 0, 0, 0)
            {
                return Some(UNIX_EPOCH + Duration::from_secs(secs));
            }
            continue;
        }
        if ts.len() < 13 || ts.as_bytes()[8] != b'T' {
            continue;
        }
        let (Ok(y), Ok(m), Ok(d), Ok(hh), Ok(mm)) = (
            ts[0..4].parse::<i32>(),
            ts[4..6].parse::<u32>(),
            ts[6..8].parse::<u32>(),
            ts[9..11].parse::<u32>(),
            ts[11..13].parse::<u32>(),
        ) else {
            continue;
        };
        let mut tail = &ts[13..];
        let ss: u32 = if tail.len() >= 2 && tail.as_bytes()[0].is_ascii_digit() && tail.as_bytes()[1].is_ascii_digit() {
            let Ok(v) = tail[0..2].parse::<u32>() else {
                continue;
            };
            tail = &tail[2..];
            v
        } else {
            0
        };
        let unix_secs = if tail.is_empty() {
            ymd_hms_to_unix_local(y, m, d, hh, mm, ss)
        } else if tail == "Z" {
            ymd_hms_to_unix_utc(y, m, d, hh, mm, ss)
        } else if tail.len() == 5 && (tail.starts_with('+') || tail.starts_with('-')) {
            let sign: i64 = if tail.starts_with('+') { 1 } else { -1 };
            let (Ok(oh), Ok(om)) = (tail[1..3].parse::<i64>(), tail[3..5].parse::<i64>()) else {
                continue;
            };
            if oh > 23 || om > 59 {
                continue;
            }
            let offset_secs = sign * (oh * 3600 + om * 60);
            ymd_hms_to_unix_utc(y, m, d, hh, mm, ss)
                .and_then(|u| u.checked_add_signed(-offset_secs))
        } else {
            continue;
        };
        if let Some(secs) = unix_secs {
            return Some(UNIX_EPOCH + Duration::from_secs(secs));
        }
    }
    None
}

fn ymd_hms_to_unix_local(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 59 {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = m as i32;
    tm.tm_sec = s as i32;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    if t < 0 {
        ymd_hms_to_unix_utc(year, month, day, h, m, s)
    } else {
        Some(t as u64)
    }
}

fn ymd_hms_to_unix_utc(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 59 {
        return None;
    }
    let y = year - i32::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146_097 + doe as i64 - 719_468;
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + h as u64 * 3600 + m as u64 * 60 + s as u64)
}

