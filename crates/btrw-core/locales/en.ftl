app-title = Btrfs Rewind
app-subtitle = Btrfs Snapshot Comparison & Recovery

timeline-header = Timeline
timeline-empty = No snapshots discovered — use --left / --right or run with sudo
source-live = LIVE
panel-left-tag = L
panel-right-tag = R

snapshot-count = { $count ->
    [one] { $count } snapshot
   *[other] { $count } snapshots
}

key-f1 = F1 Help
key-f2 = F2 Diff
key-f3 = F3 TimeTravel
key-f4 = F4 ExtDiff
key-f5 = F5 Restore
key-f6 = F6 SaveAs
key-f7 = F7 Filter
key-f8 = F8 Subvol
key-f9 = F9 Config
key-f10 = F10 Quit
key-src = 1/2 Source
key-hide = h Unchanged
key-swap = ^U Swap

status-ready = Ready — Tab pane, [ ] timeline, F8 subvol, F9 config, F10 quit
status-filter = Filter: { $query }
status-hidden-unchanged = [changes only]
status-errors = { $count } errors (press i)
status-sort = Sort: { $label }

sort-name-asc = name ↑
sort-name-desc = name ↓
sort-size-desc = size ↓
sort-size-asc = size ↑
sort-delta-desc = delta ↓
sort-delta-asc = delta ↑
sort-mtime-desc = mtime ↓
sort-mtime-asc = mtime ↑

config-title = Settings (F9) — saved automatically
config-hint = ↑↓ select  Enter/Space/←→ change  Esc close
config-saved = Configuration saved.
config-save-failed = Save failed: { $detail }
cfg-language = Language
cfg-theme = Theme
cfg-hide-unchanged = Hide unchanged files
cfg-show-size = Column: Size
cfg-show-delta = Column: Size delta
cfg-show-mtime = Column: Modified date (mtime)
cfg-show-permissions = Column: Permissions
perm-off = off
perm-octal = octal (0644)
perm-octal-owner = octal + owner:group
cfg-mouse = Mouse support (MC click / double-click)
cfg-show-metadata = Show metadata-only changes
cfg-create-backup = Restore: create .bak backup
cfg-live-strategy = Live state diff strategy
val-on = ON
val-off = OFF

info-title = Errors & Excluded Mountpoints (i)
info-hint = ↑↓ scroll  Esc/q close
info-errors-hdr = Errors ({ $count }):
info-excluded-hdr = Excluded mountpoints / nested subvolumes ({ $count }):
info-none = (none)

subvol-picker-title = Select Subvolume (F8 / g)
subvol-picker-hint = ↑↓ select  Enter confirm  Esc cancel
subvol-picker-hint-initial = ↑↓ select  Enter confirm  Esc/q quit

picker-title-left = Select Left Source (Alt+F1 / 1) | Total / Exclusive
picker-title-right = Select Right Source (Alt+F2 / 2) | Total / Exclusive
picker-hint = ↑↓ select  ←→/Enter expand/confirm  Space/s du sizes  Esc cancel
picker-du-running = Computing sizes { $done }/{ $total }… (Esc cancels)
picker-unknown-date = Undated snapshots

diff-title = Diff: { $path }
diff-hint = ↑↓ PgUp/PgDn scroll  ←→ horizontal  v unified/side-by-side  Esc/q back
diff-identical = File contents are identical.
diff-binary = Binary file — text diff is not available.
diff-too-large = File exceeds text diff size limit.
diff-too-many-lines = File exceeds text diff line limit.
diff-non-regular = Special file, directory, or symlink — text diff is not available.
diff-error = Unable to read file: { $detail }

diff-menu-title = Diff Scope & Export (F2)
diff-menu-dir = Current directory — flat change list
diff-menu-snapshot = Whole snapshot — flat change list
diff-menu-export = Export .patch for current view / selected file
diff-menu-tree = Return to MC directory tree
diff-menu-hint = ↑↓ select  Enter confirm  Esc cancel

export-title = Export Unified Patch (.patch)
export-scope-label = Scope:
export-scope-selected = Selected object ({ $name })
export-scope-dir = Current directory ({ $dir })
export-scope-snapshot = Whole snapshot
export-hint = Tab switch scope/dir/name  ←→ change/cursor  Enter open dir or export  Esc cancel
export-ok = Exported { $exported } file(s) to { $path } (skipped: { $skipped })
export-err = Patch export failed: { $detail }

extdiff-not-found = No external diff tool found (configure in F9 or set $DIFFPROG)
extdiff-need-file = Select a regular file on both sides for external diff (F4)
extdiff-too-large = File exceeds external diff staging limit ({ $limit })
extdiff-failed = External diff failed: { $detail }

cfg-diff-format = Default diff view
cfg-ext-diff = External diff command (F4)

flat-scanning = Scanning changes… { $scanned } entries ({ $path }) — Esc cancels
flat-mode-badge = [FLAT: { $scope }]
dir-chg-short = { $count } chg

tt-title = Time-Travel: { $path } ({ $versions } versions across { $total } snapshots)
tt-loading = Scanning history of { $path } across { $total } snapshots… (Esc cancels)
tt-empty = No readable versions found for this file across snapshots.
tt-hint = ←→ ver  Space pin (2=diff)  F2 export  F5/F6 restore  n/p chg  m ({ $mode })  Esc back
tt-need-two-pinned = Pin 2 versions with Space first to export their diff (F2).
tt-already-live = Active version is LIVE — select a snapshot version with ←→ to restore (F5).
tt-pinned-header = Diff between pinned: { $from } → { $to }
tt-mode-prev = vs prev
tt-mode-live = vs LIVE
tt-mode-none = clean

restore-title = CONFIRM FILE RESTORE (WRITE OPERATION)
restore-saveas-title = Save File From Snapshot As (F6)
restore-saveas-dir = Directory: { $dir }
restore-saveas-name = File name:
restore-saveas-target = Target: { $path }
restore-saveas-hint = Tab switch dir/name  ←→ cursor/nav  Enter open dir or confirm  Esc cancel
restore-confirm-hint = y CONFIRM WRITE   d preview diff   n / Esc cancel
restore-warn-overwrite = WARNING: Destination file EXISTS and will be OVERWRITTEN!
restore-warn-hardlink = NOTE: Destination has { $nlink } hardlinks — atomic restore will replace only this path (new inode).
restore-warn-new = Destination file does not exist — a new file will be created.
restore-label-source = Source snapshot:
restore-label-dest = Destination:
restore-label-src-meta = Source file:
restore-label-dst-meta = Current live:
restore-label-options = Options:
restore-opt-backup-on = .bak backup ON
restore-opt-backup-off = .bak backup OFF
restore-opt-reflink-auto = Btrfs reflink (FICLONE) AUTO
restore-opt-reflink-never = reflink OFF
restore-need-file = Select a regular file in a snapshot to restore (F5/F6).
restore-need-live = F5 requires a Btrfs subvolume target; use F6 (SaveAs) for custom dirs.
restore-ok = Restored to { $dest } ({ $info })
restore-err = Restore failed: { $detail }
restore-err-no-filename = destination path has no file name
restore-err-dest-dir = destination { $path } is a directory (refusing to overwrite)
restore-err-dest-symlink = destination { $path } is a symlink (refusing to overwrite)
restore-err-dest-not-regular = destination { $path } is not a regular file
restore-err-dest-inspect = cannot inspect destination { $path }: { $reason }
restore-err-perm-denied = permission denied
restore-err-inside-snapshots = refusing to write inside snapshots directory
restore-err-ro-subvol = refusing to write into read-only Btrfs subvolume: { $path }
restore-err-ro-fs = destination filesystem is mounted read-only: { $path }
restore-err-dest-symlink-generic = refusing to overwrite a symlink at destination
restore-err-dest-dir-generic = refusing to overwrite a directory at destination
restore-err-dest-special-generic = refusing to overwrite a special file at destination
restore-err-dest-changed = destination file changed since confirmation; please re-confirm
restore-err-src-changed = source file changed since confirmation; please retry
restore-err-mount-boundary = refusing F5 live restore across mountpoint or subvolume boundary

help-title = Keyboard Shortcuts (MC style)
help-body =
    Tab              Switch active pane (Left / Right)
    ↑ ↓ PgUp PgDn    Navigate entries (synchronized)
    Enter            Enter directory or view file diff
    Space            Async recursive dir summary (changed count + sizes, 4 Hz)
    F2 or f          Flat change list (current dir / whole snapshot / tree)
    Backspace        Go to parent directory
    1 / Alt+F1       Choose Left panel source (snapshot / LIVE)
    2 / Alt+F2       Choose Right panel source (snapshot / LIVE)
    F8 or g          Switch subvolume (resets both panels to LIVE)
    Ctrl+←/→ or [ ]  Move active panel along Timeline
    Shift+[ / ]      Jump active panel by day on Timeline
    Ctrl+U           Swap Left and Right sources
    F3               View diff for selected file
    F7 or /          Filter entries by name (Esc clears)
    h                Toggle hide unchanged files
    n / s / c / t    Sort by name / size / delta / mtime (repeat reverses)
    i                View errors & excluded mountpoints
    F9               Open settings (saved to config.toml)
    Mouse            Click to select, double-click to open/diff, wheel to scroll
    r                Reload current directory
    F10 or q         Quit

    Press Esc or Enter to close help.

err-config-read = Failed to read configuration file: { $detail }
err-config-write = Failed to write configuration file: { $detail }
err-config-parse = Invalid configuration syntax: { $detail }
status-loading-dir = Loading directory… (Esc cancel)
status-exporting-patch = Exporting patch… (Esc cancel)
status-worker-error = Background task terminated unexpectedly.
