mod diff_view;
mod modals;
mod panels;
mod pickers;

use crate::app::{App, Mode, source_day_key, source_marker, source_short_time};
use diff_view::{draw_diff_menu, draw_diff_view, draw_timetravel_view};
use modals::{
    draw_config_modal, draw_export_patch_modal, draw_help_modal, draw_info_modal,
    draw_restore_confirm_modal, draw_saveas_modal,
};
use panels::draw_pane;
use pickers::{draw_source_picker, draw_subvolume_picker};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use btrw_core::compare::format_mode;
use btrw_core::config::Theme;
use btrw_core::fs_safe::format_datetime_local;
use btrw_core::model::PanelSide;

pub(super) struct Palette {
    bg: Color,
    fg: Color,
    border: Color,
    active_border: Color,
    header_bg: Color,
    selected_bg: Color,
    selected_fg: Color,
    added: Color,
}

fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::McBlue => Palette {
            bg: Color::Blue,
            fg: Color::White,
            border: Color::Rgb(0, 40, 0),
            active_border: Color::Yellow,
            header_bg: Color::Cyan,
            selected_bg: Color::Cyan,
            selected_fg: Color::Black,
            added: Color::Rgb(0, 48, 0),
        },
        Theme::Dark => Palette {
            bg: Color::Black,
            fg: Color::Gray,
            border: Color::DarkGray,
            active_border: Color::Cyan,
            header_bg: Color::DarkGray,
            selected_bg: Color::Blue,
            selected_fg: Color::White,
            added: Color::Green,
        },
        Theme::Terminal => Palette {
            bg: Color::Reset,
            fg: Color::Reset,
            border: Color::Gray,
            active_border: Color::Yellow,
            header_bg: Color::Reset,
            selected_bg: Color::Cyan,
            selected_fg: Color::Black,
            added: Color::Green,
        },
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let pal = palette(app.config.general.theme);
    let area = frame.area();

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_timeline(frame, app, &pal, root[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    app.layout.left_pane = panes[0];
    app.layout.right_pane = panes[1];
    app.layout.footer = root[3];
    app.layout.modal = Rect::default();

    draw_pane(frame, app, &pal, PanelSide::Left, panes[0]);
    draw_pane(frame, app, &pal, PanelSide::Right, panes[1]);
    draw_status_line(frame, app, &pal, root[2]);
    draw_footer(frame, app, &pal, root[3]);

    match app.mode.clone() {
        Mode::Browse | Mode::FilterInput => {}
        Mode::SubvolumePicker { selected, initial } => {
            draw_subvolume_picker(frame, app, &pal, selected, initial);
        }
        Mode::SourcePicker {
            side,
            selected,
            expanded_days,
        } => {
            draw_source_picker(frame, app, &pal, side, selected, &expanded_days);
        }
        Mode::DiffView {
            title,
            lines,
            side_rows,
            side_by_side,
            scroll_y,
            scroll_x,
            ..
        } => {
            draw_diff_view(
                frame,
                app,
                &title,
                &lines,
                side_rows.as_deref(),
                side_by_side,
                scroll_y,
                scroll_x,
            );
        }
        Mode::ExportPatch(state) => {
            draw_export_patch_modal(frame, app, &pal, &state);
        }
        Mode::SaveAs(state) => {
            draw_saveas_modal(frame, app, &pal, &state);
        }
        Mode::RestoreConfirm(plan) => {
            draw_restore_confirm_modal(frame, app, &plan);
        }
        Mode::TimeTravel(state) => {
            draw_timetravel_view(frame, app, &state);
        }
        Mode::ConfigModal { selected } => {
            draw_config_modal(frame, app, &pal, selected);
        }
        Mode::DiffMenu { selected } => {
            draw_diff_menu(frame, app, &pal, selected);
        }
        Mode::InfoModal { scroll } => {
            draw_info_modal(frame, app, scroll);
        }
        Mode::Help => draw_help_modal(frame, app, &pal),
    }
}

fn draw_timeline(frame: &mut Frame<'_>, app: &App, pal: &Palette, area: Rect) {
    let snap_only = app
        .timeline
        .iter()
        .filter(|s| matches!(s, btrw_core::model::Source::Snapshot(_)))
        .count();
    let count_num = if snap_only > 0 {
        snap_only
    } else {
        app.timeline.len()
    };
    let count_text = app
        .catalog
        .t_args("snapshot-count", &[("count", (count_num as i64).into())]);
    let title = if let Some(target) = app.active_group_target() {
        format!(
            " {} [{}] ({}) ",
            app.catalog.t("timeline-header"),
            target.display(),
            count_text
        )
    } else {
        format!(" {} ({}) ", app.catalog.t("timeline-header"), count_text)
    };

    let lines = if app.timeline.is_empty() {
        vec![Line::from(Span::styled(
            app.catalog.t("timeline-empty"),
            Style::default().fg(Color::Yellow),
        ))]
    } else {
        let l_desc = app
            .left_source()
            .map(source_short_time)
            .unwrap_or_else(|| "—".to_string());
        let r_desc = app
            .right_source()
            .map(source_short_time)
            .unwrap_or_else(|| "—".to_string());
        let summary_line = Line::from(vec![
            Span::styled(
                format!(" [{}] ", app.catalog.t("panel-left-tag")),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {l_desc}  ↔  "), Style::default().fg(pal.fg)),
            Span::styled(
                format!(" [{}] ", app.catalog.t("panel-right-tag")),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {r_desc}"), Style::default().fg(pal.fg)),
        ]);

        let mut spans = vec![Span::raw(" ")];
        let max_slots = area.width.saturating_sub(6) as usize / 2;
        let total = app.timeline.len();
        let (start, end) = if total <= max_slots || max_slots < 5 {
            (0, total)
        } else {
            let focus = match app.active_side {
                PanelSide::Left => app.left_idx,
                PanelSide::Right => app.right_idx,
            };
            let half = max_slots / 2;
            let start = focus.saturating_sub(half).min(total - max_slots);
            (start, (start + max_slots).min(total))
        };

        if start > 0 {
            spans.push(Span::raw("« "));
        }
        let mut prev_day: Option<String> = None;
        for idx in start..end {
            let cur_day = source_day_key(&app.timeline[idx]);
            if idx > start && cur_day != prev_day {
                spans.push(Span::styled("│", Style::default().fg(pal.border)));
            }
            prev_day = cur_day;

            let is_l = idx == app.left_idx;
            let is_r = idx == app.right_idx;
            let token = if is_l && is_r {
                format!(
                    "[{}{}]",
                    app.catalog.t("panel-left-tag"),
                    app.catalog.t("panel-right-tag")
                )
            } else if is_l {
                format!("[{}]", app.catalog.t("panel-left-tag"))
            } else if is_r {
                format!("[{}]", app.catalog.t("panel-right-tag"))
            } else {
                source_marker(&app.timeline[idx]).to_string()
            };
            let style = if is_l && is_r {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_l {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_r {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(pal.fg)
            };
            spans.push(Span::styled(token, style));
            spans.push(Span::raw(" "));
        }
        if end < total {
            spans.push(Span::raw("»"));
        }
        vec![summary_line, Line::from(spans)]
    };

    let widget = Paragraph::new(lines)
        .style(Style::default().bg(pal.bg).fg(pal.fg))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(pal.border)),
        );
    frame.render_widget(widget, area);
}


fn draw_status_line(frame: &mut Frame<'_>, app: &App, pal: &Palette, area: Rect) {
    let mut parts = Vec::new();
    if let Some(job) = &app.tt_job {
        parts.push(app.catalog.t_args(
            "tt-loading",
            &[
                ("path", job.rel_path.display().to_string().into()),
                ("total", (job.total_sources as i64).into()),
            ],
        ));
    } else if app.export_job.is_some() {
        parts.push(app.catalog.t("status-exporting-patch"));
    } else if app.dir_load_job.is_some() {
        parts.push(app.catalog.t("status-loading-dir"));
    } else if let Some(job) = &app.flat_job {
        parts.push(app.catalog.t_args(
            "flat-scanning",
            &[
                ("scanned", (job.scanned as i64).into()),
                ("path", job.current_path.as_str().into()),
            ],
        ));
    } else if let Some(notice) = &app.notice {
        parts.push(notice.clone());
    } else if matches!(app.mode, Mode::FilterInput) || !app.filter.is_empty() {
        parts.push(
            app.catalog
                .t_args("status-filter", &[("query", app.filter.as_str().into())]),
        );
    } else if let Some(row) = app.selected_row() {
        let active_entry = match app.active_side {
            PanelSide::Left => row.left.as_ref().or(row.right.as_ref()),
            PanelSide::Right => row.right.as_ref().or(row.left.as_ref()),
        };
        if let Some(e) = active_entry {
            let mut meta = format!(
                "{} {}:{} {}",
                format_mode(e.sig.mode),
                e.sig.uid,
                e.sig.gid,
                format_datetime_local(e.sig.mtime_sec)
            );
            if let Some(target) = &e.target {
                meta.push_str(&format!(" → {target}"));
            }
            parts.push(meta);
        }
    } else {
        parts.push(app.catalog.t("status-ready"));
    }

    if let Some((scope, used_send)) = &app.flat_mode {
        let scope_str = if scope.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", scope.display())
        };
        let label = if *used_send {
            format!("{scope_str} · btrfs send")
        } else {
            scope_str
        };
        parts.push(
            app.catalog
                .t_args("flat-mode-badge", &[("scope", label.into())]),
        );
    }

    let sort_lbl = app.sort_label();
    parts.push(
        app.catalog
            .t_args("status-sort", &[("label", sort_lbl.into())]),
    );
    if app.hide_unchanged {
        parts.push(app.catalog.t("status-hidden-unchanged"));
    }
    if !app.compare.errors.is_empty() {
        parts.push(app.catalog.t_args(
            "status-errors",
            &[("count", (app.compare.errors.len() as i64).into())],
        ));
    }
    let line = Paragraph::new(parts.join("  |  "))
        .style(Style::default().bg(Color::Black).fg(pal.fg));
    frame.render_widget(line, area);
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, pal: &Palette, area: Rect) {
    let keys = [
        "key-f1",
        "key-f2",
        "key-f3",
        "key-f4",
        "key-f5",
        "key-f6",
        "key-f8",
        "key-f9",
        "key-f10",
    ]
    .iter()
    .map(|k| app.catalog.t(k))
    .collect::<Vec<_>>()
    .join("  ");

    let footer = Paragraph::new(keys).style(
        Style::default()
            .bg(pal.header_bg)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(footer, area);
}


pub(super) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

pub(super) fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else if max_chars == 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    }
}
