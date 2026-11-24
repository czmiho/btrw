use super::*;
use btrw_core::config::TimeTravelHighlight;
use btrw_core::fs_safe::{format_os_display, open_file_safe};
use btrw_core::model::{SnapshotKind, Source};
use btrw_core::restore::RestorePlan;
use btrw_core::timetravel::TimeTravelLineKind;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
    use btrw_core::fs_safe::StatSig;
    use btrw_core::model::SnapshotInfo;
    use btrw_core::timetravel::{FileVersionMeta, TimeTravelHistory, TimeTravelLine};
    use std::time::{Duration, UNIX_EPOCH};

    fn dummy_sig() -> StatSig {
        StatSig {
            dev: 1,
            ino: 2,
            nlink: 1,
            mode: libc::S_IFREG | 0o644,
            uid: 1000,
            gid: 1000,
            size: 10,
            mtime_sec: 1_700_000_000,
            mtime_nsec: 0,
            ctime_sec: 1_700_000_000,
            ctime_nsec: 0,
        }
    }

    #[test]
    fn saveas_utf8_cursor_editing_and_validation() {
        let mut st = SaveAsState {
            source_root: PathBuf::from("/snap"),
            source_label: "s1".to_string(),
            rel_path: PathBuf::from("článek.txt"),
            source_sig: dummy_sig(),
            current_dir: PathBuf::from("/tmp"),
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: "článek.txt".to_string(),
            original_os_name: None,
            filename_edited: false,
            cursor: "článek.txt".chars().count(),
            focus: SaveAsFocus::Filename,
        };

        // Move left 4 chars (before ".txt"), insert "_ř", verify UTF-8 integrity.
        for _ in 0..4 {
            st.cursor_left();
        }
        st.insert_char('_');
        st.insert_char('ř');
        assert_eq!(st.filename, "článek_ř.txt");

        // Reject path separators inside filename.
        st.insert_char('/');
        assert_eq!(st.filename, "článek_ř.txt");

        // Home + Delete removes 'č'.
        st.cursor_home();
        st.delete_char();
        assert_eq!(st.filename, "lánek_ř.txt");

        // End + Backspace removes 't'.
        st.cursor_end();
        st.backspace();
        assert_eq!(st.filename, "lánek_ř.tx");
        assert_eq!(st.full_dest_path(), Some(PathBuf::from("/tmp/lánek_ř.tx")));

        // Invalid names ("..", "") return None.
        st.filename = "..".to_string();
        assert_eq!(st.full_dest_path(), None);
        st.filename = "   ".to_string();
        assert_eq!(st.full_dest_path(), None);
    }

    #[test]
    fn timetravel_space_pinning_and_jump_change() {
        let mk_ver = |idx: usize, label: &str| FileVersionMeta {
            timeline_idx: idx,
            root_path: PathBuf::from(format!("/s{idx}")),
            label: label.to_string(),
            timestamp: None,
            is_live: idx == 3,
            verified_parent: true,
            exists: true,
            size: Some(10),
            error: None,
        };
        let mut tt = TimeTravelState {
            history: TimeTravelHistory {
                rel_path: PathBuf::from("a.txt"),
                total_sources_checked: 10,
                versions: vec![
                    mk_ver(0, "s0"),
                    mk_ver(1, "s1"),
                    mk_ver(2, "s2"),
                    mk_ver(3, "LIVE"),
                ],
                errors: Vec::new(),
            },
            group_idx: None,
            verified_live_target: None,
            active_idx: 2,
            pinned: Vec::new(),
            highlight: TimeTravelHighlight::PreviousVersion,
            content_cache: HashMap::new(),
            rendered: vec![
                TimeTravelLine { line_no: Some(1), kind: TimeTravelLineKind::Context, text: "a".into() },
                TimeTravelLine { line_no: Some(2), kind: TimeTravelLineKind::Context, text: "b".into() },
                TimeTravelLine { line_no: Some(3), kind: TimeTravelLineKind::Context, text: "c".into() },
                TimeTravelLine { line_no: Some(4), kind: TimeTravelLineKind::Context, text: "d".into() },
                TimeTravelLine { line_no: Some(5), kind: TimeTravelLineKind::Added, text: "chg1".into() },
                TimeTravelLine { line_no: Some(6), kind: TimeTravelLineKind::Context, text: "e".into() },
                TimeTravelLine { line_no: Some(7), kind: TimeTravelLineKind::Context, text: "f".into() },
                TimeTravelLine { line_no: Some(8), kind: TimeTravelLineKind::Context, text: "g".into() },
                TimeTravelLine { line_no: Some(9), kind: TimeTravelLineKind::Removed, text: "chg2".into() },
            ],
            scroll_y: 0,
            scroll_x: 0,
        };

        // Pin 2, then 0 -> sorted pair (0, 2).
        tt.toggle_pin();
        assert_eq!(tt.pinned, vec![2]);
        assert_eq!(tt.pinned_pair(), None);
        tt.active_idx = 0;
        tt.toggle_pin();
        assert_eq!(tt.pinned_pair(), Some((0, 2)));

        // Pinning a 3rd replaces oldest of pair -> sorted (2, 3).
        tt.active_idx = 3;
        tt.toggle_pin();
        assert_eq!(tt.pinned_pair(), Some((2, 3)));

        // Unpinning 3 leaves [2].
        tt.toggle_pin();
        assert_eq!(tt.pinned, vec![2]);
        assert_eq!(tt.pinned_pair(), None);

        // Jump forward to first change (index 4 -> scroll_y = 1), then second (index 8 -> scroll_y = 5).
        tt.jump_change(true);
        assert_eq!(tt.scroll_y, 1);
        tt.jump_change(true);
        assert_eq!(tt.scroll_y, 5);
    }

    #[test]
    fn picker_groups_snapshots_by_day_newest_first() {
        let catalog = Catalog::new(Locale::En);
        let day1 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let day2 = day1 + Duration::from_secs(86_400);
        let timeline = vec![
            Source::Snapshot(SnapshotInfo {
                path: PathBuf::from("/snaps/s1"),
                live_target: PathBuf::from("/"),
                label: "s1".into(),
                created_at: Some(day1),
                kind: SnapshotKind::Scheduled,
                read_only: true,
                verified_parent: true,
            }),
            Source::Snapshot(SnapshotInfo {
                path: PathBuf::from("/snaps/s2"),
                live_target: PathBuf::from("/"),
                label: "s2".into(),
                created_at: Some(day2),
                kind: SnapshotKind::Event,
                read_only: true,
                verified_parent: true,
            }),
            Source::Live {
                path: PathBuf::from("/"),
                label: "LIVE (/)".into(),
            },
        ];

        // Collapsed: LIVE + 2 day headers.
        let collapsed = build_picker_entries_from(&timeline, &catalog, &BTreeSet::new());
        assert_eq!(collapsed.len(), 3);
        assert!(matches!(collapsed[0], PickerEntry::Live { .. }));
        assert!(matches!(collapsed[1], PickerEntry::Day { count: 1, expanded: false, .. }));

        // Expand newest day -> 4 items total.
        let newest_day = match &collapsed[1] {
            PickerEntry::Day { day_key, .. } => day_key.clone(),
            _ => unreachable!(),
        };
        let mut expanded_set = BTreeSet::new();
        expanded_set.insert(newest_day);
        let expanded = build_picker_entries_from(&timeline, &catalog, &expanded_set);
        assert_eq!(expanded.len(), 4);
        assert!(matches!(expanded[2], PickerEntry::Snapshot { timeline_idx: 1, .. }));
    }

    #[test]
    fn saveas_and_export_reload_subdirs_with_empty_snapshots_dir() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-subdirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("alpha")).unwrap();
        std::fs::create_dir_all(base.join("beta")).unwrap();

        let empty: &[PathBuf] = &[];
        let mut save = SaveAsState {
            source_root: base.clone(),
            source_label: "s".into(),
            rel_path: PathBuf::from("a.txt"),
            source_sig: StatSig::default(),
            current_dir: base.clone(),
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: "a.txt".into(),
            original_os_name: None,
            filename_edited: false,
            cursor: 5,
            focus: SaveAsFocus::DirTree,
        };
        save.reload_subdirs(empty);
        assert_eq!(save.subdirs.len(), 2);

        let mut exp = ExportPatchState {
            scope: ExportScope::CurrentDir,
            selected_row: None,
            custom_roots: None,
            current_dir: base.clone(),
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: "out.patch".into(),
            cursor: 9,
            focus: ExportFocus::DirTree,
        };
        exp.reload_subdirs(empty);
        assert_eq!(exp.subdirs.len(), 2);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn stale_timetravel_result_discarded_after_group_switch() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-ttstale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            None,
            None,
            Catalog::new(Locale::En),
            Some(base.clone()),
            Some(base.clone()),
        );
        app.active_group = Some(1);
        let (tx, rx) = mpsc::channel();
        tx.send(TimeTravelHistory {
            rel_path: PathBuf::from("f.txt"),
            total_sources_checked: 1,
            versions: vec![FileVersionMeta {
                timeline_idx: 0,
                root_path: base.clone(),
                label: "s0".into(),
                timestamp: None,
                is_live: false,
                verified_parent: true,
                exists: true,
                size: Some(5),
                error: None,
            }],
            errors: Vec::new(),
        })
        .unwrap();
        app.tt_job = Some(TimeTravelJob {
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            rel_path: PathBuf::from("f.txt"),
            total_sources: 1,
            group_idx: Some(0),
            verified_live_target: Some(base.clone()),
        });

        app.poll_background_jobs();
        assert!(app.tt_job.is_none());
        assert!(
            matches!(app.mode, Mode::Browse),
            "stale TimeTravelHistory from old group must be discarded"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn effective_snapshots_dir_prefers_cli_over_empty_config_and_saveas_preserves_os_name() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let base = std::env::temp_dir().join(format!("snapshot-diff-cli-snaps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let snaps = base.join("snaps");
        let other = base.join("other");
        std::fs::create_dir_all(&snaps).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        let cfg = Config::default();
        assert!(cfg.general.snapshots_dir.as_os_str().is_empty());
        let app = App::new(
            cfg,
            base.join("config.toml"),
            None,
            Some(snaps.clone()),
            Catalog::new(Locale::En),
            Some(base.clone()),
            Some(base.clone()),
        );
        assert_eq!(app.effective_snapshots_dir(), snaps.as_path());
        assert!(app.config.general.snapshots_dir.as_os_str().is_empty());

        let raw_os = OsStr::from_bytes(b"raw\xFFname.txt").to_os_string();
        let mut save = SaveAsState {
            source_root: base.clone(),
            source_label: "s".into(),
            rel_path: PathBuf::from(&raw_os),
            source_sig: StatSig::default(),
            current_dir: base.clone(),
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: format_os_display(&raw_os),
            original_os_name: Some(raw_os.clone()),
            filename_edited: false,
            cursor: 0,
            focus: SaveAsFocus::DirTree,
        };
        let protected = app.protected_snapshot_dirs();
        save.reload_subdirs(&protected);
        assert_eq!(save.subdirs.len(), 1);
        assert_eq!(save.subdirs[0], OsStr::new("other"));
        // Unedited SaveAsState must preserve exact raw OsString bytes!
        assert_eq!(save.full_dest_path(), Some(base.join(&raw_os)));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn late_timetravel_does_not_overwrite_open_modal() {
        let base = std::env::temp_dir().join(format!("snapshot-diff-ttmodal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            None,
            None,
            Catalog::new(Locale::En),
            Some(base.clone()),
            Some(base.clone()),
        );
        app.mode = Mode::Help;
        let (tx, rx) = mpsc::channel();
        tx.send(TimeTravelHistory {
            rel_path: PathBuf::from("f.txt"),
            total_sources_checked: 1,
            versions: vec![FileVersionMeta {
                timeline_idx: 0,
                root_path: base.clone(),
                label: "s0".into(),
                timestamp: None,
                is_live: false,
                verified_parent: true,
                exists: true,
                size: Some(5),
                error: None,
            }],
            errors: Vec::new(),
        })
        .unwrap();
        app.tt_job = Some(TimeTravelJob {
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            rel_path: PathBuf::from("f.txt"),
            total_sources: 1,
            group_idx: app.active_group,
            verified_live_target: None,
        });

        app.poll_background_jobs();
        assert!(matches!(app.mode, Mode::Help), "open modal must not be overwritten");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn saveas_and_restore_block_auto_discovered_snapshots_when_config_empty() {
        let base = std::env::temp_dir().join(format!(
            "snapshot-diff-auto-protect-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let auto_snaps = base.join("@snapshots");
        let snap = auto_snaps.join("s1");
        let other = base.join("other");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(snap.join("meta-only.txt"), "payload").unwrap();

        let cfg = Config::default();
        assert!(cfg.general.snapshots_dir.as_os_str().is_empty());
        let mut app = App::new(
            cfg,
            base.join("config.toml"),
            None,
            None,
            Catalog::new(Locale::En),
            Some(snap.clone()),
            Some(other.clone()),
        );
        app.auto_snapshot_roots = vec![auto_snaps.clone()];
        let protected = app.protected_snapshot_dirs();
        assert!(protected.contains(&auto_snaps));

        // SaveAs directory browser hides auto-discovered `@snapshots`.
        let mut save = SaveAsState {
            source_root: snap.clone(),
            source_label: "s1".into(),
            rel_path: PathBuf::from("meta-only.txt"),
            source_sig: StatSig::default(),
            current_dir: base.clone(),
            subdirs: Vec::new(),
            dir_selected: 0,
            filename: "meta-only.txt".into(),
            original_os_name: None,
            filename_edited: false,
            cursor: 0,
            focus: SaveAsFocus::DirTree,
        };
        save.reload_subdirs(&protected);
        assert_eq!(save.subdirs, vec![OsString::from("other")]);

        // Even if execute_restore is invoked directly with dest inside `@snapshots`, it refuses.
        let (_, src_sig) = open_file_safe(&snap, std::path::Path::new("meta-only.txt")).unwrap();
        let forbidden_dest = auto_snaps.join("meta-only.txt");
        let plan = RestorePlan {
            source_root: snap,
            rel_path: PathBuf::from("meta-only.txt"),
            source_label: "s1".into(),
            dest_path: forbidden_dest.clone(),
            source_sig: src_sig,
            dest_sig: None,
            expected_live_root: None,
            expected_live_root_sig: None,
        };
        app.execute_restore(&plan);
        assert!(
            !forbidden_dest.exists(),
            "F6 restore must refuse writing into auto-discovered @snapshots"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn czech_restore_errors_contain_no_english_leftovers() {
        let base = std::env::temp_dir().join(format!(
            "snapshot-diff-cs-err-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let snap = base.join("snap");
        let bad_dir = base.join("bad-target");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(snap.join("orig.txt"), "hello").unwrap();
        let (_, src_sig) = open_file_safe(&snap, std::path::Path::new("orig.txt")).unwrap();

        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            Some("cs".to_string()),
            None,
            Catalog::new(Locale::Cs),
            Some(snap.clone()),
            Some(base.clone()),
        );
        app.open_restore_confirm_for_path(
            snap,
            "snap".into(),
            PathBuf::from("orig.txt"),
            src_sig,
            bad_dir,
        );
        let notice = app.notice.as_deref().expect("Czech error notice");
        assert!(notice.starts_with("Obnova selhala:"), "got: {notice}");
        assert!(notice.contains("je adresář"), "got: {notice}");
        assert!(
            !notice.contains("cannot inspect") && !notice.contains("not a regular file"),
            "English detail leaked into Czech UI: {notice}"
        );

        let localized_snaps = app.localize_restore_error("refusing to write inside snapshots directory");
        assert!(localized_snaps.contains("snapshotů"));
        assert!(!localized_snaps.contains("refusing"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dir_stats_queues_multiple_directories_and_completes_all() {
        let base = std::env::temp_dir().join(format!(
            "snapshot-diff-dirstats-q-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let left = base.join("left");
        let right = base.join("right");
        std::fs::create_dir_all(left.join("d1")).unwrap();
        std::fs::create_dir_all(left.join("d2")).unwrap();
        std::fs::create_dir_all(right.join("d1")).unwrap();
        std::fs::create_dir_all(right.join("d2")).unwrap();
        std::fs::write(left.join("d1/a.txt"), "hello").unwrap();
        std::fs::write(right.join("d1/a.txt"), "hello-changed").unwrap();
        std::fs::write(left.join("d2/b.txt"), "one").unwrap();
        std::fs::write(right.join("d2/b.txt"), "two-changed").unwrap();

        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            None,
            None,
            Catalog::new(Locale::En),
            Some(left),
            Some(right),
        );
        for _ in 0..50 {
            app.poll_background_jobs();
            if app.dir_load_job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Rapidly press Space, Down, Space across d1 and d2.
        app.selected = 0;
        app.start_dir_stats_for_selected();
        app.move_selection(1);
        app.start_dir_stats_for_selected();

        for _ in 0..100 {
            app.poll_background_jobs();
            if app.dir_stats_job.is_none() && app.dir_stats_queue.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let s1 = app
            .dir_stats
            .get(&PathBuf::from("d1"))
            .expect("d1 stats computed");
        let s2 = app
            .dir_stats
            .get(&PathBuf::from("d2"))
            .expect("d2 stats computed");
        assert!(s1.done && s1.changed_count == 1, "d1: {s1:?}");
        assert!(s2.done && s2.changed_count == 1, "d2: {s2:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn f10_quits_from_subvolume_picker() {
        let base = std::env::temp_dir().join(format!(
            "snapshot-diff-f10-subvol-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            None,
            None,
            Catalog::new(Locale::En),
            Some(base.clone()),
            Some(base.clone()),
        );
        app.mode = Mode::SubvolumePicker {
            selected: 0,
            initial: true,
        };
        crate::input::handle_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::F(10)),
            40,
        );
        assert!(app.should_quit, "F10 must quit from SubvolumePicker");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn renders_400_snapshots_without_overflow_in_timeline_and_picker() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let base = std::env::temp_dir().join(format!(
            "btrw-400snaps-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut app = App::new(
            Config::default(),
            base.join("config.toml"),
            Some("cs".to_string()),
            None,
            Catalog::new(Locale::Cs),
            Some(base.clone()),
            Some(base.clone()),
        );

        // Build 400 hourly snapshots (~17 days) + LIVE.
        let start_ts = 1_760_000_000u64;
        let mut sources = Vec::with_capacity(401);
        for i in 0..400u64 {
            let ts = UNIX_EPOCH + Duration::from_secs(start_ts + i * 3600);
            sources.push(Source::Snapshot(SnapshotInfo {
                path: base.join(format!("@home.s{i:03}")),
                live_target: PathBuf::from("/home"),
                label: format!("timed/@home.s{i:03}"),
                created_at: Some(ts),
                kind: if i % 48 == 0 {
                    SnapshotKind::Event
                } else {
                    SnapshotKind::Scheduled
                },
                read_only: true,
                verified_parent: true,
            }));
        }
        sources.push(Source::Live {
            path: PathBuf::from("/home"),
            label: "ŽIVÝ (/home)".to_string(),
        });
        app.timeline = sources;
        app.left_idx = 200;
        app.right_idx = 205;
        app.active_side = PanelSide::Left;

        let render_to_string = |app: &mut App, w: u16, h: u16| -> String {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            let mut out = String::new();
            for y in 0..h {
                for x in 0..w {
                    out.push_str(buf[(x, y)].symbol());
                }
                out.push('\n');
            }
            out
        };

        let main_view = render_to_string(&mut app, 96, 14);
        let timeline_row = main_view.lines().nth(2).unwrap_or("");
        assert!(
            timeline_row.contains('«') && timeline_row.contains('»'),
            "timeline with 400 snapshots must show both « and » inside border:\n{main_view}"
        );
        assert!(
            timeline_row.ends_with('│'),
            "right border must not be clipped:\n{main_view}"
        );

        app.open_picker(PanelSide::Left);
        let picker_view = render_to_string(&mut app, 96, 20);
        println!("=== MAIN VIEW (400 snapshots, 96x14) ===\n{main_view}");
        println!("=== SOURCE PICKER (400 snapshots, 96x20) ===\n{picker_view}");

        let _ = std::fs::remove_dir_all(&base);
    }
