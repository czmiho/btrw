use crate::app::{App, ExportFocus, Mode, PickerEntry, SaveAsFocus, build_picker_entries_from};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use btrw_core::model::PanelSide;
use std::time::{Duration, Instant};

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn is_double_click(app: &mut App, col: u16, row: u16) -> bool {
    let now = Instant::now();
    let double = app.last_click.is_some_and(|(prev, _, pr)| {
        pr == row && now.duration_since(prev) <= Duration::from_millis(400)
    });
    app.last_click = if double { None } else { Some((now, col, row)) };
    double
}

pub fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let (col, row) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::ScrollUp => match &mut app.mode {
            Mode::Browse => app.move_selection(-3),
            Mode::DiffView { scroll_y, .. } => *scroll_y = scroll_y.saturating_sub(3),
            Mode::TimeTravel(state) => state.scroll_y = state.scroll_y.saturating_sub(3),
            Mode::InfoModal { scroll } => *scroll = scroll.saturating_sub(3),
            Mode::SubvolumePicker { selected, .. } | Mode::SourcePicker { selected, .. } => {
                *selected = selected.saturating_sub(1);
            }
            Mode::ConfigModal { selected } => *selected = selected.saturating_sub(1),
            Mode::SaveAs(state) => state.dir_selected = state.dir_selected.saturating_sub(1),
            Mode::ExportPatch(state) => state.dir_selected = state.dir_selected.saturating_sub(1),
            _ => {}
        },
        MouseEventKind::ScrollDown => match &mut app.mode {
            Mode::Browse => app.move_selection(3),
            Mode::DiffView {
                lines, scroll_y, ..
            } => {
                *scroll_y = (*scroll_y + 3).min(lines.len().saturating_sub(1));
            }
            Mode::TimeTravel(state) => {
                state.scroll_y = (state.scroll_y + 3).min(state.rendered.len().saturating_sub(1));
            }
            Mode::InfoModal { scroll } => *scroll += 3,
            Mode::SubvolumePicker { selected, .. } => {
                if *selected + 1 < app.groups.len() {
                    *selected += 1;
                }
            }
            Mode::SourcePicker {
                selected,
                expanded_days,
                ..
            } => {
                let total = build_picker_entries_from(&app.timeline, &app.catalog, expanded_days).len();
                if *selected + 1 < total {
                    *selected += 1;
                }
            }
            Mode::ConfigModal { selected } => {
                if *selected + 1 < App::CONFIG_ITEM_COUNT {
                    *selected += 1;
                }
            }
            Mode::SaveAs(state) => {
                if state.dir_selected + 1 < state.total_dir_items() {
                    state.dir_selected += 1;
                }
            }
            Mode::ExportPatch(state) => {
                if state.dir_selected + 1 < state.total_dir_items() {
                    state.dir_selected += 1;
                }
            }
            _ => {}
        },
        MouseEventKind::Down(MouseButton::Left) => {
            app.notice = None;
            let double = is_double_click(app, col, row);
            let modal = app.layout.modal;
            match app.mode.clone() {
                Mode::Browse => {
                    let in_left = rect_contains(app.layout.left_pane, col, row);
                    let in_right = rect_contains(app.layout.right_pane, col, row);
                    if in_left || in_right {
                        app.active_side = if in_left {
                            PanelSide::Left
                        } else {
                            PanelSide::Right
                        };
                        let pane = if in_left {
                            app.layout.left_pane
                        } else {
                            app.layout.right_pane
                        };
                        if row > pane.y && row + 1 < pane.y + pane.height {
                            let idx = app.scroll + (row - pane.y - 1) as usize;
                            if idx < app.total_visible_items() {
                                app.selected = idx;
                                if double {
                                    app.activate_selected();
                                }
                            }
                        }
                    }
                }
                Mode::SubvolumePicker { .. } => {
                    if rect_contains(modal, col, row)
                        && row > modal.y
                        && row + 1 < modal.y + modal.height
                    {
                        let idx = app.modal_scroll + (row - modal.y - 1) as usize;
                        if idx < app.groups.len() {
                            if let Mode::SubvolumePicker { selected, .. } = &mut app.mode {
                                *selected = idx;
                            }
                            if double {
                                app.select_group(idx);
                            }
                        }
                    }
                }
                Mode::SourcePicker {
                    side,
                    mut expanded_days,
                    ..
                } => {
                    if rect_contains(modal, col, row)
                        && row > modal.y
                        && row + 1 < modal.y + modal.height
                    {
                        let entries =
                            build_picker_entries_from(&app.timeline, &app.catalog, &expanded_days);
                        let idx = app.modal_scroll + (row - modal.y - 1) as usize;
                        if idx < entries.len() {
                            if let Mode::SourcePicker { selected, .. } = &mut app.mode {
                                *selected = idx;
                            }
                            if double {
                                match &entries[idx] {
                                    PickerEntry::Day {
                                        day_key, expanded, ..
                                    } => {
                                        if *expanded {
                                            expanded_days.remove(day_key);
                                        } else {
                                            expanded_days.insert(day_key.clone());
                                        }
                                        app.mode = Mode::SourcePicker {
                                            side,
                                            selected: idx,
                                            expanded_days,
                                        };
                                    }
                                    PickerEntry::Live { timeline_idx, .. }
                                    | PickerEntry::Snapshot { timeline_idx, .. } => {
                                        match side {
                                            PanelSide::Left => app.left_idx = *timeline_idx,
                                            PanelSide::Right => app.right_idx = *timeline_idx,
                                        }
                                        app.mode = Mode::Browse;
                                        app.reload_dir();
                                    }
                                }
                            }
                        }
                    }
                }
                Mode::ConfigModal { .. } => {
                    if rect_contains(modal, col, row)
                        && row > modal.y
                        && row + 1 < modal.y + modal.height
                    {
                        let idx = app.modal_scroll + (row - modal.y - 1) as usize;
                        if idx < App::CONFIG_ITEM_COUNT {
                            if let Mode::ConfigModal { selected } = &mut app.mode {
                                *selected = idx;
                            }
                            if double {
                                app.cycle_config_item(idx);
                            }
                        }
                    }
                }
                Mode::SaveAs(_) => {
                    if rect_contains(modal, col, row)
                        && row > modal.y
                        && row + 1 < modal.y + modal.height
                    {
                        let idx = app.modal_scroll + (row - modal.y - 1) as usize;
                        let protected = app.protected_snapshot_dirs();
                        if let Mode::SaveAs(state) = &mut app.mode
                            && idx < state.total_dir_items()
                        {
                            state.focus = SaveAsFocus::DirTree;
                            state.dir_selected = idx;
                            if double {
                                state.enter_selected_dir(&protected);
                            }
                        }
                    }
                }
                Mode::ExportPatch(_) => {
                    if rect_contains(modal, col, row)
                        && row > modal.y
                        && row + 1 < modal.y + modal.height
                    {
                        let idx = app.modal_scroll + (row - modal.y - 1) as usize;
                        let protected = app.protected_snapshot_dirs();
                        if let Mode::ExportPatch(state) = &mut app.mode
                            && idx < state.total_dir_items()
                        {
                            state.focus = ExportFocus::DirTree;
                            state.dir_selected = idx;
                            if double {
                                state.enter_selected_dir(&protected);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}
