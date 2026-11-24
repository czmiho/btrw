use crate::compare::escape_control_chars;
use crate::config::TimeTravelHighlight;
use crate::fs_safe::{
    StatSig, files_content_equal_cancellable, format_system_time_local, open_file_safe,
    read_file_limited,
};
use crate::model::Source;
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct FileVersionMeta {
    pub timeline_idx: usize,
    pub root_path: PathBuf,
    pub label: String,
    pub timestamp: Option<SystemTime>,
    pub is_live: bool,
    pub verified_parent: bool,
    pub exists: bool,
    pub size: Option<u64>,
    pub error: Option<String>,
}

impl FileVersionMeta {
    pub fn short_label(&self) -> String {
        if self.is_live {
            self.label.clone()
        } else if let Some(ts) = self.timestamp {
            format_system_time_local(ts)
        } else {
            self.label.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimeTravelHistory {
    pub rel_path: PathBuf,
    pub total_sources_checked: usize,
    pub versions: Vec<FileVersionMeta>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum VersionContent {
    Text(String),
    Missing,
    Binary,
    TooLarge,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeTravelLineKind {
    Context,
    Added,
    Removed,
    Info,
}

#[derive(Debug, Clone)]
pub struct TimeTravelLine {
    pub line_no: Option<usize>,
    pub kind: TimeTravelLineKind,
    pub text: String,
}

/// Scans a single `rel_path` across all `sources` (oldest -> newest -> LIVE) and keeps
/// only snapshots where the file actually changed.
///
/// Because it touches one path per snapshot and uses `FIEMAP` physical extent equality,
/// deduplicating 200+ hourly snapshots finishes in milliseconds with zero data reads on unchanged versions.
pub fn discover_file_versions(
    sources: &[Source],
    rel_path: &Path,
    cancel: &AtomicBool,
) -> TimeTravelHistory {
    let mut versions: Vec<FileVersionMeta> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut prev_state: Option<(PathBuf, Option<StatSig>)> = None;

    for (idx, src) in sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let root = src.path().clone();
        let is_live = matches!(src, Source::Live { .. });
        let (current_sig, open_err) = match open_file_safe(&root, rel_path) {
            Ok((_, sig)) => (Some(sig), None),
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                (None, None)
            }
            Err(e) => (None, Some(e.to_string())),
        };

        let mut compare_err: Option<String> = None;
        let changed = if open_err.is_some() {
            true
        } else {
            match (&prev_state, &current_sig) {
                (None, None) => false, // Skip leading snapshots before the file existed.
                (None, Some(_)) => true,
                (Some((_, None)), None) => false,
                (Some((_, None)), Some(_)) => true,
                (Some((_, Some(_))), None) => true,
                (Some((prev_root, Some(psig))), Some(csig)) => {
                    match files_content_equal_cancellable(
                        prev_root, &root, rel_path, psig, csig, cancel,
                    ) {
                        Ok(eq) => !eq,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => break,
                        Err(e) => {
                            compare_err = Some(e.to_string());
                            true
                        }
                    }
                }
            }
        };

        let step_error = open_err.or(compare_err);
        if let Some(ref err_msg) = step_error {
            let src_label = match src {
                Source::Snapshot(info) => info.label.as_str(),
                Source::Live { label, .. } | Source::CustomDir { label, .. } => label.as_str(),
            };
            errors.push(format!("{src_label}: {err_msg}"));
        }

        if changed {
            let (label, timestamp) = match src {
                Source::Snapshot(info) => (info.label.clone(), info.created_at),
                Source::Live { label, .. } | Source::CustomDir { label, .. } => {
                    let ts = current_sig.and_then(|s| {
                        if s.mtime_sec >= 0 {
                            Some(UNIX_EPOCH + Duration::from_secs(s.mtime_sec as u64))
                        } else {
                            None
                        }
                    });
                    (label.clone(), ts)
                }
            };
            let verified_parent = matches!(src, Source::Snapshot(info) if info.verified_parent);
            versions.push(FileVersionMeta {
                timeline_idx: idx,
                root_path: root.clone(),
                label,
                timestamp,
                is_live,
                verified_parent,
                exists: current_sig.is_some(),
                size: current_sig.map(|s| s.size),
                error: step_error.clone(),
            });
            if step_error.is_none() {
                prev_state = Some((root, current_sig));
            }
        }
    }

    TimeTravelHistory {
        rel_path: rel_path.to_path_buf(),
        total_sources_checked: sources.len(),
        versions,
        errors,
    }
}

/// Loads UTF-8 text content for one version safely up to `max_bytes`.
pub fn load_version_content(
    version: &FileVersionMeta,
    rel_path: &Path,
    max_bytes: u64,
) -> VersionContent {
    if let Some(ref err) = version.error {
        return VersionContent::Error(err.clone());
    }
    if !version.exists {
        return VersionContent::Missing;
    }
    match read_file_limited(&version.root_path, rel_path, max_bytes) {
        Ok(bytes) => {
            if bytes.contains(&0) {
                return VersionContent::Binary;
            }
            match String::from_utf8(bytes) {
                Ok(s) => VersionContent::Text(s),
                Err(_) => VersionContent::Binary,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => VersionContent::TooLarge,
        Err(e) => VersionContent::Error(e.to_string()),
    }
}

/// Renders a file version with live change highlighting against `reference`
/// (either previous version, LIVE state, or clean text when `highlight == None`).
pub fn render_version_lines(
    current: &VersionContent,
    reference: Option<&VersionContent>,
    highlight: TimeTravelHighlight,
    max_lines: usize,
) -> Vec<TimeTravelLine> {
    let cur_text = match current {
        VersionContent::Text(s) => s,
        VersionContent::Missing => {
            return vec![TimeTravelLine {
                line_no: None,
                kind: TimeTravelLineKind::Info,
                text: "(file did not exist in this snapshot)".to_string(),
            }];
        }
        VersionContent::Binary => {
            return vec![TimeTravelLine {
                line_no: None,
                kind: TimeTravelLineKind::Info,
                text: "(binary file)".to_string(),
            }];
        }
        VersionContent::TooLarge => {
            return vec![TimeTravelLine {
                line_no: None,
                kind: TimeTravelLineKind::Info,
                text: "(file exceeds text preview limit)".to_string(),
            }];
        }
        VersionContent::Error(e) => {
            return vec![TimeTravelLine {
                line_no: None,
                kind: TimeTravelLineKind::Info,
                text: format!("(read error: {e})"),
            }];
        }
    };

    let ref_text = match (highlight, reference) {
        (TimeTravelHighlight::None, _) | (_, None) => None,
        (_, Some(VersionContent::Text(s))) => Some(s.as_str()),
        (_, Some(VersionContent::Missing)) => Some(""),
        _ => None,
    };

    let Some(other) = ref_text else {
        return cur_text
            .lines()
            .enumerate()
            .map(|(i, line)| TimeTravelLine {
                line_no: Some(i + 1),
                kind: TimeTravelLineKind::Context,
                text: escape_control_chars(line),
            })
            .collect();
    };

    if cur_text.lines().count() + other.lines().count() > max_lines {
        return cur_text
            .lines()
            .enumerate()
            .map(|(i, line)| TimeTravelLine {
                line_no: Some(i + 1),
                kind: TimeTravelLineKind::Context,
                text: escape_control_chars(line),
            })
            .collect();
    }

    // For PreviousVersion: diff(previous -> current), so insertions are new in current (+).
    // For LiveState: diff(live -> current), so insertions are what current had differently (+).
    let diff = TextDiff::from_lines(other, cur_text);
    let mut out = Vec::new();
    for change in diff.iter_all_changes() {
        let text = escape_control_chars(change.value().trim_end_matches(['\r', '\n']));
        match change.tag() {
            ChangeTag::Equal => out.push(TimeTravelLine {
                line_no: change.new_index().map(|i| i + 1),
                kind: TimeTravelLineKind::Context,
                text,
            }),
            ChangeTag::Insert => out.push(TimeTravelLine {
                line_no: change.new_index().map(|i| i + 1),
                kind: TimeTravelLineKind::Added,
                text,
            }),
            ChangeTag::Delete => out.push(TimeTravelLine {
                line_no: change.old_index().map(|i| i + 1),
                kind: TimeTravelLineKind::Removed,
                text,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SnapshotInfo, SnapshotKind};
    use std::fs;

    #[test]
    fn deduplicates_unchanged_snapshots_and_highlights_changes() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-tt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let dirs: Vec<PathBuf> = (0..5).map(|i| base.join(format!("s{i}"))).collect();
        for d in &dirs {
            fs::create_dir_all(d).unwrap();
        }
        // s0: missing, s1: v1, s2: v1 (identical!), s3: v2, s4: v2 (identical!)
        fs::write(dirs[1].join("f.txt"), "a\nb\n").unwrap();
        fs::write(dirs[2].join("f.txt"), "a\nb\n").unwrap();
        fs::write(dirs[3].join("f.txt"), "a\nb_changed\nc\n").unwrap();
        fs::write(dirs[4].join("f.txt"), "a\nb_changed\nc\n").unwrap();

        let sources: Vec<Source> = dirs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                Source::Snapshot(SnapshotInfo {
                    path: p.clone(),
                    live_target: PathBuf::from("/"),
                    label: format!("s{i}"),
                    created_at: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i as u64 * 3600)),
                    kind: SnapshotKind::Scheduled,
                    read_only: true,
                    verified_parent: true,
                })
            })
            .collect();

        let cancel = AtomicBool::new(false);
        let hist = discover_file_versions(&sources, Path::new("f.txt"), &cancel);
        assert_eq!(hist.total_sources_checked, 5);
        // Only s1 (created) and s3 (modified) should remain!
        assert_eq!(hist.versions.len(), 2);
        assert_eq!(hist.versions[0].label, "s1");
        assert_eq!(hist.versions[1].label, "s3");

        let c0 = load_version_content(&hist.versions[0], Path::new("f.txt"), 1024 * 1024);
        let c1 = load_version_content(&hist.versions[1], Path::new("f.txt"), 1024 * 1024);
        let rendered = render_version_lines(&c1, Some(&c0), TimeTravelHighlight::PreviousVersion, 1000);
        assert!(rendered.iter().any(|l| l.kind == TimeTravelLineKind::Removed && l.text == "b"));
        assert!(rendered.iter().any(|l| l.kind == TimeTravelLineKind::Added && l.text == "b_changed"));

        let _ = fs::remove_dir_all(&base);
    }
}
