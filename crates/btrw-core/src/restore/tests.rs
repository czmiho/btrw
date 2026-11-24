use super::*;
use crate::config::RestoreConfig;
use crate::fs_safe::open_file_safe;
use std::path::{Path, PathBuf};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("snapshot-diff-restore-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn restores_new_file_atomically_and_preserves_mode() {
        let base = test_dir("new");
        let snaps = base.join("snaps");
        let snap = snaps.join("s1");
        let live = base.join("live");
        fs::create_dir_all(&snap).unwrap();
        fs::create_dir_all(&live).unwrap();

        let src = snap.join("script.sh");
        fs::write(&src, "#!/bin/sh\necho ok\n").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();

        let dest = live.join("script.sh");
        let cfg = RestoreConfig::default();
        let out = restore_file(&snap, Path::new("script.sh"), &dest, &snaps, &cfg).expect("restore");
        assert!(out.backup_path.is_none());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho ok\n");
        assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o750);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn overwrites_existing_file_with_backup() {
        let base = test_dir("bak");
        let snaps = base.join("snaps");
        let snap = snaps.join("s1");
        let live = base.join("live");
        fs::create_dir_all(&snap).unwrap();
        fs::create_dir_all(&live).unwrap();

        fs::write(snap.join("cfg.toml"), "version = 1\n").unwrap();
        let dest = live.join("cfg.toml");
        fs::write(&dest, "version = 2\n").unwrap();

        let cfg = RestoreConfig::default();
        let out = restore_file(&snap, Path::new("cfg.toml"), &dest, &snaps, &cfg).expect("restore");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "version = 1\n");
        let bak = out.backup_path.expect("backup created");
        assert_eq!(fs::read_to_string(&bak).unwrap(), "version = 2\n");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_write_into_snapshots_dir_and_refuses_symlink_clobber() {
        let base = test_dir("safety");
        let snaps = base.join("snaps");
        let snap = snaps.join("s1");
        let live = base.join("live");
        fs::create_dir_all(&snap).unwrap();
        fs::create_dir_all(&live).unwrap();

        fs::write(snap.join("a.txt"), "safe").unwrap();
        let cfg = RestoreConfig::default();

        // 1. Refuse writing inside snapshots_dir.
        let err1 = restore_file(&snap, Path::new("a.txt"), &snap.join("b.txt"), &snaps, &cfg)
            .unwrap_err();
        assert!(err1.contains("snapshots directory"));

        // 2. Refuse overwriting a symlink at destination.
        let victim = live.join("victim.txt");
        fs::write(&victim, "untouched").unwrap();
        let link = live.join("a.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let err2 = restore_file(&snap, Path::new("a.txt"), &link, &snaps, &cfg).unwrap_err();
        assert!(err2.contains("symlink"));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");

        // 3. Refuse overwriting a directory.
        let dir_dest = live.join("sub");
        fs::create_dir_all(&dir_dest).unwrap();
        let err3 = restore_file(&snap, Path::new("a.txt"), &dir_dest, &snaps, &cfg).unwrap_err();
        assert!(err3.contains("directory"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_snapshots_dir_allows_restore_and_handles_no_backup_and_backup_collision() {
        let base = test_dir("opts");
        let snap = base.join("snap");
        let live = base.join("live");
        fs::create_dir_all(&snap).unwrap();
        fs::create_dir_all(&live).unwrap();

        fs::write(snap.join("f.txt"), "v1").unwrap();
        let dest = live.join("f.txt");
        fs::write(&dest, "v2").unwrap();

        // Empty snapshots_dir (auto-discovery mode) must NOT block writing to live.
        let mut cfg = RestoreConfig::default();
        cfg.create_backup = false;
        let out = restore_file(&snap, Path::new("f.txt"), &dest, Path::new(""), &cfg)
            .expect("restore without backup");
        assert!(out.backup_path.is_none());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "v1");

        // Two rapid overwrites with backup=true must produce two distinct backup files (collision _1).
        cfg.create_backup = true;
        fs::write(&dest, "v2").unwrap();
        let out1 = restore_file(&snap, Path::new("f.txt"), &dest, Path::new(""), &cfg).unwrap();
        fs::write(&dest, "v3").unwrap();
        let out2 = restore_file(&snap, Path::new("f.txt"), &dest, Path::new(""), &cfg).unwrap();
        let b1 = out1.backup_path.expect("bak 1");
        let b2 = out2.backup_path.expect("bak 2");
        assert_ne!(b1, b2);
        assert_eq!(fs::read_to_string(&b1).unwrap(), "v2");
        assert_eq!(fs::read_to_string(&b2).unwrap(), "v3");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_checked_rejects_toctou_dest_appearance_and_modification() {
        let base = test_dir("toctou");
        let snap = base.join("snap");
        let live = base.join("live");
        fs::create_dir_all(&snap).unwrap();
        fs::create_dir_all(&live).unwrap();
        fs::write(snap.join("x.txt"), "from-snap").unwrap();
        let (_, src_sig) = open_file_safe(&snap, Path::new("x.txt")).unwrap();
        let dest = live.join("x.txt");

        // Case 1: Plan expected missing dest, then dest appeared before confirmation.
        let plan_new = RestorePlan {
            source_root: snap.clone(),
            rel_path: PathBuf::from("x.txt"),
            source_label: "snap".into(),
            dest_path: dest.clone(),
            source_sig: src_sig,
            dest_sig: None,
            expected_live_root: None,
            expected_live_root_sig: None,
        };
        fs::write(&dest, "appeared-concurrently").unwrap();
        let err1 = restore_file_checked(&plan_new, Path::new(""), &RestoreConfig::default())
            .unwrap_err();
        assert!(err1.contains("changed"));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "appeared-concurrently");

        // Case 2: Plan recorded dest sig, then dest was modified before confirmation.
        let (_, old_dest_sig) = open_file_safe(&live, Path::new("x.txt")).unwrap();
        let plan_ovw = RestorePlan {
            dest_sig: Some(old_dest_sig),
            ..plan_new
        };
        fs::write(&dest, "modified-concurrently-longer").unwrap();
        let err2 = restore_file_checked(&plan_ovw, Path::new(""), &RestoreConfig::default())
            .unwrap_err();
        assert!(err2.contains("changed"));
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            "modified-concurrently-longer"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_checked_with_protected_blocks_auto_discovered_snapshot_roots() {
        let base = test_dir("auto-protected");
        let snap_root = base.join("@snapshots");
        let snap = snap_root.join("timed").join("@root.meta2");
        fs::create_dir_all(&snap).unwrap();
        fs::write(snap.join("meta-only.txt"), "hello").unwrap();
        let (_, src_sig) = open_file_safe(&snap, Path::new("meta-only.txt")).unwrap();

        let dest = snap_root.join("meta-only.txt");
        let plan = RestorePlan {
            source_root: snap.clone(),
            rel_path: PathBuf::from("meta-only.txt"),
            source_label: "timed/@root.meta2".into(),
            dest_path: dest.clone(),
            source_sig: src_sig,
            dest_sig: None,
            expected_live_root: None,
            expected_live_root_sig: None,
        };
        let protected = vec![snap_root.clone()];
        let err = restore_file_checked_with_protected(&plan, &protected, &RestoreConfig::default())
            .unwrap_err();
        assert!(err.contains("snapshots directory"));
        assert!(!dest.exists(), "must not write inside auto-discovered @snapshots");

        let _ = fs::remove_dir_all(&base);
    }
