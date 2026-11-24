use super::{Palette, centered_rect, truncate};
use crate::app::{App, PickerEntry};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use btrw_core::compare::human_size;
use btrw_core::model::PanelSide;
use std::collections::BTreeSet;

pub(super) fn draw_subvolume_picker(
    frame: &mut Frame<'_>,
    app: &mut App,
    pal: &Palette,
    selected: usize,
    initial: bool,
) {
    let popup = centered_rect(76, 60, frame.area());
    app.layout.modal = popup;
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem<'_>> = app
        .groups
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let group = &entry.group;
            let count = app.catalog.t_args(
                "snapshot-count",
                &[("count", (group.snapshots.len() as i64).into())],
            );
            let subvol = group.subvol_name.as_deref().unwrap_or("—");
            let latest = group
                .snapshots
                .last()
                .map(|s| s.label.as_str())
                .unwrap_or("—");
            let active_mark = if app.active_group == Some(idx) {
                " *"
            } else {
                ""
            };
            let style = if group.snapshots.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(
                    " {:<28} {:<20} {:>14}   {}{active_mark}",
                    truncate(&entry.tree_label, 28),
                    truncate(subvol, 20),
                    count,
                    latest
                ),
                style,
            )))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));

    let hint_key = if initial {
        "subvol-picker-hint-initial"
    } else {
        "subvol-picker-hint"
    };

    let list = List::new(items)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .highlight_style(
            Style::default()
                .bg(pal.selected_bg)
                .fg(pal.selected_fg)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .title(format!(" {} ", app.catalog.t("subvol-picker-title")))
                .title_bottom(format!(" {} ", app.catalog.t(hint_key)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_stateful_widget(list, popup, &mut state);
    app.modal_scroll = state.offset();
}

pub(super) fn draw_source_picker(
    frame: &mut Frame<'_>,
    app: &mut App,
    pal: &Palette,
    side: PanelSide,
    selected: usize,
    expanded_days: &BTreeSet<String>,
) {
    let popup = centered_rect(76, 75, frame.area());
    app.layout.modal = popup;
    frame.render_widget(Clear, popup);

    let title_key = match side {
        PanelSide::Left => "picker-title-left",
        PanelSide::Right => "picker-title-right",
    };

    let entries = app.build_picker_entries(expanded_days);
    let items: Vec<ListItem<'_>> = entries
        .iter()
        .map(|entry| match entry {
            PickerEntry::Live {
                timeline_idx,
                label,
            } => {
                let tags = active_tags(*timeline_idx, app.left_idx, app.right_idx);
                ListItem::new(Line::from(Span::styled(
                    format!(" ★ {label}{tags}"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )))
            }
            PickerEntry::Day {
                display_day,
                count,
                expanded,
                ..
            } => {
                let arrow = if *expanded { "▾" } else { "▸" };
                let cnt = app
                    .catalog
                    .t_args("snapshot-count", &[("count", (*count as i64).into())]);
                ListItem::new(Line::from(Span::styled(
                    format!(" {arrow} {display_day}  ({cnt})"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )))
            }
            PickerEntry::Snapshot {
                timeline_idx,
                time_hm,
                label,
                marker,
                ..
            } => {
                let tags = active_tags(*timeline_idx, app.left_idx, app.right_idx);
                let path = app.timeline[*timeline_idx].path();
                let is_running = app
                    .du_job
                    .as_ref()
                    .and_then(|j| j.current.as_ref())
                    .is_some_and(|p| p == path);
                let size_str = if is_running {
                    format!("{:>10} {:>10}", "…", "…")
                } else {
                    match app.du_cache.get(path) {
                        Some(Ok(u)) => format!(
                            "{:>10} {:>10}",
                            human_size(u.total_bytes),
                            human_size(u.exclusive_bytes)
                        ),
                        Some(Err(_)) => format!("{:>10} {:>10}", "?", "?"),
                        None => format!("{:>10} {:>10}", "—", "—"),
                    }
                };
                ListItem::new(format!("     {marker} {time_hm}  {size_str}  {label}{tags}"))
            }
        })
        .collect();

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }

    let bottom_hint = if let Some(job) = &app.du_job {
        app.catalog.t_args(
            "picker-du-running",
            &[
                ("done", (job.done as i64).into()),
                ("total", (job.total as i64).into()),
            ],
        )
    } else {
        app.catalog.t("picker-hint")
    };

    let list = List::new(items)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .highlight_style(
            Style::default()
                .bg(pal.selected_bg)
                .fg(pal.selected_fg)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .title(format!(" {} ", app.catalog.t(title_key)))
                .title_bottom(format!(" {bottom_hint} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_stateful_widget(list, popup, &mut state);
    app.modal_scroll = state.offset();
}


fn active_tags(idx: usize, left_idx: usize, right_idx: usize) -> &'static str {
    match (idx == left_idx, idx == right_idx) {
        (true, true) => " [L,R]",
        (true, false) => " [L]",
        (false, true) => " [R]",
        (false, false) => "",
    }
}
