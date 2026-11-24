mod dir;
mod text_diff;
#[cfg(test)]
mod tests;

pub use dir::{
    DirStats, compare_directory, compare_directory_cancellable, scan_directory_stats,
    scan_directory_stats_with_mounts, scan_flat_changes, scan_flat_changes_with_mounts,
};
pub use text_diff::{
    DiffLine, DiffLineKind, FileDiffOutcome, PatchExportSummary, PatchFileErr, SideBySideRow,
    SideRowKind, diff_file, diff_file_side_by_side, escape_control_chars, export_patch,
    export_patch_cancellable,
};

use crate::config::SortOrder;
use crate::fs_safe::StatSig;
use crate::model::{ChangeKind, EntryKind};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DirEntryInfo {
    pub name: String,
    pub rel_path: PathBuf,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub target: Option<String>,
    pub target_raw: Option<OsString>,
    pub sig: StatSig,
}

fn metadata_differs(l: &StatSig, r: &StatSig) -> bool {
    (l.mode & 0o7777) != (r.mode & 0o7777)
        || l.uid != r.uid
        || l.gid != r.gid
        || l.mtime_sec != r.mtime_sec
        || l.mtime_nsec != r.mtime_nsec
}

#[derive(Debug, Clone)]
pub struct PaneRow {
    pub name: String,
    pub rel_path: PathBuf,
    pub left: Option<DirEntryInfo>,
    pub right: Option<DirEntryInfo>,
    pub status: Option<ChangeKind>,
    pub delta_bytes: Option<i128>,
    pub error: Option<String>,
}

impl PaneRow {
    pub fn is_dir(&self) -> bool {
        self.left
            .as_ref()
            .map(|e| e.kind == EntryKind::Directory)
            .unwrap_or(false)
            || self
                .right
                .as_ref()
                .map(|e| e.kind == EntryKind::Directory)
                .unwrap_or(false)
    }
}

#[derive(Debug, Default)]
pub struct DirCompareResult {
    pub rows: Vec<PaneRow>,
    pub excluded: Vec<String>,
    pub errors: Vec<String>,
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    for (idx, unit) in UNITS.iter().enumerate() {
        if value < 1024.0 || idx == UNITS.len() - 1 {
            return if idx == 0 {
                format!("{bytes} B")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    format!("{bytes} B")
}

pub fn human_delta(delta: i128) -> String {
    let abs = delta.unsigned_abs() as u64;
    if delta > 0 {
        format!("+{}", human_size(abs))
    } else if delta < 0 {
        format!("-{}", human_size(abs))
    } else {
        "0 B".to_string()
    }
}

pub fn format_octal_mode(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

pub fn format_mode(mode: u32) -> String {
    let ft = match mode & libc::S_IFMT {
        libc::S_IFDIR => 'd',
        libc::S_IFLNK => 'l',
        libc::S_IFCHR => 'c',
        libc::S_IFBLK => 'b',
        libc::S_IFIFO => 'p',
        libc::S_IFSOCK => 's',
        _ => '-',
    };
    let bit = |mask: u32, ch: char| if (mode & mask) != 0 { ch } else { '-' };
    format!(
        "{ft}{}{}{}{}{}{}{}{}{}",
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        bit(0o100, 'x'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        bit(0o010, 'x'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        bit(0o001, 'x'),
    )
}

/// Sorts pane rows keeping directories at the top (MC style).
pub fn sort_pane_rows(rows: &mut [PaneRow], order: SortOrder) {
    rows.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| match order {
                SortOrder::NameAsc => a
                    .name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.name.cmp(&b.name)),
                SortOrder::NameDesc => b
                    .name
                    .to_lowercase()
                    .cmp(&a.name.to_lowercase())
                    .then_with(|| b.name.cmp(&a.name)),
                SortOrder::SizeDesc => row_size(b)
                    .cmp(&row_size(a))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortOrder::SizeAsc => row_size(a)
                    .cmp(&row_size(b))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortOrder::DeltaDesc => b
                    .delta_bytes
                    .unwrap_or(i128::MIN)
                    .cmp(&a.delta_bytes.unwrap_or(i128::MIN))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortOrder::DeltaAsc => a
                    .delta_bytes
                    .unwrap_or(i128::MAX)
                    .cmp(&b.delta_bytes.unwrap_or(i128::MAX))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortOrder::MtimeDesc => row_mtime(b)
                    .cmp(&row_mtime(a))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortOrder::MtimeAsc => row_mtime(a)
                    .cmp(&row_mtime(b))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            })
    });
}

fn row_size(row: &PaneRow) -> u64 {
    row.right
        .as_ref()
        .and_then(|e| e.size)
        .or_else(|| row.left.as_ref().and_then(|e| e.size))
        .unwrap_or(0)
}

fn row_mtime(row: &PaneRow) -> i64 {
    row.right
        .as_ref()
        .or(row.left.as_ref())
        .map(|e| e.sig.mtime_sec)
        .unwrap_or(i64::MIN)
}
