use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static CFG_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub panels: PanelsConfig,
    pub diff: DiffConfig,
    pub restore: RestoreConfig,
    pub btrfs: BtrfsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// "auto", "en", or "cs"
    pub language: String,
    pub theme: Theme,
    pub snapshots_dir: PathBuf,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            language: "auto".to_string(),
            theme: Theme::Dark,
            // Empty = auto-discover snapshot roots from mounted Btrfs filesystems in /proc/self/mountinfo.
            snapshots_dir: PathBuf::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    McBlue,
    Dark,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelsConfig {
    pub sync_navigation: bool,
    pub default_view: PanelViewMode,
    pub default_sort: SortOrder,
    pub hide_unchanged: bool,
    pub show_size: bool,
    pub show_delta: bool,
    pub show_mtime: bool,
    pub show_permissions: PermissionsColumn,
    pub mouse_enabled: bool,
}

impl Default for PanelsConfig {
    fn default() -> Self {
        Self {
            sync_navigation: true,
            default_view: PanelViewMode::Tree,
            default_sort: SortOrder::NameAsc,
            hide_unchanged: false,
            show_size: true,
            show_delta: true,
            show_mtime: true,
            show_permissions: PermissionsColumn::Off,
            mouse_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionsColumn {
    Off,
    Octal,
    OctalOwnerGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelViewMode {
    Tree,
    FlatChanges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    NameAsc,
    NameDesc,
    SizeDesc,
    SizeAsc,
    DeltaDesc,
    DeltaAsc,
    MtimeDesc,
    MtimeAsc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffConfig {
    pub show_metadata_only: bool,
    pub format: DiffFormat,
    pub timetravel_highlight: TimeTravelHighlight,
    pub max_file_bytes: u64,
    pub max_lines: usize,
    pub external_diff_cmd: Option<String>,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            show_metadata_only: false,
            format: DiffFormat::Unified,
            timetravel_highlight: TimeTravelHighlight::PreviousVersion,
            max_file_bytes: 2 * 1024 * 1024,
            max_lines: 20_000,
            external_diff_cmd: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffFormat {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeTravelHighlight {
    PreviousVersion,
    LiveState,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RestoreConfig {
    pub create_backup: bool,
    pub use_reflink: ReflinkMode,
    pub preserve_ownership: bool,
    pub preserve_permissions: bool,
    pub preserve_xattrs: bool,
}

impl Default for RestoreConfig {
    fn default() -> Self {
        Self {
            create_backup: true,
            use_reflink: ReflinkMode::Auto,
            preserve_ownership: true,
            preserve_permissions: true,
            preserve_xattrs: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflinkMode {
    Auto,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BtrfsConfig {
    pub live_strategy: LiveDiffStrategy,
}

impl Default for BtrfsConfig {
    fn default() -> Self {
        Self {
            live_strategy: LiveDiffStrategy::ReadOnlyFiemap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveDiffStrategy {
    /// Strict read-only traversal + FS_IOC_FIEMAP extent comparison.
    ReadOnlyFiemap,
    /// Opt-in temporary read-only snapshot of live subvolume for atomic btrfs send.
    TempRoSnapshot,
}

impl Config {
    /// Resolves the default user config path, respecting `$SUDO_USER` when invoked via sudo.
    pub fn default_path() -> PathBuf {
        if let Some(sudo_home) = sudo_user_home() {
            return sudo_home.join(".config/btrw/config.toml");
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join("btrw/config.toml");
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            return PathBuf::from(home).join(".config/btrw/config.toml");
        }
        PathBuf::from("btrw.toml")
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let parsed: Self = toml::from_str(&raw)?;
        Ok(parsed)
    }

    pub fn to_toml_pretty(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Saves configuration atomically via an exclusive temp file (`O_CREAT | O_EXCL | O_NOFOLLOW`)
    /// in the parent directory, preserving `$SUDO_UID:$SUDO_GID` ownership when run under `sudo`.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "config path has no file name")
        })?;

        let sudo_ids = sudo_uid_gid();
        // Walk from `/` with O_NOFOLLOW, creating any missing parent components via mkdirat
        // and fchowning every newly created directory to SUDO_UID:SUDO_GID.
        let dir_fd = crate::fs_safe::ensure_dir_chain_owned(parent, 0o755, sudo_ids)?;
        let serialized = self.to_toml_pretty()?;
        let seq = CFG_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            ".{}.tmp.{}.{seq}",
            file_name.to_string_lossy(),
            std::process::id()
        );
        let tmp_c = CString::new(tmp_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in temp name"))?;

        let raw_fd = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                tmp_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut tmp_file = File::from(unsafe { OwnedFd::from_raw_fd(raw_fd) });

        let write_res: io::Result<()> = (|| {
            tmp_file.write_all(serialized.as_bytes())?;
            if let Some((uid, gid)) = sudo_ids {
                let rc = unsafe { libc::fchown(tmp_file.as_raw_fd(), uid, gid) };
                if rc != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let rc = unsafe { libc::fchmod(tmp_file.as_raw_fd(), 0o644) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            tmp_file.sync_all()?;
            Ok(())
        })();

        if let Err(e) = write_res {
            drop(tmp_file);
            unsafe {
                libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
            }
            return Err(e.into());
        }
        drop(tmp_file);

        let dest_c = CString::new(file_name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in config filename"))?;
        let rc = unsafe {
            libc::renameat(
                dir_fd.as_raw_fd(),
                tmp_c.as_ptr(),
                dir_fd.as_raw_fd(),
                dest_c.as_ptr(),
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            unsafe {
                libc::unlinkat(dir_fd.as_raw_fd(), tmp_c.as_ptr(), 0);
            }
            return Err(err.into());
        }
        unsafe {
            libc::fsync(dir_fd.as_raw_fd());
        }
        Ok(())
    }
}

fn sudo_uid_gid() -> Option<(u32, u32)> {
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid: u32 = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    if uid == 0 { None } else { Some((uid, gid)) }
}

fn sudo_user_home() -> Option<PathBuf> {
    let user = std::env::var("SUDO_USER").ok()?;
    if user.is_empty() || user == "root" {
        return None;
    }
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let mut fields = line.split(':');
        let name = fields.next()?;
        if name == user {
            let home = fields.nth(4)?;
            if !home.is_empty() {
                return Some(PathBuf::from(home));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_default_config_toml() {
        let cfg = Config::default();
        let toml_str = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: Config = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let raw = r#"
            [general]
            language = "cs"
            [diff]
            show_metadata_only = true
        "#;
        let parsed: Config = toml::from_str(raw).expect("parse partial");
        assert_eq!(parsed.general.language, "cs");
        assert!(parsed.diff.show_metadata_only);
        assert_eq!(parsed.btrfs.live_strategy, LiveDiffStrategy::ReadOnlyFiemap);
        assert!(parsed.restore.create_backup);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let path = PathBuf::from("/tmp/snapshot-diff-nonexistent-test-config.toml");
        let _ = fs::remove_file(&path);
        let cfg = Config::load(&path).expect("load missing");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn save_and_reload_config() {
        let dir = std::env::temp_dir().join(format!("snapshot-diff-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.general.language = "cs".to_string();
        cfg.panels.hide_unchanged = true;
        cfg.save(&path).expect("save");
        let reloaded = Config::load(&path).expect("reload");
        assert_eq!(cfg, reloaded);
        let _ = fs::remove_dir_all(&dir);
    }
}
