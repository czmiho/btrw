mod mouse;

pub use mouse::handle_mouse;

use crate::app::{App, ExportFocus, Mode, PickerEntry, SaveAsFocus, build_picker_entries_from};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use btrw_core::config::TimeTravelHighlight;
use btrw_core::model::PanelSide;
use std::path::PathBuf;

pub fn handle_key(app: &mut App, key: KeyEvent, term_height: usize) {
    let page = term_height.saturating_sub(7).max(1) as isize;

    match &mut app.mode {
        Mode::DiffMenu { selected } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(2) => app.mode = Mode::Browse,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => {
                if *selected + 1 < 4 {
                    *selected += 1;
                }
            }
            KeyCode::Enter => {
                let choice = *selected;
                app.mode = Mode::Browse;
                match choice {
                    0 => app.open_export_patch(),
                    1 => {
                        let scope = app.rel_dir.clone();
                        app.start_flat_scan(scope);
                    }
                    2 => app.start_flat_scan(PathBuf::new()),
                    _ => app.reload_dir(),
                }
            }
            _ => {}
        },
        Mode::ExportPatch(state) => {
            let protected = App::collect_protected_snapshot_dirs(
                app.cli_snapshots_dir.as_deref(),
                app.config.general.snapshots_dir.as_path(),
                &app.auto_snapshot_roots,
            );
            match key.code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Tab => {
                    state.focus = match state.focus {
                        ExportFocus::Scope => ExportFocus::DirTree,
                        ExportFocus::DirTree => ExportFocus::Filename,
                        ExportFocus::Filename => ExportFocus::Scope,
                    };
                }
                _ => match state.focus {
                    ExportFocus::Scope => match key.code {
                        KeyCode::Left => state.cycle_scope(false),
                        KeyCode::Right | KeyCode::Char(' ') => state.cycle_scope(true),
                        KeyCode::Enter => {
                            let cloned = (**state).clone();
                            app.mode = Mode::Browse;
                            app.run_export_patch_state(&cloned);
                        }
                        _ => {}
                    },
                    ExportFocus::DirTree => match key.code {
                        KeyCode::Up => state.dir_selected = state.dir_selected.saturating_sub(1),
                        KeyCode::Down => {
                            if state.dir_selected + 1 < state.total_dir_items() {
                                state.dir_selected += 1;
                            }
                        }
                        KeyCode::PageUp => {
                            state.dir_selected = state.dir_selected.saturating_sub(page as usize);
                        }
                        KeyCode::PageDown => {
                            state.dir_selected = (state.dir_selected + page as usize)
                                .min(state.total_dir_items().saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::Right => state.enter_selected_dir(&protected),
                        KeyCode::Backspace | KeyCode::Left => state.go_parent_dir(&protected),
                        _ => {}
                    },
                    ExportFocus::Filename => match key.code {
                        KeyCode::Left => state.cursor_left(),
                        KeyCode::Right => state.cursor_right(),
                        KeyCode::Home => state.cursor_home(),
                        KeyCode::End => state.cursor_end(),
                        KeyCode::Backspace => state.backspace(),
                        KeyCode::Delete => state.delete_char(),
                        KeyCode::Char(c) => state.insert_char(c),
                        KeyCode::Enter => {
                            let cloned = (**state).clone();
                            app.mode = Mode::Browse;
                            app.run_export_patch_state(&cloned);
                        }
                        _ => {}
                    },
                },
            }
        }
        Mode::SaveAs(state) => {
            let protected = App::collect_protected_snapshot_dirs(
                app.cli_snapshots_dir.as_deref(),
                app.config.general.snapshots_dir.as_path(),
                &app.auto_snapshot_roots,
            );
            match key.code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Tab => {
                    state.focus = match state.focus {
                        SaveAsFocus::DirTree => SaveAsFocus::Filename,
                        SaveAsFocus::Filename => SaveAsFocus::DirTree,
                    };
                }
                _ => match state.focus {
                    SaveAsFocus::DirTree => match key.code {
                        KeyCode::Up => state.dir_selected = state.dir_selected.saturating_sub(1),
                        KeyCode::Down => {
                            if state.dir_selected + 1 < state.total_dir_items() {
                                state.dir_selected += 1;
                            }
                        }
                        KeyCode::PageUp => {
                            state.dir_selected = state.dir_selected.saturating_sub(page as usize);
                        }
                        KeyCode::PageDown => {
                            state.dir_selected = (state.dir_selected + page as usize)
                                .min(state.total_dir_items().saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::Right => state.enter_selected_dir(&protected),
                        KeyCode::Backspace | KeyCode::Left => state.go_parent_dir(&protected),
                        _ => {}
                    },
                    SaveAsFocus::Filename => match key.code {
                        KeyCode::Left => state.cursor_left(),
                        KeyCode::Right => state.cursor_right(),
                        KeyCode::Home => state.cursor_home(),
                        KeyCode::End => state.cursor_end(),
                        KeyCode::Backspace => state.backspace(),
                        KeyCode::Delete => state.delete_char(),
                        KeyCode::Char(c) => state.insert_char(c),
                        KeyCode::Enter => {
                            if let Some(dest) = state.full_dest_path() {
                                let (sr, sl, rp, ss) = (
                                    state.source_root.clone(),
                                    state.source_label.clone(),
                                    state.rel_path.clone(),
                                    state.source_sig,
                                );
                                app.open_restore_confirm_for_path(sr, sl, rp, ss, dest);
                            }
                        }
                        _ => {}
                    },
                },
            }
        }
        Mode::RestoreConfirm(plan) => match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                app.mode = Mode::Browse;
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let plan_cloned = (**plan).clone();
                app.open_diff_for_restore_plan(plan_cloned);
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let plan_cloned = (**plan).clone();
                app.execute_restore(&plan_cloned);
            }
            _ => {}
        },
        Mode::TimeTravel(state) => {
            let max_bytes = app.config.diff.max_file_bytes;
            let max_lines = app.config.diff.max_lines;
            let total_lines = state.rendered.len();
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(3) => app.mode = Mode::Browse,
                KeyCode::F(2) | KeyCode::Char('e') => {
                    let cloned = (**state).clone();
                    app.open_export_patch_for_timetravel(&cloned);
                }
                KeyCode::F(5) => {
                    let cloned = (**state).clone();
                    app.open_restore_live_from_timetravel(&cloned);
                }
                KeyCode::F(6) => {
                    let cloned = (**state).clone();
                    app.open_save_as_from_timetravel(&cloned);
                }
                KeyCode::Char(' ') => {
                    state.toggle_pin();
                    state.rebuild_lines(max_bytes, max_lines, true);
                }
                KeyCode::Left | KeyCode::Char('[') => {
                    if state.active_idx > 0 {
                        state.active_idx -= 1;
                        if state.pinned_pair().is_none() {
                            state.rebuild_lines(max_bytes, max_lines, true);
                        }
                    }
                }
                KeyCode::Right | KeyCode::Char(']') => {
                    if state.active_idx + 1 < state.history.versions.len() {
                        state.active_idx += 1;
                        if state.pinned_pair().is_none() {
                            state.rebuild_lines(max_bytes, max_lines, true);
                        }
                    }
                }
                KeyCode::Char('m') => {
                    state.highlight = match state.highlight {
                        TimeTravelHighlight::PreviousVersion => TimeTravelHighlight::LiveState,
                        TimeTravelHighlight::LiveState => TimeTravelHighlight::None,
                        TimeTravelHighlight::None => TimeTravelHighlight::PreviousVersion,
                    };
                    state.rebuild_lines(max_bytes, max_lines, false);
                }
                KeyCode::Char('n') => state.jump_change(true),
                KeyCode::Char('p') => state.jump_change(false),
                KeyCode::Down | KeyCode::Char('j') => {
                    if state.scroll_y + 1 < total_lines {
                        state.scroll_y += 1;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => state.scroll_y = state.scroll_y.saturating_sub(1),
                KeyCode::PageDown => {
                    state.scroll_y = (state.scroll_y + page as usize).min(total_lines.saturating_sub(1));
                }
                KeyCode::PageUp => state.scroll_y = state.scroll_y.saturating_sub(page as usize),
                KeyCode::Home => state.scroll_y = 0,
                KeyCode::End => state.scroll_y = total_lines.saturating_sub(1),
                _ => {}
            }
        }
        Mode::ConfigModal { selected } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(9) => app.mode = Mode::Browse,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => {
                if *selected + 1 < App::CONFIG_ITEM_COUNT {
                    *selected += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                let idx = *selected;
                app.cycle_config_item(idx);
            }
            _ => {}
        },
        Mode::InfoModal { scroll } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => app.mode = Mode::Browse,
            KeyCode::Down => *scroll += 1,
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => *scroll += page as usize,
            KeyCode::PageUp => *scroll = scroll.saturating_sub(page as usize),
            _ => {}
        },
        Mode::Help => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1)
            ) {
                app.mode = Mode::Browse;
            }
        }
        Mode::DiffView {
            lines,
            side_rows,
            side_by_side,
            scroll_y,
            scroll_x,
            return_to_restore,
            ..
        } => {
            let total_len = if *side_by_side {
                side_rows.as_ref().map(|r| r.len()).unwrap_or(lines.len())
            } else {
                lines.len()
            };
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(3) => {
                    if let Some(plan) = return_to_restore.take() {
                        app.mode = Mode::RestoreConfirm(plan);
                    } else {
                        app.mode = Mode::Browse;
                    }
                }
                KeyCode::Char('v') => {
                    if side_rows.is_some() {
                        *side_by_side = !*side_by_side;
                        *scroll_y = 0;
                        *scroll_x = 0;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if *scroll_y + 1 < total_len {
                        *scroll_y += 1;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => *scroll_y = scroll_y.saturating_sub(1),
                KeyCode::PageDown => {
                    *scroll_y = (*scroll_y + page as usize).min(total_len.saturating_sub(1));
                }
                KeyCode::PageUp => *scroll_y = scroll_y.saturating_sub(page as usize),
                KeyCode::Right | KeyCode::Char('l') => *scroll_x += 8,
                KeyCode::Left | KeyCode::Char('h') => *scroll_x = scroll_x.saturating_sub(8),
                KeyCode::Home => *scroll_y = 0,
                KeyCode::End => *scroll_y = total_len.saturating_sub(1),
                _ => {}
            }
        }
        Mode::SubvolumePicker { selected, initial } => {
            let initial = *initial;
            let total = app.groups.len();
            match key.code {
                KeyCode::F(10) => {
                    app.should_quit = true;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    if initial {
                        app.should_quit = true;
                    } else {
                        app.mode = Mode::Browse;
                    }
                }
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if *selected + 1 < total {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let chosen = *selected;
                    app.select_group(chosen);
                }
                _ => {}
            }
        }
        Mode::SourcePicker {
            side,
            selected,
            expanded_days,
        } => {
            let side = *side;
            let entries = build_picker_entries_from(&app.timeline, &app.catalog, expanded_days);
            let total = entries.len();
            match key.code {
                KeyCode::Esc => {
                    if !app.cancel_du_job() {
                        app.mode = Mode::Browse;
                    }
                }
                KeyCode::Char('q') => {
                    app.cancel_du_job();
                    app.mode = Mode::Browse;
                }
                KeyCode::Char(' ') => {
                    if let Some(PickerEntry::Snapshot { timeline_idx, .. }) = entries.get(*selected)
                    {
                        let p = app.timeline[*timeline_idx].path().clone();
                        app.start_du_job(vec![p], true);
                    }
                }
                KeyCode::Char('s') => {
                    let all_snaps: Vec<PathBuf> = app
                        .timeline
                        .iter()
                        .filter_map(|src| match src {
                            btrw_core::model::Source::Snapshot(info) => Some(info.path.clone()),
                            _ => None,
                        })
                        .collect();
                    app.start_du_job(all_snaps, false);
                }
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    if *selected + 1 < total {
                        *selected += 1;
                    }
                }
                KeyCode::PageUp => *selected = selected.saturating_sub(page as usize),
                KeyCode::PageDown => {
                    *selected = (*selected + page as usize).min(total.saturating_sub(1));
                }
                KeyCode::Right => {
                    if let Some(PickerEntry::Day { day_key, .. }) = entries.get(*selected) {
                        expanded_days.insert(day_key.clone());
                    }
                }
                KeyCode::Left => match entries.get(*selected) {
                    Some(PickerEntry::Day { day_key, .. }) => {
                        expanded_days.remove(day_key);
                    }
                    Some(PickerEntry::Snapshot { day_key, .. }) => {
                        if let Some(day_pos) = entries.iter().position(|e| {
                            matches!(e, PickerEntry::Day { day_key: k, .. } if k == day_key)
                        }) {
                            *selected = day_pos;
                        }
                    }
                    _ => {}
                },
                KeyCode::Enter => match entries.get(*selected) {
                    Some(PickerEntry::Day { day_key, expanded, .. }) => {
                        if *expanded {
                            expanded_days.remove(day_key);
                        } else {
                            expanded_days.insert(day_key.clone());
                        }
                    }
                    Some(
                        PickerEntry::Live { timeline_idx, .. }
                        | PickerEntry::Snapshot { timeline_idx, .. },
                    ) => {
                        let chosen = *timeline_idx;
                        match side {
                            PanelSide::Left => app.left_idx = chosen,
                            PanelSide::Right => app.right_idx = chosen,
                        }
                        app.mode = Mode::Browse;
                        app.reload_dir();
                    }
                    None => {}
                },
                _ => {}
            }
        }
        Mode::FilterInput => match key.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.selected = 0;
                app.mode = Mode::Browse;
            }
            KeyCode::Enter => {
                app.mode = Mode::Browse;
                app.clamp_selection();
            }
            KeyCode::Backspace => {
                app.filter.pop();
                app.selected = 0;
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.selected = 0;
            }
            _ => {}
        },
        Mode::Browse => {
            if key.modifiers.contains(KeyModifiers::ALT) {
                match key.code {
                    KeyCode::F(1) => {
                        app.open_picker(PanelSide::Left);
                        return;
                    }
                    KeyCode::F(2) => {
                        app.open_picker(PanelSide::Right);
                        return;
                    }
                    _ => {}
                }
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('u') | KeyCode::Char('U') => {
                        app.swap_panels();
                        return;
                    }
                    KeyCode::Left => {
                        app.step_timeline(-1);
                        return;
                    }
                    KeyCode::Right => {
                        app.step_timeline(1);
                        return;
                    }
                    KeyCode::Char('c') => {
                        app.should_quit = true;
                        return;
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::F(10) | KeyCode::Char('q') => app.should_quit = true,
                KeyCode::F(1) => app.mode = Mode::Help,
                KeyCode::Tab => {
                    app.active_side = match app.active_side {
                        PanelSide::Left => PanelSide::Right,
                        PanelSide::Right => PanelSide::Left,
                    };
                }
                KeyCode::Up => app.move_selection(-1),
                KeyCode::Down => app.move_selection(1),
                KeyCode::PageUp => app.move_selection(-page),
                KeyCode::PageDown => app.move_selection(page),
                KeyCode::Home => app.selected = 0,
                KeyCode::End => app.selected = app.total_visible_items().saturating_sub(1),
                KeyCode::Enter => app.activate_selected(),
                KeyCode::Char(' ') => app.start_dir_stats_for_selected(),
                KeyCode::Backspace => app.go_parent(),
                KeyCode::F(2) => app.mode = Mode::DiffMenu { selected: 0 },
                KeyCode::Char('f') => {
                    if app.flat_mode.is_some() {
                        app.reload_dir();
                    } else {
                        let scope = app.rel_dir.clone();
                        app.start_flat_scan(scope);
                    }
                }
                KeyCode::F(3) => app.start_timetravel_for_selected(),
                KeyCode::F(5) => app.open_restore_live(),
                KeyCode::F(6) => app.open_restore_save_as(),
                KeyCode::F(7) | KeyCode::Char('/') => app.mode = Mode::FilterInput,
                KeyCode::F(8) | KeyCode::Char('g') => app.open_subvolume_picker(),
                KeyCode::F(9) => app.mode = Mode::ConfigModal { selected: 0 },
                KeyCode::Char('i') => app.mode = Mode::InfoModal { scroll: 0 },
                KeyCode::Char('n') => app.toggle_sort('n'),
                KeyCode::Char('s') => app.toggle_sort('s'),
                KeyCode::Char('c') => app.toggle_sort('c'),
                KeyCode::Char('t') => app.toggle_sort('t'),
                KeyCode::Char('1') => app.open_picker(PanelSide::Left),
                KeyCode::Char('2') => app.open_picker(PanelSide::Right),
                KeyCode::Char('[') => app.step_timeline(-1),
                KeyCode::Char(']') => app.step_timeline(1),
                KeyCode::Char('{') => app.step_timeline_day(-1),
                KeyCode::Char('}') => app.step_timeline_day(1),
                KeyCode::Char('h') => {
                    app.hide_unchanged = !app.hide_unchanged;
                    app.clamp_selection();
                }
                KeyCode::Char('r') => app.reload_dir(),
                KeyCode::Esc => {
                    if app.cancel_timetravel_job()
                        || app.cancel_export_job()
                        || app.cancel_flat_job()
                        || app.cancel_dir_stats_job()
                        || app.cancel_dir_load_job()
                    {
                        // Cancelled running background traversal.
                    } else if app.flat_mode.is_some() {
                        app.reload_dir();
                    } else if !app.filter.is_empty() {
                        app.filter.clear();
                        app.clamp_selection();
                    }
                }
                _ => {}
            }
        }
    }
}

