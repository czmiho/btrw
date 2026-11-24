use crate::app::App;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use btrw_core::compare::human_size;
use btrw_core::fs_safe::{find_in_path, read_file_limited};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

fn resolve_external_diff_cmd(configured: Option<&str>) -> Option<Vec<String>> {
    if let Some(cmd) = configured {
        let parts: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    if let Ok(env_cmd) = std::env::var("DIFFPROG") {
        let parts: Vec<String> = env_cmd.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    for candidate in [
        &["meld"][..],
        &["kdiff3"][..],
        &["code", "--diff"][..],
        &["nvim", "-d"][..],
        &["vimdiff"][..],
    ] {
        if find_in_path(candidate[0]).is_some() {
            return Some(candidate.iter().map(|s| (*s).to_string()).collect());
        }
    }
    None
}

const EXT_DIFF_MAX_BYTES: u64 = 16 * 1024 * 1024;

struct SudoUser {
    uid: u32,
    gid: u32,
    name: String,
    home: PathBuf,
}

fn detect_sudo_user() -> Option<SudoUser> {
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid: u32 = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    let name = std::env::var("SUDO_USER").unwrap_or_else(|_| uid.to_string());
    let home = std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| {
            passwd.lines().find_map(|line| {
                let mut fields = line.split(':');
                let user = fields.next()?;
                let _ = fields.next()?;
                let fuid: u32 = fields.next()?.parse().ok()?;
                let _ = fields.next()?;
                let _ = fields.next()?;
                let h = fields.next()?;
                if fuid == uid || user == name {
                    Some(PathBuf::from(h))
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Some(SudoUser {
        uid,
        gid,
        name,
        home,
    })
}

/// Tests whether `sudo` user can directly open & read `path` (covers directory traversal,
/// mount permissions, and file permissions with the exact credentials used for the diff tool).
fn can_user_read(path: &Path, sudo: &SudoUser) -> bool {
    let Some(test_bin) = find_in_path("test") else {
        return false;
    };
    let mut cmd = std::process::Command::new(test_bin);
    cmd.arg("-r")
        .arg(path)
        .uid(sudo.uid)
        .gid(sudo.gid)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setgroups(0, std::ptr::null());
            Ok(())
        });
    }
    cmd.status().is_ok_and(|s| s.success())
}

struct StageDir {
    parent_fd: std::os::fd::OwnedFd,
    dir_name: std::ffi::CString,
    path: PathBuf,
    fd: std::os::fd::OwnedFd,
    created_files: Vec<std::ffi::CString>,
}

fn ensure_stage_dir(stage: &mut Option<StageDir>) -> Result<&mut StageDir, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    if stage.is_none() {
        let tmp_base = Path::new("/tmp");
        let parent_c = CString::new("/tmp").unwrap();
        let pfd = unsafe {
            libc::open(
                parent_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if pfd < 0 {
            return Err(format!("open /tmp failed: {}", io::Error::last_os_error()));
        }
        let parent_fd = unsafe { OwnedFd::from_raw_fd(pfd) };
        let pst = btrw_core::fs_safe::fstat_fd(parent_fd.as_raw_fd())
            .map_err(|e| format!("fstat /tmp failed: {e}"))?;
        if pst.uid != 0 || (pst.mode & libc::S_ISVTX) == 0 {
            return Err("refusing untrusted /tmp without root ownership and sticky bit".to_string());
        }

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name_str = format!("btrw-ext-{}-{stamp}", std::process::id());
        let dir_name = CString::new(name_str.as_bytes()).map_err(|_| "NUL in dir name".to_string())?;
        // Keep dir owned by root:root (0700) until all privileged writes are complete!
        let mk = unsafe { libc::mkdirat(parent_fd.as_raw_fd(), dir_name.as_ptr(), 0o700) };
        if mk != 0 {
            return Err(format!(
                "create temp dir failed: {}",
                io::Error::last_os_error()
            ));
        }
        let fd = unsafe {
            libc::openat(
                parent_fd.as_raw_fd(),
                dir_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            unsafe {
                libc::unlinkat(parent_fd.as_raw_fd(), dir_name.as_ptr(), libc::AT_REMOVEDIR);
            }
            return Err(format!(
                "open temp dir failed: {}",
                io::Error::last_os_error()
            ));
        }
        let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let st = btrw_core::fs_safe::fstat_fd(owned_fd.as_raw_fd())
            .map_err(|e| format!("fstat temp dir failed: {e}"))?;
        if st.uid != unsafe { libc::geteuid() } || (st.mode & 0o777) != 0o700 {
            unsafe {
                libc::unlinkat(parent_fd.as_raw_fd(), dir_name.as_ptr(), libc::AT_REMOVEDIR);
            }
            return Err("staged temp dir has unexpected owner or mode".to_string());
        }
        *stage = Some(StageDir {
            parent_fd,
            dir_name,
            path: tmp_base.join(name_str),
            fd: owned_fd,
            created_files: Vec::new(),
        });
    }
    Ok(stage.as_mut().unwrap())
}

impl Drop for StageDir {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // Unlink ONLY the exact filenames we created via our held dir_fd, never `remove_dir_all` as root!
        for cname in &self.created_files {
            unsafe {
                libc::unlinkat(self.fd.as_raw_fd(), cname.as_ptr(), 0);
            }
        }
        unsafe {
            libc::unlinkat(
                self.parent_fd.as_raw_fd(),
                self.dir_name.as_ptr(),
                libc::AT_REMOVEDIR,
            );
        }
    }
}

fn cleanup_stage_dir(stage: StageDir) {
    drop(stage);
}

fn stage_single_file(
    root: &Path,
    rel_path: &Path,
    prefix: &str,
    stage: &mut Option<StageDir>,
    sudo: &SudoUser,
) -> Result<PathBuf, String> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let bytes =
        read_file_limited(root, rel_path, EXT_DIFF_MAX_BYTES).map_err(|e| format!("{e}"))?;
    let st_dir = ensure_stage_dir(stage)?;
    let dir_fd = st_dir.fd.as_raw_fd();
    let fname = rel_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let staged_name = format!("{prefix}_{fname}");
    let cname = CString::new(staged_name.as_bytes()).map_err(|_| "NUL in staged name".to_string())?;

    let raw_fd = unsafe {
        libc::openat(
            dir_fd,
            cname.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw_fd < 0 {
        return Err(format!(
            "create temp file failed: {}",
            io::Error::last_os_error()
        ));
    }
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw_fd) });
    st_dir.created_files.push(cname);
    file.write_all(&bytes)
        .map_err(|e| format!("write temp file failed: {e}"))?;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0 {
        return Err(format!(
            "fchmod temp file failed: {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fchown(file.as_raw_fd(), sudo.uid, sudo.gid) } != 0 {
        return Err(format!(
            "fchown temp file failed: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(st_dir.path.join(staged_name))
}

fn finalize_stage_dir_for_user(stage: &StageDir, sudo: &SudoUser) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::fchown(stage.fd.as_raw_fd(), sudo.uid, sudo.gid) } != 0 {
        return Err(format!(
            "fchown temp dir failed: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn launch_external_diff(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mouse_active: bool,
) -> io::Result<()> {
    let Some(row) = app.selected_row().cloned() else {
        app.notice = Some(app.catalog.t("extdiff-need-file"));
        return Ok(());
    };
    if row.is_dir() || row.left.is_none() || row.right.is_none() {
        app.notice = Some(app.catalog.t("extdiff-need-file"));
        return Ok(());
    }
    let (Some(left_src), Some(right_src)) = (app.left_source(), app.right_source()) else {
        app.notice = Some(app.catalog.t("extdiff-need-file"));
        return Ok(());
    };
    let left_root = left_src.path().clone();
    let right_root = right_src.path().clone();
    let left_direct = left_root.join(&row.rel_path);
    let right_direct = right_root.join(&row.rel_path);

    let Some(mut cmd_parts) =
        resolve_external_diff_cmd(app.config.diff.external_diff_cmd.as_deref())
    else {
        app.notice = Some(app.catalog.t("extdiff-not-found"));
        return Ok(());
    };

    // When invoked via sudo, de-escalate GUI diff tools (e.g. VS Code refuses root) back to
    // SUDO_UID:SUDO_GID. Test direct readability per side first; only unreadable sides are
    // staged into a private 0700 temp dir (0400 files, capped at 16 MiB) and cleaned up on close.
    let sudo_user = detect_sudo_user();
    let mut stage_dir_to_clean: Option<StageDir> = None;

    let (left_arg, right_arg) = if let Some(ref sudo) = sudo_user {
        let left_ok = can_user_read(&left_direct, sudo);
        let right_ok = can_user_read(&right_direct, sudo);

        let left_path = if left_ok {
            left_direct
        } else {
            let lsize = row.left.as_ref().and_then(|e| e.size).unwrap_or(0);
            if lsize > EXT_DIFF_MAX_BYTES {
                app.notice = Some(app.catalog.t_args(
                    "extdiff-too-large",
                    &[("limit", human_size(EXT_DIFF_MAX_BYTES).into())],
                ));
                return Ok(());
            }
            match stage_single_file(
                &left_root,
                &row.rel_path,
                "LEFT",
                &mut stage_dir_to_clean,
                sudo,
            ) {
                Ok(p) => p,
                Err(detail) => {
                    if let Some(s) = stage_dir_to_clean {
                        cleanup_stage_dir(s);
                    }
                    app.notice = Some(
                        app.catalog
                            .t_args("extdiff-failed", &[("detail", detail.into())]),
                    );
                    return Ok(());
                }
            }
        };

        let right_path = if right_ok {
            right_direct
        } else {
            let rsize = row.right.as_ref().and_then(|e| e.size).unwrap_or(0);
            if rsize > EXT_DIFF_MAX_BYTES {
                if let Some(s) = stage_dir_to_clean {
                    cleanup_stage_dir(s);
                }
                app.notice = Some(app.catalog.t_args(
                    "extdiff-too-large",
                    &[("limit", human_size(EXT_DIFF_MAX_BYTES).into())],
                ));
                return Ok(());
            }
            match stage_single_file(
                &right_root,
                &row.rel_path,
                "RIGHT",
                &mut stage_dir_to_clean,
                sudo,
            ) {
                Ok(p) => p,
                Err(detail) => {
                    if let Some(s) = stage_dir_to_clean {
                        cleanup_stage_dir(s);
                    }
                    app.notice = Some(
                        app.catalog
                            .t_args("extdiff-failed", &[("detail", detail.into())]),
                    );
                    return Ok(());
                }
            }
        };

        // Hand over staging dir ownership to SUDO_UID only after all privileged writes completed.
        let finalize_err = stage_dir_to_clean
            .as_ref()
            .and_then(|s| finalize_stage_dir_for_user(s, sudo).err());
        if let Some(detail) = finalize_err {
            if let Some(s) = stage_dir_to_clean.take() {
                cleanup_stage_dir(s);
            }
            app.notice = Some(
                app.catalog
                    .t_args("extdiff-failed", &[("detail", detail.into())]),
            );
            return Ok(());
        }

        (left_path, right_path)
    } else {
        (left_direct, right_direct)
    };

    // Only inject --wait for VS Code when we staged temp files that must be cleaned up on close.
    let bin_name = Path::new(&cmd_parts[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if stage_dir_to_clean.is_some()
        && (bin_name == "code" || bin_name == "code-insiders" || bin_name == "codium")
        && !cmd_parts.iter().any(|a| a == "--wait" || a == "-w")
    {
        cmd_parts.insert(1, "--wait".to_string());
    }

    if mouse_active {
        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let mut cmd = std::process::Command::new(&cmd_parts[0]);
    cmd.args(&cmd_parts[1..]).arg(&left_arg).arg(&right_arg);
    if let Some(ref sudo) = sudo_user {
        cmd.uid(sudo.uid)
            .gid(sudo.gid)
            .env("HOME", &sudo.home)
            .env("USER", &sudo.name)
            .env("LOGNAME", &sudo.name);
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{}", sudo.uid));
        }
        unsafe {
            cmd.pre_exec(|| {
                libc::setgroups(0, std::ptr::null());
                Ok(())
            });
        }
    }
    let status = cmd.status();

    if let Some(s) = stage_dir_to_clean {
        cleanup_stage_dir(s);
    }

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    if mouse_active {
        execute!(terminal.backend_mut(), EnableMouseCapture)?;
    }
    terminal.clear()?;

    if let Err(e) = status {
        app.notice = Some(
            app.catalog
                .t_args("extdiff-failed", &[("detail", e.to_string().into())]),
        );
    }
    Ok(())
}

