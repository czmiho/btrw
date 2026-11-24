use super::PaneRow;
use crate::fs_safe::{format_os_display, read_file_limited};
use crate::model::EntryKind;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
    Info,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum FileDiffOutcome {
    Unified(Vec<DiffLine>),
    Identical,
    Binary,
    TooLarge,
    TooManyLines,
    NonRegular,
    Error(String),
}

pub fn diff_file(
    left_root: &Path,
    right_root: &Path,
    row: &PaneRow,
    max_bytes: u64,
    max_lines: usize,
) -> FileDiffOutcome {
    if row
        .left
        .as_ref()
        .map(|e| e.kind != EntryKind::File)
        .unwrap_or(false)
        || row
            .right
            .as_ref()
            .map(|e| e.kind != EntryKind::File)
            .unwrap_or(false)
    {
        return FileDiffOutcome::NonRegular;
    }

    let left_bytes = match &row.left {
        Some(_) => match read_file_limited(left_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return FileDiffOutcome::TooLarge;
            }
            Err(e) => return FileDiffOutcome::Error(e.to_string()),
        },
        None => Vec::new(),
    };

    let right_bytes = match &row.right {
        Some(_) => match read_file_limited(right_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return FileDiffOutcome::TooLarge;
            }
            Err(e) => return FileDiffOutcome::Error(e.to_string()),
        },
        None => Vec::new(),
    };

    if left_bytes.contains(&0) || right_bytes.contains(&0) {
        return FileDiffOutcome::Binary;
    }

    let Ok(left_str) = String::from_utf8(left_bytes) else {
        return FileDiffOutcome::Binary;
    };
    let Ok(right_str) = String::from_utf8(right_bytes) else {
        return FileDiffOutcome::Binary;
    };

    if left_str == right_str {
        return FileDiffOutcome::Identical;
    }

    if left_str.lines().count() + right_str.lines().count() > max_lines {
        return FileDiffOutcome::TooManyLines;
    }

    let diff = TextDiff::from_lines(&left_str, &right_str);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("left/{}", row.rel_path.display()),
            &format!("right/{}", row.rel_path.display()),
        )
        .to_string();

    let lines = unified
        .lines()
        .map(|line| {
            let kind = if line.starts_with("--- ") || line.starts_with("+++ ") {
                DiffLineKind::Header
            } else if line.starts_with("@@") {
                DiffLineKind::Hunk
            } else if line.starts_with('+') {
                DiffLineKind::Added
            } else if line.starts_with('-') {
                DiffLineKind::Removed
            } else {
                DiffLineKind::Context
            };
            DiffLine {
                kind,
                text: escape_control_chars(line),
            }
        })
        .collect();

    FileDiffOutcome::Unified(lines)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideRowKind {
    Equal,
    Changed,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
pub struct SideBySideRow {
    pub left_no: Option<usize>,
    pub left_text: String,
    pub right_no: Option<usize>,
    pub right_text: String,
    pub kind: SideRowKind,
}

#[derive(Debug, Clone)]
pub struct PatchExportSummary {
    pub files_exported: usize,
    pub files_skipped: usize,
}

/// Builds side-by-side diff rows for a single file pair.
pub fn diff_file_side_by_side(
    left_root: &Path,
    right_root: &Path,
    row: &PaneRow,
    max_bytes: u64,
    max_lines: usize,
) -> Result<Vec<SideBySideRow>, FileDiffOutcome> {
    if row
        .left
        .as_ref()
        .map(|e| e.kind != EntryKind::File)
        .unwrap_or(false)
        || row
            .right
            .as_ref()
            .map(|e| e.kind != EntryKind::File)
            .unwrap_or(false)
    {
        return Err(FileDiffOutcome::NonRegular);
    }

    let left_bytes = match &row.left {
        Some(_) => match read_file_limited(left_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return Err(FileDiffOutcome::TooLarge);
            }
            Err(e) => return Err(FileDiffOutcome::Error(e.to_string())),
        },
        None => Vec::new(),
    };
    let right_bytes = match &row.right {
        Some(_) => match read_file_limited(right_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return Err(FileDiffOutcome::TooLarge);
            }
            Err(e) => return Err(FileDiffOutcome::Error(e.to_string())),
        },
        None => Vec::new(),
    };

    if left_bytes.contains(&0) || right_bytes.contains(&0) {
        return Err(FileDiffOutcome::Binary);
    }
    let Ok(left_str) = String::from_utf8(left_bytes) else {
        return Err(FileDiffOutcome::Binary);
    };
    let Ok(right_str) = String::from_utf8(right_bytes) else {
        return Err(FileDiffOutcome::Binary);
    };
    if left_str.lines().count() + right_str.lines().count() > max_lines {
        return Err(FileDiffOutcome::TooManyLines);
    }

    let diff = TextDiff::from_lines(&left_str, &right_str);
    let mut rows = Vec::new();
    let mut pending_del: Vec<(usize, String)> = Vec::new();
    let mut pending_ins: Vec<(usize, String)> = Vec::new();

    let flush = |del: &mut Vec<(usize, String)>,
                 ins: &mut Vec<(usize, String)>,
                 out: &mut Vec<SideBySideRow>| {
        let max = del.len().max(ins.len());
        for i in 0..max {
            match (del.get(i), ins.get(i)) {
                (Some((ln, lt)), Some((rn, rt))) => out.push(SideBySideRow {
                    left_no: Some(*ln),
                    left_text: lt.clone(),
                    right_no: Some(*rn),
                    right_text: rt.clone(),
                    kind: SideRowKind::Changed,
                }),
                (Some((ln, lt)), None) => out.push(SideBySideRow {
                    left_no: Some(*ln),
                    left_text: lt.clone(),
                    right_no: None,
                    right_text: String::new(),
                    kind: SideRowKind::Removed,
                }),
                (None, Some((rn, rt))) => out.push(SideBySideRow {
                    left_no: None,
                    left_text: String::new(),
                    right_no: Some(*rn),
                    right_text: rt.clone(),
                    kind: SideRowKind::Added,
                }),
                (None, None) => {}
            }
        }
        del.clear();
        ins.clear();
    };

    for change in diff.iter_all_changes() {
        let text = escape_control_chars(change.value().trim_end_matches(['\r', '\n']));
        match change.tag() {
            ChangeTag::Equal => {
                flush(&mut pending_del, &mut pending_ins, &mut rows);
                rows.push(SideBySideRow {
                    left_no: change.old_index().map(|i| i + 1),
                    left_text: text.clone(),
                    right_no: change.new_index().map(|i| i + 1),
                    right_text: text,
                    kind: SideRowKind::Equal,
                });
            }
            ChangeTag::Delete => {
                pending_del.push((change.old_index().map(|i| i + 1).unwrap_or(0), text));
            }
            ChangeTag::Insert => {
                pending_ins.push((change.new_index().map(|i| i + 1).unwrap_or(0), text));
            }
        }
    }
    flush(&mut pending_del, &mut pending_ins, &mut rows);
    Ok(rows)
}

/// Generates a raw unified diff patch preserving exact tabs, CRLF, and missing trailing newlines
/// (unlike `diff_file`, which escapes control characters for terminal rendering).
pub enum PatchFileErr {
    Skipped,
    ReadError(String),
}

fn diff_file_raw_patch(
    left_root: &Path,
    right_root: &Path,
    row: &PaneRow,
    max_bytes: u64,
    max_lines: usize,
) -> Result<Option<String>, PatchFileErr> {
    if row
        .left
        .as_ref()
        .map(|e| e.kind != EntryKind::File)
        .unwrap_or(false)
        || row
            .right
            .as_ref()
            .map(|e| e.kind != EntryKind::File)
            .unwrap_or(false)
    {
        return Err(PatchFileErr::Skipped);
    }

    let left_bytes = match &row.left {
        Some(_) => match read_file_limited(left_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return Err(PatchFileErr::Skipped);
            }
            Err(e) => {
                return Err(PatchFileErr::ReadError(format!(
                    "read left {}: {e}",
                    format_os_display(row.rel_path.as_os_str())
                )));
            }
        },
        None => Vec::new(),
    };
    let right_bytes = match &row.right {
        Some(_) => match read_file_limited(right_root, &row.rel_path, max_bytes) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
                return Err(PatchFileErr::Skipped);
            }
            Err(e) => {
                return Err(PatchFileErr::ReadError(format!(
                    "read right {}: {e}",
                    format_os_display(row.rel_path.as_os_str())
                )));
            }
        },
        None => Vec::new(),
    };

    if left_bytes.contains(&0) || right_bytes.contains(&0) {
        return Err(PatchFileErr::Skipped);
    }
    let left_str = String::from_utf8(left_bytes).map_err(|_| PatchFileErr::Skipped)?;
    let right_str = String::from_utf8(right_bytes).map_err(|_| PatchFileErr::Skipped)?;

    if left_str.lines().count() + right_str.lines().count() > max_lines {
        return Err(PatchFileErr::Skipped);
    }

    // Reject non-UTF-8 paths from unified patch headers so filenames are never lossy.
    if row.rel_path.to_str().is_none() {
        return Err(PatchFileErr::Skipped);
    }

    let a_git = format_patch_header_path("a/", &row.rel_path);
    let b_git = format_patch_header_path("b/", &row.rel_path);

    // Handle added/deleted empty regular files with valid git-apply headers so they apply cleanly.
    if left_str == right_str {
        if let (None, Some(r)) = (&row.left, &row.right) {
            let mode = r.sig.mode & 0o777;
            return Ok(Some(format!(
                "diff --git {a_git} {b_git}\nnew file mode 100{mode:03o}\nindex 0000000..e69de29\n--- /dev/null\n+++ {b_git}\n"
            )));
        }
        if let (Some(l), None) = (&row.left, &row.right) {
            let mode = l.sig.mode & 0o777;
            return Ok(Some(format!(
                "diff --git {a_git} {b_git}\ndeleted file mode 100{mode:03o}\nindex e69de29..0000000\n--- {a_git}\n+++ /dev/null\n"
            )));
        }
        return Ok(None);
    }

    let from_hdr = if row.left.is_some() {
        a_git.clone()
    } else {
        "/dev/null".to_string()
    };
    let to_hdr = if row.right.is_some() {
        b_git.clone()
    } else {
        "/dev/null".to_string()
    };

    let diff = TextDiff::from_lines(&left_str, &right_str);
    let mut unified = format!("diff --git {a_git} {b_git}\n");
    if let (None, Some(r)) = (&row.left, &row.right) {
        let mode = r.sig.mode & 0o777;
        unified.push_str(&format!("new file mode 100{mode:03o}\n"));
    } else if let (Some(l), None) = (&row.left, &row.right) {
        let mode = l.sig.mode & 0o777;
        unified.push_str(&format!("deleted file mode 100{mode:03o}\n"));
    }
    unified.push_str(
        &diff
            .unified_diff()
            .context_radius(3)
            .missing_newline_hint(true)
            .header(&from_hdr, &to_hdr)
            .to_string(),
    );
    if !unified.ends_with('\n') {
        unified.push('\n');
    }
    Ok(Some(unified))
}

fn format_patch_header_path(prefix: &str, rel: &Path) -> String {
    let raw = format!("{prefix}{}", format_os_display(rel.as_os_str()));
    if raw.bytes().any(|b| b == b'\t' || b == b'\n' || b == b'\r' || b == b'"' || b == b'\\') {
        let mut out = String::with_capacity(raw.len() + 4);
        out.push('"');
        for ch in raw.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    } else {
        raw
    }
}

/// Exports raw unified diff `.patch` for all changed regular text files in `rows`.
/// Publishes atomically via an exclusive temp file + `linkat` (refuses to overwrite an existing
/// file or follow symlinks).
static PATCH_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn export_patch(
    left_root: &Path,
    right_root: &Path,
    rows: &[PaneRow],
    output_path: &Path,
    max_bytes: u64,
    max_lines: usize,
) -> Result<PatchExportSummary, String> {
    let cancel = AtomicBool::new(false);
    export_patch_cancellable(
        left_root,
        right_root,
        rows,
        output_path,
        max_bytes,
        max_lines,
        &cancel,
    )
}

pub fn export_patch_cancellable(
    left_root: &Path,
    right_root: &Path,
    rows: &[PaneRow],
    output_path: &Path,
    max_bytes: u64,
    max_lines: usize,
    cancel: &AtomicBool,
) -> Result<PatchExportSummary, String> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let mut patch = String::new();
    let mut exported = 0usize;
    let mut skipped = 0usize;

    for row in rows {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if row.is_dir() || row.status.is_none() {
            continue;
        }
        match diff_file_raw_patch(left_root, right_root, row, max_bytes, max_lines) {
            Ok(Some(chunk)) => {
                patch.push_str(&chunk);
                exported += 1;
            }
            Ok(None) => {}
            Err(PatchFileErr::Skipped) => {
                let safe_name = escape_control_chars(&format_os_display(row.rel_path.as_os_str()));
                patch.push_str(&format!("# skipped non-text/over-limit file: {safe_name}\n"));
                skipped += 1;
            }
            Err(PatchFileErr::ReadError(err)) => {
                return Err(format!("patch export aborted due to read error: {err}"));
            }
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }

    let parent = output_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dest_name = output_path
        .file_name()
        .ok_or_else(|| "patch output path has no file name".to_string())?;

    let sudo_ids = if unsafe { libc::geteuid() } == 0
        && let (Ok(u), Ok(g)) = (std::env::var("SUDO_UID"), std::env::var("SUDO_GID"))
        && let (Ok(uid), Ok(gid)) = (u.parse::<u32>(), g.parse::<u32>())
        && uid != 0
    {
        Some((uid, gid))
    } else {
        None
    };

    let dir_fd = crate::fs_safe::ensure_dir_chain_owned(parent, 0o755, sudo_ids)
        .map_err(|e| format!("open/create output dir failed: {e}"))?;
    let seq = PATCH_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.patch.{}.{seq}.tmp",
        dest_name.to_string_lossy(),
        std::process::id()
    );
    let tmp_c = CString::new(tmp_name.as_bytes()).map_err(|_| "NUL in temp name".to_string())?;
    let dest_c = CString::new(dest_name.as_bytes()).map_err(|_| "NUL in dest name".to_string())?;

    let raw_fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            tmp_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw_fd < 0 {
        return Err(format!(
            "create temp patch failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut tmp_file = fs::File::from(unsafe { OwnedFd::from_raw_fd(raw_fd) });
    let write_res: std::io::Result<()> = (|| {
        tmp_file.write_all(patch.as_bytes())?;
        if let Some((uid, gid)) = sudo_ids {
            let rc = unsafe { libc::fchown(tmp_file.as_raw_fd(), uid, gid) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        // Keep private 0600 permissions since patches may contain sensitive diffs (e.g. /etc).
        let rc = unsafe { libc::fchmod(tmp_file.as_raw_fd(), 0o600) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        tmp_file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        drop(tmp_file);
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
        }
        return Err(format!("write patch failed: {e}"));
    }
    drop(tmp_file);

    if cancel.load(Ordering::Relaxed) {
        unsafe {
            libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
        }
        return Err("cancelled".to_string());
    }

    // Atomic publish without overwriting an existing file or following symlinks (`linkat` fails with EEXIST).
    let rc = unsafe {
        libc::linkat(
            dir_fd.as_raw_fd(),
            tmp_c.as_ptr(),
            dir_fd.as_raw_fd(),
            dest_c.as_ptr(),
            0,
        )
    };
    let link_err = if rc != 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
    }
    if let Some(err) = link_err {
        return Err(format!("cannot publish patch (target exists or error): {err}"));
    }
    unsafe {
        libc::fsync(dir_fd.as_raw_fd());
    }

    Ok(PatchExportSummary {
        files_exported: exported,
        files_skipped: skipped,
    })
}

pub fn escape_control_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            out.push_str("    ");
        } else if ch.is_control() {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}

