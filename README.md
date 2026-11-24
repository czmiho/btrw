# Btrfs Rewind (`btrw`)

A fast, Midnight Commander–style dual-pane terminal UI for browsing, comparing, time-traveling through, and safely restoring files from **Btrfs snapshots**.

Designed for systems with frequent scheduled snapshots (hourly or more often), while staying completely agnostic to your snapshot tool — works out of the box with **`btrbk`**, **`snapper`**, **`timeshift`**, and manual `btrfs subvolume snapshot` setups, as well as any pair of plain directories.

```text
┌─ Timeline [/home] (48 snapshots) ─────────────────────────────────────────────┐
│ [L] 2026-11-23 14:00  ↔  [R] LIVE (/home)                                     │
│  · · · · ● · · │ · · · [L] · · · │ · · · ● · · · [R]                          │
├───────────────────────────────────────┬───────────────────────────────────────┤
│ [L] @home.20261123T1400 (11-23 14:00) │ [R] LIVE (/home) — /miho              │
│   * projects/                  1.2 GiB│   * projects/      1.2 GiB     3 chg  │
│   M .bashrc                   4.1 KiB │   M .bashrc        4.3 KiB    +214 B  │
│   D old-notes.md              1.8 KiB │   D —                   —   -1.8 KiB  │
│   A —                               — │   A todo.md          620 B    +620 B  │
├───────────────────────────────────────┴───────────────────────────────────────┤
│ -rw-r--r-- 1000:1000 2026-11-23 14:12  |  Sort: name ↑                        │
│ F1 Help  F2 Diff  F3 TimeTravel  F4 ExtDiff  F5 Restore  F6 SaveAs  F8 Subvol  F9 Config  F10 Quit │
└───────────────────────────────────────────────────────────────────────────────┘
```

---

## Features

* **Midnight Commander–style dual panes & mouse support**
  * Pick any snapshot or `LIVE` independently on the left and right panels, with synchronized directory navigation, sorting (`n`/`s`/`c`/`t`), filtering (`/`), and full mouse support (click to select, double-click to open/diff, scroll wheel).
  * Virtual nesting tree for all mounted Btrfs subvolumes (`F8` / `g`), locked cleanly inside the selected subvolume root.
* **Built for hundreds of snapshots**
  * Top timeline bar with day separators and quick stepping (`[` / `]`, `Shift+[` / `Shift+]`).
  * Collapsible **Day → Hour** snapshot picker (`1` / `2` or `Alt+F1` / `Alt+F2`) using true Btrfs creation timestamps (`otime`).
  * Non-blocking background `btrfs filesystem du` (`Total` / `Exclusive`) on demand (`Space` / `s`, cancellable with `Esc`).
* **Fast, accurate diffing**
  * **RO snapshot ↔ RO snapshot:** parses binary `btrfs send --no-data` directly in Rust (no `btrfs receive` writes, resolves `rename` and temporary nodes, orders of magnitude faster than tree-walking).
  * **RO snapshot ↔ `LIVE`:** strict read-only traversal (`O_NOATIME`, `O_NOFOLLOW`, skips mountpoints & nested subvolumes) with **`FS_IOC_FIEMAP`** physical extent comparison — zero unreliable `mtime` heuristics. Optional opt-in temporary RO snapshot mode in `F9` for atomic `btrfs send` against `LIVE`.
  * Built-in colored **Unified** and **Side-by-Side** (`v`) diff viewer, `.patch` export (`F2`), async recursive directory summaries (`Space`, live updates at ~4 Hz), and de-escalated external GUI diff (`F4` → `meld`, `kdiff3`, `code --diff`, `vimdiff`).
* **Time-Travel text file viewer (`F3`)**
  * Deduplicates a file's history across all snapshots in milliseconds using `stat` + `FIEMAP`, showing **only snapshots where the file actually changed**.
  * Step through versions with `←` / `→` with live change highlighting (`m`: vs previous / vs `LIVE` / clean), jump between changes (`n` / `p`), pin any 2 versions with `Space` to diff directly between them, export their `.patch` (`F2`), or restore any version (`F5` / `F6`).
* **Safe, atomic file recovery (`F5` / `F6`)**
  * Read-only everywhere except explicit file recovery, always behind a mandatory confirmation dialog requiring `y`.
  * Atomic temp file + `fsync` + `renameat`, instant zero-copy **Btrfs `FICLONE` reflink**, automatic `.bak.<timestamp>` backups, ownership/permission/xattr preservation, and hard blocks against writing into snapshots or clobbering symlinks/directories.
* **In-app configuration & localization**
  * Press `F9` to configure language (`en` / `cs` / `auto`), theme (`dark` / `mc_blue` / `terminal`), visible columns (size, delta, `mtime`, octal / `owner:group` permissions), mouse, diff format, external diff tool, backup behavior, and live diff strategy — saved automatically to `~/.config/btrw/config.toml`.

---

## Installation

### Pre-built `.deb` Package & Static Binary (Recommended)

Download the latest release from [GitHub Releases](https://github.com/czmiho/btrw/releases):

* **Universal `.deb` package (`btrw_<version>_amd64.deb`):** contains a **fully static `musl` binary** (`x86_64-unknown-linux-musl`) packaged with `xz` compression and **zero `glibc` or shared library dependencies** (`Recommends: btrfs-progs` only). Works out of the box across old and new `.deb` distributions alike — verified from **Debian 11 Bullseye** (`glibc 2.31`, kernel `5.10`) through **Ubuntu 24.04+**, Linux Mint, Pop!_OS, and even other Linux distros via direct extraction.
  ```bash
  sudo apt install ./btrw_0.1_amd64.deb
  ```
* **Standalone static tarball (`btrw-<version>-x86_64-linux-musl.tar.gz`):** works on any `x86_64` Linux distribution (Arch, Fedora, openSUSE, Alpine, etc.) — simply extract `btrw` into `/usr/local/bin/`.

### Building from Source

* **Linux** with `btrfs-progs` (`btrfs` in `$PATH`)
* **Rust** toolchain (`cargo`, `rustc`)

```bash
git clone https://github.com/czmiho/btrw.git
cd btrw
cargo build --release --workspace
```

The standalone binary is produced at `target/release/btrw` (zero external runtime dependencies).

---

## Usage

```bash
# Auto-discover mounted Btrfs subvolumes and snapshots (English default)
sudo ./target/release/btrw

# Run in Czech
sudo ./target/release/btrw --lang cs

# Explicit snapshot directory (e.g. for unmounted subvolumes or custom mount)
sudo ./target/release/btrw --snapshots-dir /mnt/btrfs-top/@snapshots

# Compare any two plain directories (no Btrfs or sudo required)
./target/release/btrw --left /path/to/old --right /path/to/new

# Inspect resolved TOML configuration
./target/release/btrw --print-config
```

### Automatic Btrfs Discovery

By default (`snapshots_dir = ""`), `btrw` reads `/proc/self/mountinfo` to discover all mounted Btrfs subvolumes and snapshot containers (`@snapshots`, `.snapshots`, `snapshots` under top-level `subvol=/` mounts, directly mounted `/.snapshots`, and `<mount>/.snapshots`). Snapshots are matched to their live subvolumes via kernel Btrfs **`parent_uuid == uuid`** metadata, with longest-prefix name matching as a fallback.

### Permissions & When `sudo` Is Needed

**Running without `sudo` works when:**
* Comparing any two readable directories (`--left <dir1> --right <dir2>`), including non-Btrfs filesystems.
* Browsing and diffing Btrfs snapshots whose container directory (e.g. `~/.snapshots` or a user-readable `@snapshots`) and target files are readable by your user — including `FS_IOC_FIEMAP` extent comparison, `F3` Time-Travel, `F2` `.patch` export, `F4` external diff, and `F5`/`F6` restore to destinations you have write access to.
* Note: without root privileges, `btrw` automatically falls back to filesystem traversal instead of `btrfs send --no-data`, cannot inspect `BTRFS_IOC_TREE_SEARCH` (compressed-extent shortcut) or privileged Btrfs subvolume metadata if blocked by permissions, and cannot read root-only directories (such as Snapper's default `0750 root:root` `/.snapshots`).

**Running with `sudo` is required for:**
* **Fast RO ↔ RO diffing via `btrfs send --no-data`** (requires `CAP_SYS_ADMIN`) and optional **temporary RO snapshots** (`F9` → *Live diff strategy*) for atomic `btrfs send` against `LIVE`.
* **Accessing restricted snapshot roots and system subvolumes** (e.g. `/.snapshots`, `/mnt/btrfs-top/@snapshots`, `/etc`, `/var`) and reading kernel Btrfs metadata (`BTRFS_IOC_GET_SUBVOL_INFO` / `BTRFS_IOC_TREE_SEARCH` for `parent_uuid` matching, `otime`, and zero-read compressed extent comparison).
* **Computing exclusive snapshot sizes** (`btrfs filesystem du` in the `1`/`2` picker).
* **Restoring files (`F5`/`F6`)** to system paths or preserving original `uid:gid` ownership across users.

**Safety when running under `sudo`:**
* External GUI diff tools (`F4`) are automatically **de-escalated** back to `SUDO_UID:SUDO_GID` (with isolated temporary staging only when a snapshot file is unreadable to the invoking user).
* Saved settings (`F9` → `config.toml`) and exported `.patch` files (`F2`) are chowned to `SUDO_UID:SUDO_GID` rather than leaving root-owned files in your home directory.

---

## Keyboard & Mouse Controls

### Main Dual-Pane View

| Key / Action | Description |
| :--- | :--- |
| `Tab` | Switch active panel (Left ↔ Right) |
| `↑` `↓` `PgUp` `PgDn` | Navigate entries (synchronized across both panes) |
| `Enter` / Double-click | Enter directory or open file diff |
| `Backspace` | Go to parent directory (locked inside subvolume root) |
| `Space` *(on directory)* | Async recursive summary: changed file count + sizes on both sides (~4 Hz live) |
| `1` / `Alt+F1` | Pick Left panel source (Day → Hour picker; `Space`/`s` computes `btrfs du`) |
| `2` / `Alt+F2` | Pick Right panel source |
| `[` / `]` *(or `Ctrl+←`/`→`)* | Step active panel along the Timeline |
| `{` / `}` *(or `Shift+[`/`]`)* | Jump active panel by day on the Timeline |
| `Ctrl+U` | Swap Left and Right sources |
| `F1` | Keyboard shortcuts help |
| `F2` / `f` | Diff menu (export `.patch`, flat change list for current dir / whole snapshot) |
| `F3` | Open **Time-Travel** history viewer for selected file |
| `F4` | Open selected file pair in external GUI diff (de-escalated to `SUDO_UID`) |
| `F5` | Restore selected snapshot file to its original `LIVE` path (confirmation required) |
| `F6` | Save selected snapshot file to any directory/name (directory tree + cursor editor) |
| `F7` or `/` | Filter entries by name (`Esc` clears) |
| `F8` or `g` | Switch Btrfs subvolume (virtual nesting tree, resets both panels to `LIVE`) |
| `F9` | Open in-app Settings (`config.toml`) |
| `n` / `s` / `c` / `t` | Sort by name / size / delta / modified date (repeat reverses) |
| `h` | Toggle hide unchanged files |
| `i` | Show read errors and excluded mountpoints / nested subvolumes |
| `F10` or `q` | Quit |

### Time-Travel Viewer (`F3`)

| Key | Description |
| :--- | :--- |
| `←` / `→` *(or `[` / `]`)* | Step to previous / next changing snapshot |
| `Space` | Pin / unpin version (`★`); pinning **2 versions** shows the diff directly between them |
| `n` / `p` | Jump to next / previous changed line |
| `m` | Cycle highlight mode (*vs previous* → *vs LIVE* → *clean text*) |
| `F2` / `e` | Export `.patch` between the 2 pinned versions |
| `F5` / `F6` | Restore or Save As the currently viewed snapshot version |
| `Esc` / `q` / `F3` | Return to panels |

---

## Development & Testing

The repository is structured as a Rust Cargo workspace:

* `crates/btrw-core` — Btrfs discovery, safe FS/ioctl layer, `FIEMAP`, `btrfs send --no-data` binary TLV parser, diff/patch generation, Time-Travel engine, atomic restore, TOML config, Fluent i18n.
* `crates/btrw` — `ratatui` + `crossterm` Midnight Commander–style terminal frontend (`btrw` binary).

```bash
cargo test --workspace
```

## License

Licensed under the **GNU General Public License v3.0 or later** (`GPL-3.0-or-later`). See [LICENSE](LICENSE) for the full text.
