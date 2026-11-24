use super::{Palette, centered_rect, truncate};
use crate::app::{
    App, ExportFocus, ExportPatchState, ExportScope, SaveAsFocus, SaveAsState,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use btrw_core::compare::{format_mode, human_size};
use btrw_core::config::ReflinkMode;
use btrw_core::fs_safe::format_datetime_local;
use btrw_core::restore::RestorePlan;

pub(super) fn draw_saveas_modal(frame: &mut Frame<'_>, app: &mut App, pal: &Palette, state: &SaveAsState) {
    let popup = centered_rect(76, 72, frame.area());
    frame.render_widget(Clear, popup);

    let outer = Block::default()
        .title(format!(" {} ", app.catalog.t("restore-saveas-title")))
        .title_bottom(format!(" {} ", app.catalog.t("restore-saveas-hint")))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);

    // 1. Directory picker tree.
    app.layout.modal = chunks[0];
    let dir_focused = state.focus == SaveAsFocus::DirTree;
    let mut items = Vec::with_capacity(state.total_dir_items());
    if state.has_parent() {
        items.push(ListItem::new(Line::from(Span::styled(
            " ../",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))));
    }
    for sub in &state.subdirs {
        items.push(ListItem::new(format!(
            " ├─ {}/",
            btrw_core::fs_safe::format_os_display(sub)
        )));
    }
    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(state.dir_selected.min(items.len() - 1)));
    }
    let dir_title = app.catalog.t_args(
        "restore-saveas-dir",
        &[("dir", state.current_dir.display().to_string().into())],
    );
    let dir_list = List::new(items)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .highlight_style(if dir_focused {
            Style::default()
                .bg(pal.selected_bg)
                .fg(pal.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        })
        .block(
            Block::default()
                .title(format!(" {dir_title} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if dir_focused {
                    Color::Yellow
                } else {
                    Color::DarkGray
                })),
        );
    frame.render_stateful_widget(dir_list, chunks[0], &mut list_state);
    app.modal_scroll = list_state.offset();

    // 2. Filename input with visible cursor.
    let name_focused = state.focus == SaveAsFocus::Filename;
    let chars: Vec<char> = state.filename.chars().collect();
    let cur = state.cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (at_cur, after): (String, String) = if cur < chars.len() {
        (
            chars[cur].to_string(),
            chars[cur + 1..].iter().collect(),
        )
    } else {
        (" ".to_string(), String::new())
    };
    let name_line = if name_focused {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(before, Style::default().fg(Color::Yellow)),
            Span::styled(
                at_cur,
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(after, Style::default().fg(Color::Yellow)),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", state.filename),
            Style::default().fg(Color::White),
        ))
    };

    let name_box = Paragraph::new(name_line).block(
        Block::default()
            .title(format!(" {} ", app.catalog.t("restore-saveas-name")))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if name_focused {
                Color::Yellow
            } else {
                Color::DarkGray
            })),
    );
    frame.render_widget(name_box, chunks[1]);

    // 3. Live full target preview.
    let preview = state
        .full_dest_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_string());
    let target_line = Paragraph::new(format!(
        " {}",
        app.catalog
            .t_args("restore-saveas-target", &[("path", preview.into())])
    ))
    .style(Style::default().fg(Color::Cyan));
    frame.render_widget(target_line, chunks[2]);
}

pub(super) fn draw_restore_confirm_modal(frame: &mut Frame<'_>, app: &App, plan: &RestorePlan) {
    let popup = centered_rect(76, 56, frame.area());
    frame.render_widget(Clear, popup);

    let (warn_text, warn_color) = if plan.dest_sig.is_some() {
        (app.catalog.t("restore-warn-overwrite"), Color::Red)
    } else {
        (app.catalog.t("restore-warn-new"), Color::Green)
    };

    let src_meta = format!(
        "{}  {}  {}:{}  {}",
        human_size(plan.source_sig.size),
        format_mode(plan.source_sig.mode),
        plan.source_sig.uid,
        plan.source_sig.gid,
        format_datetime_local(plan.source_sig.mtime_sec)
    );
    let dst_meta = match &plan.dest_sig {
        Some(s) => format!(
            "{}  {}  {}:{}  {}",
            human_size(s.size),
            format_mode(s.mode),
            s.uid,
            s.gid,
            format_datetime_local(s.mtime_sec)
        ),
        None => "—".to_string(),
    };
    let bak_opt = if app.config.restore.create_backup {
        app.catalog.t("restore-opt-backup-on")
    } else {
        app.catalog.t("restore-opt-backup-off")
    };
    let ref_opt = match app.config.restore.use_reflink {
        ReflinkMode::Auto => app.catalog.t("restore-opt-reflink-auto"),
        ReflinkMode::Never => app.catalog.t("restore-opt-reflink-never"),
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {warn_text}"),
            Style::default()
                .fg(warn_color)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if let Some(ds) = &plan.dest_sig
        && ds.nlink > 1
    {
        let hl_msg = app
            .catalog
            .t_args("restore-warn-hardlink", &[("nlink", (ds.nlink as i64).into())]);
        lines.push(Line::from(Span::styled(
            format!("  {hl_msg}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("  {:<18} ", app.catalog.t("restore-label-source")),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "{} / {}",
                plan.source_label,
                plan.rel_path.display()
            )),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {:<18} ", app.catalog.t("restore-label-dest")),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                plan.dest_path.display().to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("  {:<18} ", app.catalog.t("restore-label-src-meta")),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(src_meta),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {:<18} ", app.catalog.t("restore-label-dst-meta")),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(dst_meta),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("  {:<18} ", app.catalog.t("restore-label-options")),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(format!("{bak_opt}  |  {ref_opt}")),
        ]),
    ]);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(format!(" {} ", app.catalog.t("restore-title")))
                .title_bottom(format!(" {} ", app.catalog.t("restore-confirm-hint")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(warn_color)),
        );
    frame.render_widget(paragraph, popup);
}

pub(super) fn draw_export_patch_modal(
    frame: &mut Frame<'_>,
    app: &mut App,
    pal: &Palette,
    state: &ExportPatchState,
) {
    let popup = centered_rect(76, 76, frame.area());
    frame.render_widget(Clear, popup);

    let outer = Block::default()
        .title(format!(" {} ", app.catalog.t("export-title")))
        .title_bottom(format!(" {} ", app.catalog.t("export-hint")))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black).fg(Color::White));
    let inner = outer.inner(popup);
    frame.render_widget(outer, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);

    // 1. Scope selector toggle.
    let scope_focused = state.focus == ExportFocus::Scope;
    let sel_name = state
        .selected_row
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_else(|| "/".to_string());
    let cur_dir_label = if app.rel_dir.as_os_str().is_empty() {
        "/".to_string()
    } else {
        format!("/{}", app.rel_dir.display())
    };
    let scope_text = match state.scope {
        ExportScope::SelectedObject => app
            .catalog
            .t_args("export-scope-selected", &[("name", sel_name.into())]),
        ExportScope::CurrentDir => app
            .catalog
            .t_args("export-scope-dir", &[("dir", cur_dir_label.into())]),
        ExportScope::WholeSnapshot => app.catalog.t("export-scope-snapshot"),
    };
    let scope_box = Paragraph::new(Line::from(vec![
        Span::raw(" ◀ "),
        Span::styled(
            scope_text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ▶"),
    ]))
    .block(
        Block::default()
            .title(format!(" {} ", app.catalog.t("export-scope-label")))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if scope_focused {
                Color::Yellow
            } else {
                Color::DarkGray
            })),
    );
    frame.render_widget(scope_box, chunks[0]);

    // 2. Directory picker tree.
    app.layout.modal = chunks[1];
    let dir_focused = state.focus == ExportFocus::DirTree;
    let mut items = Vec::with_capacity(state.total_dir_items());
    if state.has_parent() {
        items.push(ListItem::new(Line::from(Span::styled(
            " ../",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))));
    }
    for sub in &state.subdirs {
        items.push(ListItem::new(format!(
            " ├─ {}/",
            btrw_core::fs_safe::format_os_display(sub)
        )));
    }
    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(state.dir_selected.min(items.len() - 1)));
    }
    let dir_title = app.catalog.t_args(
        "restore-saveas-dir",
        &[("dir", state.current_dir.display().to_string().into())],
    );
    let dir_list = List::new(items)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .highlight_style(if dir_focused {
            Style::default()
                .bg(pal.selected_bg)
                .fg(pal.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        })
        .block(
            Block::default()
                .title(format!(" {dir_title} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if dir_focused {
                    Color::Yellow
                } else {
                    Color::DarkGray
                })),
        );
    frame.render_stateful_widget(dir_list, chunks[1], &mut list_state);
    app.modal_scroll = list_state.offset();

    // 3. Filename input with visible cursor.
    let name_focused = state.focus == ExportFocus::Filename;
    let chars: Vec<char> = state.filename.chars().collect();
    let cur = state.cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (at_cur, after): (String, String) = if cur < chars.len() {
        (chars[cur].to_string(), chars[cur + 1..].iter().collect())
    } else {
        (" ".to_string(), String::new())
    };
    let name_line = if name_focused {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(before, Style::default().fg(Color::Yellow)),
            Span::styled(
                at_cur,
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(after, Style::default().fg(Color::Yellow)),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", state.filename),
            Style::default().fg(Color::White),
        ))
    };
    let name_box = Paragraph::new(name_line).block(
        Block::default()
            .title(format!(" {} ", app.catalog.t("restore-saveas-name")))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if name_focused {
                Color::Yellow
            } else {
                Color::DarkGray
            })),
    );
    frame.render_widget(name_box, chunks[2]);

    // 4. Live full target preview.
    let preview = state
        .full_dest_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_string());
    let target_line = Paragraph::new(format!(
        " {}",
        app.catalog
            .t_args("restore-saveas-target", &[("path", preview.into())])
    ))
    .style(Style::default().fg(Color::Cyan));
    frame.render_widget(target_line, chunks[3]);
}

pub(super) fn draw_config_modal(frame: &mut Frame<'_>, app: &mut App, pal: &Palette, selected: usize) {
    let popup = centered_rect(70, 65, frame.area());
    app.layout.modal = popup;
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem<'_>> = (0..App::CONFIG_ITEM_COUNT)
        .map(|i| {
            let (label, value) = app.config_item_row(i);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:<42}", truncate(&label, 42)), Style::default().fg(Color::White)),
                Span::styled(
                    format!("[{value}]"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));

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
                .title(format!(" {} ", app.catalog.t("config-title")))
                .title_bottom(format!(" {} ", app.catalog.t("config-hint")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_stateful_widget(list, popup, &mut state);
    app.modal_scroll = state.offset();
}

pub(super) fn draw_info_modal(frame: &mut Frame<'_>, app: &App, scroll: usize) {
    let popup = centered_rect(76, 75, frame.area());
    frame.render_widget(Clear, popup);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(Span::styled(
        app.catalog.t_args(
            "info-errors-hdr",
            &[("count", (app.compare.errors.len() as i64).into())],
        ),
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
    )));
    if app.compare.errors.is_empty() {
        lines.push(Line::from(format!("  {}", app.catalog.t("info-none"))));
    } else {
        for err in &app.compare.errors {
            lines.push(Line::from(format!("  {err}")));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.catalog.t_args(
            "info-excluded-hdr",
            &[("count", (app.compare.excluded.len() as i64).into())],
        ),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if app.compare.excluded.is_empty() {
        lines.push(Line::from(format!("  {}", app.catalog.t("info-none"))));
    } else {
        for ex in &app.compare.excluded {
            lines.push(Line::from(format!("  {ex}")));
        }
    }

    let height = popup.height.saturating_sub(2) as usize;
    let visible: Vec<Line<'_>> = lines.into_iter().skip(scroll).take(height).collect();
    let paragraph = Paragraph::new(visible)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(format!(" {} ", app.catalog.t("info-title")))
                .title_bottom(format!(" {} ", app.catalog.t("info-hint")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    frame.render_widget(paragraph, popup);
}


pub(super) fn draw_help_modal(frame: &mut Frame<'_>, app: &App, _pal: &Palette) {
    let popup = centered_rect(68, 70, frame.area());
    frame.render_widget(Clear, popup);

    let paragraph = Paragraph::new(app.catalog.t("help-body"))
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(format!(" {} ", app.catalog.t("help-title")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_widget(paragraph, popup);
}

