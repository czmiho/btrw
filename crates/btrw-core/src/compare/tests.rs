use super::*;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;
    use std::fs;

    #[test]
    fn compares_directories_and_diffs_modified_file() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-cmp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(left.join("sub")).unwrap();
        fs::create_dir_all(right.join("sub")).unwrap();

        fs::write(left.join("same.txt"), "hello\n").unwrap();
        fs::write(right.join("same.txt"), "hello\n").unwrap();
        fs::write(left.join("mod.txt"), "line1\nold\n").unwrap();
        fs::write(right.join("mod.txt"), "line1\nnew\n").unwrap();
        fs::write(left.join("deleted.txt"), "bye\n").unwrap();
        fs::write(right.join("added.txt"), "hi!\n").unwrap();

        let result = compare_directory(&left, &right, Path::new(""), false);
        assert!(result.errors.is_empty());

        // Directories first
        assert_eq!(result.rows[0].name, "sub");
        assert!(result.rows[0].is_dir());

        let find = |name: &str| result.rows.iter().find(|r| r.name == name).unwrap();
        assert_eq!(find("same.txt").status, None);
        assert_eq!(find("mod.txt").status, Some(ChangeKind::Modified));
        assert_eq!(find("deleted.txt").status, Some(ChangeKind::Deleted));
        assert_eq!(find("added.txt").status, Some(ChangeKind::Added));

        match diff_file(&left, &right, find("mod.txt"), 1024 * 1024, 1000) {
            FileDiffOutcome::Unified(lines) => {
                assert!(lines.iter().any(|l| l.kind == DiffLineKind::Removed && l.text == "-old"));
                assert!(lines.iter().any(|l| l.kind == DiffLineKind::Added && l.text == "+new"));
            }
            other => panic!("expected unified diff, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scans_recursive_dir_stats_and_flat_changes() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-rec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(left.join("a/b")).unwrap();
        fs::create_dir_all(right.join("a/b")).unwrap();

        fs::write(left.join("a/same.txt"), "12345").unwrap();
        fs::write(right.join("a/same.txt"), "12345").unwrap();
        fs::write(left.join("a/b/mod.txt"), "old").unwrap();
        fs::write(right.join("a/b/mod.txt"), "newer!").unwrap();
        fs::write(right.join("a/b/add.txt"), "abcd").unwrap();

        let cancel = AtomicBool::new(false);
        let stats = scan_directory_stats(&left, &right, Path::new("a"), &cancel, |_| {});
        assert!(stats.done);
        assert_eq!(stats.changed_count, 2);
        assert_eq!(stats.left_bytes, 8); // 5 + 3
        assert_eq!(stats.right_bytes, 15); // 5 + 6 + 4

        let flat = scan_flat_changes(&left, &right, Path::new(""), false, &cancel, |_, _| {});
        assert_eq!(flat.rows.len(), 2);
        assert!(flat.rows.iter().any(|r| r.name == "a/b/mod.txt"));
        assert!(flat.rows.iter().any(|r| r.name == "a/b/add.txt"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn side_by_side_export_patch_sorting_type_change_and_ansi_escape() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-ext-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(left.join("zdir")).unwrap();
        fs::create_dir_all(right.join("zdir")).unwrap();

        fs::write(left.join("text.txt"), "a\nold\nc\n").unwrap();
        fs::write(right.join("text.txt"), "a\nnew\nc\nd\n").unwrap();
        fs::write(left.join("bin.dat"), b"\x00\x01\x02").unwrap();
        fs::write(right.join("bin.dat"), b"\x00\x01\x03\x04").unwrap();
        // Type change: regular file on left, symlink on right.
        fs::write(left.join("flip"), "file").unwrap();
        std::os::unix::fs::symlink("target", right.join("flip")).unwrap();

        let mut cmp = compare_directory(&left, &right, Path::new(""), false);
        let flip = cmp.rows.iter().find(|r| r.name == "flip").unwrap();
        assert_eq!(flip.status, Some(ChangeKind::TypeChanged));

        // Side-by-side diff.
        let txt = cmp.rows.iter().find(|r| r.name == "text.txt").unwrap();
        let srows = diff_file_side_by_side(&left, &right, txt, 1024 * 1024, 1000).expect("sbs");
        assert!(srows.iter().any(|r| r.kind == SideRowKind::Changed && r.left_text == "old" && r.right_text == "new"));
        assert!(srows.iter().any(|r| r.kind == SideRowKind::Added && r.right_text == "d"));

        // Export patch: exports text.txt, skips bin.dat and flip.
        let patch_path = base.join("out.patch");
        let summary = export_patch(&left, &right, &cmp.rows, &patch_path, 1024 * 1024, 1000).unwrap();
        assert_eq!(summary.files_exported, 1);
        assert_eq!(summary.files_skipped, 2);
        let patch_text = fs::read_to_string(&patch_path).unwrap();
        assert!(patch_text.contains("-old"));
        assert!(patch_text.contains("+new"));

        // Sorting keeps directories first in every sort order.
        for order in [
            SortOrder::NameAsc,
            SortOrder::NameDesc,
            SortOrder::SizeDesc,
            SortOrder::SizeAsc,
            SortOrder::DeltaDesc,
            SortOrder::DeltaAsc,
            SortOrder::MtimeDesc,
            SortOrder::MtimeAsc,
        ] {
            sort_pane_rows(&mut cmp.rows, order);
            assert_eq!(cmp.rows[0].name, "zdir", "dir must stay first for {order:?}");
        }

        // ANSI control escape sanitization.
        let sanitized = escape_control_chars("ok\x1b[31mred\tX");
        assert!(!sanitized.contains('\x1b'));
        assert!(sanitized.contains("    X"));

        // Raw patch preserves real tabs and refuses to overwrite an existing patch file.
        fs::write(left.join("Makefile"), "all:\n\techo old\n").unwrap();
        fs::write(right.join("Makefile"), "all:\n\techo new\n").unwrap();
        let cmp2 = compare_directory(&left, &right, Path::new(""), false);
        let raw_patch = base.join("tabs.patch");
        export_patch(&left, &right, &cmp2.rows, &raw_patch, 1024 * 1024, 1000).unwrap();
        let raw_text = fs::read_to_string(&raw_patch).unwrap();
        assert!(raw_text.contains("-\techo old\n"), "raw patch must keep literal tabs");
        assert!(raw_text.contains("+\techo new\n"), "raw patch must keep literal tabs");
        // Second export to the same path must fail (refuses silent overwrite).
        assert!(export_patch(&left, &right, &cmp2.rows, &raw_patch, 1024 * 1024, 1000).is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn non_utf8_distinct_filenames_are_preserved_without_collision() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let base = std::env::temp_dir().join(format!("snapshot-diff-nonutf8-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();

        let n1 = OsStr::from_bytes(b"bad\xFF.txt");
        let n2 = OsStr::from_bytes(b"bad\xFE.txt");
        fs::write(left.join(n1), "one").unwrap();
        fs::write(right.join(n1), "one_mod").unwrap();
        fs::write(right.join(n2), "two").unwrap();

        let cmp = compare_directory(&left, &right, Path::new(""), false);
        assert_eq!(cmp.rows.len(), 2, "two distinct non-UTF-8 byte names must not collide");
        assert!(cmp.rows.iter().any(|r| r.rel_path.as_os_str() == n1 && r.status == Some(ChangeKind::Modified)));
        assert!(cmp.rows.iter().any(|r| r.rel_path.as_os_str() == n2 && r.status == Some(ChangeKind::Added)));
        assert_ne!(cmp.rows[0].name, cmp.rows[1].name, "hex-escaped display names must differ");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn excluded_mountpoint_skipped_on_both_sides_in_recursive_scans() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-excl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(left.join("mnt_sub")).unwrap();
        fs::create_dir_all(right.join("mnt_sub")).unwrap();
        fs::write(left.join("mnt_sub/inside.txt"), "should_be_ignored").unwrap();
        fs::write(left.join("real.txt"), "old").unwrap();
        fs::write(right.join("real.txt"), "new_data").unwrap();

        let mut mounts = HashSet::new();
        mounts.insert(right.join("mnt_sub"));

        let cancel = AtomicBool::new(false);
        let stats =
            scan_directory_stats_with_mounts(&left, &right, Path::new(""), &mounts, &cancel, |_| {});
        assert_eq!(stats.changed_count, 1, "excluded mountpoint must not count as deleted");
        assert_eq!(stats.left_bytes, 3);
        assert_eq!(stats.right_bytes, 8);

        let flat = scan_flat_changes_with_mounts(
            &left,
            &right,
            Path::new(""),
            false,
            &mounts,
            &cancel,
            |_, _| {},
        );
        assert_eq!(flat.rows.len(), 1);
        assert_eq!(flat.rows[0].name, "real.txt");
        assert!(flat.excluded.iter().any(|e| e == "mnt_sub"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn show_metadata_only_toggles_mode_change_detection() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("snapshot-diff-meta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(left.join("script.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::write(right.join("script.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(left.join("script.sh"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(right.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();

        let cmp_off = compare_directory(&left, &right, Path::new(""), false);
        assert_eq!(cmp_off.rows[0].status, None);

        let cmp_on = compare_directory(&left, &right, Path::new(""), true);
        assert_eq!(cmp_on.rows[0].status, Some(ChangeKind::MetadataOnly));

        let cancel = AtomicBool::new(false);
        let flat_off = scan_flat_changes(&left, &right, Path::new(""), false, &cancel, |_, _| {});
        assert!(flat_off.rows.is_empty());

        let flat_on = scan_flat_changes(&left, &right, Path::new(""), true, &cancel, |_, _| {});
        assert_eq!(flat_on.rows.len(), 1);
        assert_eq!(flat_on.rows[0].status, Some(ChangeKind::MetadataOnly));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn type_changed_dir_to_file_has_zero_errors_in_recursive_scans() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-tc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(left.join("was_dir")).unwrap();
        fs::write(left.join("was_dir/inside.txt"), "hello").unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(right.join("was_dir"), "now_a_file").unwrap();

        let cancel = AtomicBool::new(false);
        let stats = scan_directory_stats(&left, &right, Path::new(""), &cancel, |_| {});
        assert!(stats.errors.is_empty(), "TypeChanged dir->file must not emit ENOTDIR error: {:?}", stats.errors);

        let flat = scan_flat_changes(&left, &right, Path::new(""), false, &cancel, |_, _| {});
        assert!(flat.errors.is_empty(), "TypeChanged dir->file must not emit ENOTDIR error: {:?}", flat.errors);
        assert!(flat.rows.iter().any(|r| r.name == "was_dir" && r.status == Some(ChangeKind::TypeChanged)));
        assert!(flat.rows.iter().any(|r| r.name == "was_dir/inside.txt" && r.status == Some(ChangeKind::Deleted)));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn exported_patch_is_0600_and_applies_with_crlf_no_lf_and_empty_file() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("snapshot-diff-apply-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        let work = base.join("work");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::create_dir_all(&work).unwrap();

        // CRLF + tab + missing trailing LF + non-empty added + non-empty deleted + empty added.
        fs::write(left.join("crlf.txt"), "a\r\n\told\r\nno_lf").unwrap();
        fs::write(right.join("crlf.txt"), "a\r\n\tnew\r\nno_lf_2").unwrap();
        fs::write(left.join("gone.txt"), "bye\n").unwrap();
        fs::write(right.join("added.txt"), "hello new\n").unwrap();
        fs::write(right.join("empty.txt"), "").unwrap();

        fs::write(work.join("crlf.txt"), "a\r\n\told\r\nno_lf").unwrap();
        fs::write(work.join("gone.txt"), "bye\n").unwrap();

        let cmp = compare_directory(&left, &right, Path::new(""), false);
        let patch_path = base.join("full.patch");
        let summary = export_patch(&left, &right, &cmp.rows, &patch_path, 1024 * 1024, 1000).unwrap();
        assert_eq!(summary.files_exported, 4);
        let mode = fs::metadata(&patch_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "exported patch must have private 0600 permissions");

        // Apply with `git apply` inside `work` and verify the ENTIRE resulting directory tree!
        let status = std::process::Command::new("git")
            .current_dir(&work)
            .args(["apply", "--unsafe-paths"])
            .arg(&patch_path)
            .status();
        if let Ok(st) = status {
            assert!(st.success(), "git apply must succeed on exported patch");
            assert_eq!(
                fs::read(work.join("crlf.txt")).unwrap(),
                fs::read(right.join("crlf.txt")).unwrap(),
                "applied CRLF/tab/no-LF bytes must match exactly"
            );
            assert_eq!(
                fs::read(work.join("added.txt")).unwrap(),
                b"hello new\n",
                "non-empty added file must be created with exact bytes"
            );
            assert!(!work.join("gone.txt").exists(), "deleted file must be removed");
            assert!(!work.join("dev").exists(), "git apply must NOT create stray dev/null");
            assert!(work.join("empty.txt").exists(), "empty added file must be created");
            assert_eq!(fs::read(work.join("empty.txt")).unwrap(), b"");
        }

        let _ = fs::remove_dir_all(&base);
    }
