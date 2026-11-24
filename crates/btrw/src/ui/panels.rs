use super::{Palette, truncate};
use crate::app::{App, source_header_label};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use btrw_core::compare::{
    DirStats, PaneRow, format_octal_mode, human_delta, human_size,
};
use btrw_core::config::{PanelsConfig, PermissionsColumn};
use btrw_core::fs_safe::format_datetime_local;
use btrw_core::model::{ChangeKind, EntryKind, PanelSide};
use std::collections::HashMap;
use std::path::PathBuf;

pub(super) fn draw_pane(frame: &mut Frame<'_>, app: &mut App, pal: &Palette, side: PanelSide, area: Rect) {
    let is_active = app.active_side == side;
    let tag = match side {
        PanelSide::Left => app.catalog.t("panel-left-tag"),
        PanelSide::Right => app.catalog.t("panel-right-tag"),
    };
    let src_name = match side {
        PanelSide::Left => app
            .left_source()
            .map(source_header_label)
            .unwrap_or_else(|| "—".to_string()),
        PanelSide::Right => app
            .right_source()
            .map(source_header_label)
            .unwrap_or_else(|| "—".to_string()),
    };
    let rel_display = if app.rel_dir.as_os_str().is_empty() {
        "/".to_string()
    } else {
        format!("/{}", app.rel_dir.display())
    };
    let title = format!(" [{tag}] {src_name} — {rel_display} ");

    let inner_width = area.width.saturating_sub(2) as usize;
    let vis = app.visible_indices();
    let mut items = Vec::with_capacity(vis.len() + 1);

    if app.has_parent() {
        items.push(ListItem::new(Line::from(Span::styled(
            "   ../",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))));
    }

    for row_idx in vis {
        let row = &app.compare.rows[row_idx];
        items.push(format_pane_row(
            row,
            side,
            inner_width,
            &app.config.panels,
            &app.dir_stats,
            &app.uid_names,
            &app.gid_names,
            &app.catalog,
            pal,
        ));
    }

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.selected.min(items.len() - 1)));
    }

    let highlight_style = if is_active {
        Style::default()
            .bg(pal.selected_bg)
            .fg(pal.selected_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    };

    let list = List::new(items)
        .style(Style::default().bg(pal.bg).fg(pal.fg))
        .highlight_style(highlight_style)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if is_active {
                    pal.active_border
                } else {
                    pal.border
                })),
        );

    frame.render_stateful_widget(list, area, &mut state);
    app.scroll = state.offset();
}

#[allow(clippy::too_many_arguments)]
fn format_pane_row<'a>(
    row: &PaneRow,
    side: PanelSide,
    width: usize,
    cfg: &PanelsConfig,
    dir_stats: &HashMap<PathBuf, DirStats>,
    uid_names: &HashMap<u32, String>,
    gid_names: &HashMap<u32, String>,
    catalog: &btrw_core::i18n::Catalog,
    pal: &Palette,
) -> ListItem<'a> {
    let entry = match side {
        PanelSide::Left => row.left.as_ref(),
        PanelSide::Right => row.right.as_ref(),
    };
    let stats = if row.is_dir() {
        dir_stats.get(&row.rel_path)
    } else {
        None
    };

    let (badge, badge_color) = match row.status {
        Some(ChangeKind::Added) => ("A", pal.added),
        Some(ChangeKind::Deleted) => ("D", Color::Red),
        Some(ChangeKind::Modified) => ("M", Color::Yellow),
        Some(ChangeKind::TypeChanged) => ("T", Color::Magenta),
        Some(ChangeKind::MetadataOnly) => ("m", Color::Cyan),
        Some(ChangeKind::Error) => ("?", Color::LightRed),
        None if stats.is_some_and(|s| s.changed_count > 0) => ("*", Color::Yellow),
        None => (" ", pal.fg),
    };

    let display_name = match entry {
        Some(e) if e.kind == EntryKind::Directory => format!("{}/", row.name),
        Some(e) if e.kind == EntryKind::Symlink => format!("{}@", row.name),
        Some(_) => row.name.clone(),
        None => "—".to_string(),
    };

    let size_col = if let Some(st) = stats {
        let prefix = if !st.errors.is_empty() {
            "!"
        } else if st.done {
            ""
        } else {
            "~"
        };
        let bytes = match side {
            PanelSide::Left => st.left_bytes,
            PanelSide::Right => st.right_bytes,
        };
        format!("{prefix}{}", human_size(bytes))
    } else {
        match entry {
            Some(e) if e.kind == EntryKind::File => human_size(e.size.unwrap_or(0)),
            Some(e) if e.kind == EntryKind::Directory => "<DIR>".to_string(),
            _ => "—".to_string(),
        }
    };

    // Build optional columns from right to left while keeping at least 14 chars for the filename.
    let mut cols: Vec<String> = Vec::new();
    let mut cols_len: usize = 0;
    let min_name_space = 14usize;

    if cfg.show_size && width >= min_name_space + 15 {
        cols.push(format!("{size_col:>10}"));
        cols_len += 11;
    }
    if side == PanelSide::Right && cfg.show_delta && width >= min_name_space + cols_len + 15 {
        let delta_str = if let Some(st) = stats {
            let prefix = if st.done { "" } else { "~" };
            let chg = catalog.t_args("dir-chg-short", &[("count", (st.changed_count as i64).into())]);
            format!("{prefix}{chg}")
        } else {
            match row.delta_bytes {
                Some(d) if d != 0 => human_delta(d),
                _ => String::new(),
            }
        };
        cols.push(format!("{delta_str:>10}"));
        cols_len += 11;
    }
    if cfg.show_mtime && width >= min_name_space + cols_len + 21 {
        let mtime_str = entry
            .map(|e| format_datetime_local(e.sig.mtime_sec))
            .unwrap_or_else(|| "—".to_string());
        cols.push(format!("{mtime_str:>16}"));
        cols_len += 17;
    }
    match cfg.show_permissions {
        PermissionsColumn::Off => {}
        PermissionsColumn::Octal => {
            if width >= min_name_space + cols_len + 9 {
                let perm = entry
                    .map(|e| format_octal_mode(e.sig.mode))
                    .unwrap_or_else(|| "—".to_string());
                cols.push(format!("{perm:>4}"));
            }
        }
        PermissionsColumn::OctalOwnerGroup => {
            if width >= min_name_space + cols_len + 23 {
                let perm = entry
                    .map(|e| {
                        let owner = uid_names
                            .get(&e.sig.uid)
                            .cloned()
                            .unwrap_or_else(|| e.sig.uid.to_string());
                        let group = gid_names
                            .get(&e.sig.gid)
                            .cloned()
                            .unwrap_or_else(|| e.sig.gid.to_string());
                        format!("{} {}", format_octal_mode(e.sig.mode), truncate(&format!("{owner}:{group}"), 13))
                    })
                    .unwrap_or_else(|| "—".to_string());
                cols.push(format!("{perm:>18}"));
            }
        }
    }

    let right_col = cols.join(" ");
    let name_width = width.saturating_sub(right_col.len() + 4);
    let trimmed_name = truncate(&display_name, name_width);

    let name_style = if entry.is_none() {
        Style::default().fg(Color::DarkGray)
    } else if row.is_dir() {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else if row.status.is_some() {
        Style::default().fg(badge_color)
    } else {
        Style::default().fg(pal.fg)
    };

    ListItem::new(Line::from(vec![
        Span::styled(
            format!(" {badge} "),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{trimmed_name:<name_width$}"), name_style),
        Span::raw(" "),
        Span::styled(right_col, Style::default().fg(pal.fg)),
    ]))
}

