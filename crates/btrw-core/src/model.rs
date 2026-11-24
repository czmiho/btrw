use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    Scheduled,
    Event,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub path: PathBuf,
    pub live_target: PathBuf,
    pub label: String,
    pub created_at: Option<SystemTime>,
    pub kind: SnapshotKind,
    pub read_only: bool,
    /// True only when matched to `live_target` via exact Btrfs `(FSID, parent_uuid == uuid)`.
    /// Heuristic filename matches keep this `false` so automatic F5 live restore is refused.
    pub verified_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Snapshot(SnapshotInfo),
    Live { path: PathBuf, label: String },
    CustomDir { path: PathBuf, label: String },
}

impl Source {
    pub fn path(&self) -> &PathBuf {
        match self {
            Source::Snapshot(info) => &info.path,
            Source::Live { path, .. } | Source::CustomDir { path, .. } => path,
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, Source::Snapshot(info) if info.read_only)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    TypeChanged,
    MetadataOnly,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScope {
    File,
    Directory,
    Snapshot,
}
