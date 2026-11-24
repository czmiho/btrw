use super::{Palette, centered_rect};
use crate::app::{App, TimeTravelState};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use btrw_core::compare::{DiffLineKind, SideBySideRow, SideRowKind};
use btrw_core::config::TimeTravelHighlight;
use btrw_core::timetravel::TimeTravelLineKind;

pub(super) fn draw_diff_menu(frame: &mut Frame<'_>, app: &mut App, pal: &Palette, selected: usize) {
    let popup = centered_rect(58, 34, frame.area());
    app.layout.modal = popup;
    frame.render_widget(Clear, popup);

    let items = vec![
        ListItem::new(format!(" 1. {}", app.catalog.t("diff-menu-export"))),
        ListItem::new(format!(" 2. {}", app.catalog.t("diff-menu-dir"))),
        ListItem::new(format!(" 3. {}", app.catalog.t("diff-menu-snapshot"))),
        ListItem::new(format!(" 4. {}", app.catalog.t("diff-menu-tree"))),
    ];
    let mut state = ListState::default();
    state.select(Some(selected.min(items.len() - 1)));

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
                .title(format!(" {} ", app.catalog.t("diff-menu-title")))
                .title_bottom(format!(" {} ", app.catalog.t("diff-menu-hint")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_stateful_widget(list, popup, &mut state);
    app.modal_scroll = state.offset();
}


#[allow(clippy::too_many_arguments)]
pub(super) fn draw_diff_view(
    frame: &mut Frame<'_>,
    app: &App,
    title: &str,
    lines: &[btrw_core::compare::DiffLine],
    side_rows: Option<&[SideBySideRow]>,
    side_by_side: bool,
    scroll_y: usize,
    scroll_x: usize,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let height = area.height.saturating_sub(2) as usize;
    let rendered: Vec<Line<'_>> = if side_by_side && let Some(srows) = side_rows {
        let inner_w = area.width.saturating_sub(2) as usize;
        let col_w = inner_w.saturating_sub(3) / 2;
        let text_w = col_w.saturating_sub(6);
        srows
            .iter()
            .skip(scroll_y)
            .take(height)
            .map(|r| {
                let lno = r
                    .left_no
                    .map(|n| format!("{n:>4} "))
                    .unwrap_or_else(|| "     ".to_string());
                let rno = r
                    .right_no
                    .map(|n| format!("{n:>4} "))
                    .unwrap_or_else(|| "     ".to_string());
                let lt: String = r.left_text.chars().skip(scroll_x).take(text_w).collect();
                let rt: String = r.right_text.chars().skip(scroll_x).take(text_w).collect();
                let (lstyle, rstyle) = match r.kind {
                    SideRowKind::Equal => {
                        (Style::default().fg(Color::White), Style::default().fg(Color::White))
                    }
                    SideRowKind::Changed => (
                        Style::default().fg(Color::Yellow),
                        Style::default().fg(Color::Yellow),
                    ),
                    SideRowKind::Removed => (
                        Style::default().fg(Color::Red),
                        Style::default().fg(Color::DarkGray),
                    ),
                    SideRowKind::Added => (
                        Style::default().fg(Color::DarkGray),
                        Style::default().fg(Color::Green),
                    ),
                };
                Line::from(vec![
                    Span::styled(lno, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{lt:<text_w$}"), lstyle),
                    Span::styled(" │ ", Style::default().fg(Color::Cyan)),
                    Span::styled(rno, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{rt:<text_w$}"), rstyle),
                ])
            })
            .collect()
    } else {
        lines
            .iter()
            .skip(scroll_y)
            .take(height)
            .map(|l| {
                let sliced: String = l.text.chars().skip(scroll_x).collect();
                let style = match l.kind {
                    DiffLineKind::Header => Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    DiffLineKind::Hunk => Style::default().fg(Color::Cyan),
                    DiffLineKind::Added => Style::default().fg(Color::Green),
                    DiffLineKind::Removed => Style::default().fg(Color::Red),
                    DiffLineKind::Info => Style::default().fg(Color::Yellow),
                    DiffLineKind::Context => Style::default().fg(Color::White),
                };
                Line::from(Span::styled(sliced, style))
            })
            .collect()
    };

    let paragraph = Paragraph::new(rendered)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(format!(" {title} "))
                .title_bottom(format!(" {} ", app.catalog.t("diff-hint")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    frame.render_widget(paragraph, area);
}

pub(super) fn draw_timetravel_view(frame: &mut Frame<'_>, app: &App, state: &TimeTravelState) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);

    let title = app.catalog.t_args(
        "tt-title",
        &[
            ("path", state.history.rel_path.display().to_string().into()),
            ("versions", (state.history.versions.len() as i64).into()),
            ("total", (state.history.total_sources_checked as i64).into()),
        ],
    );

    // Build horizontal version timeline showing changing snapshots.
    let mut bar_spans = vec![Span::raw(" ")];
    let total = state.history.versions.len();
    let max_chips = (area.width.saturating_sub(6) as usize / 22).max(1);
    let (start, end) = if total <= max_chips {
        (0, total)
    } else {
        let half = max_chips / 2;
        let start = state.active_idx.saturating_sub(half).min(total - max_chips);
        (start, (start + max_chips).min(total))
    };

    if start > 0 {
        bar_spans.push(Span::styled("« ", Style::default().fg(Color::DarkGray)));
    }
    for i in start..end {
        let ver = &state.history.versions[i];
        let chip = ver.short_label();
        let active = i == state.active_idx;
        let pinned = state.pinned.contains(&i);
        let pin_mark = if pinned { "★ " } else { "" };
        if active {
            bar_spans.push(Span::styled(
                format!(" [{pin_mark}{chip}] "),
                Style::default()
                    .fg(Color::Black)
                    .bg(if pinned { Color::Green } else { Color::Yellow })
                    .add_modifier(Modifier::BOLD),
            ));
        } else if pinned {
            bar_spans.push(Span::styled(
                format!(" ★ {chip} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            bar_spans.push(Span::styled(
                format!(" ● {chip} "),
                Style::default().fg(Color::Cyan),
            ));
        }
        if i + 1 < end {
            bar_spans.push(Span::styled("──", Style::default().fg(Color::DarkGray)));
        }
    }
    if end < total {
        bar_spans.push(Span::styled(" »", Style::default().fg(Color::DarkGray)));
    }

    let timeline_box = Paragraph::new(Line::from(bar_spans))
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_widget(timeline_box, chunks[0]);

    let mode_label = match state.highlight {
        TimeTravelHighlight::PreviousVersion => app.catalog.t("tt-mode-prev"),
        TimeTravelHighlight::LiveState => app.catalog.t("tt-mode-live"),
        TimeTravelHighlight::None => app.catalog.t("tt-mode-none"),
    };
    let hint = if let Some(notice) = &app.notice {
        notice.clone()
    } else {
        app.catalog
            .t_args("tt-hint", &[("mode", mode_label.into())])
    };

    let ver_header = if let Some((a, b)) = state.pinned_pair() {
        let from = state.history.versions[a].short_label();
        let to = state.history.versions[b].short_label();
        format!(
            " {} ",
            app.catalog.t_args(
                "tt-pinned-header",
                &[("from", from.into()), ("to", to.into())]
            )
        )
    } else {
        let active_ver = &state.history.versions[state.active_idx];
        format!(
            " [{}/{}] {} ({}) ",
            state.active_idx + 1,
            total,
            active_ver.label,
            active_ver.short_label()
        )
    };

    let height = chunks[1].height.saturating_sub(2) as usize;
    let body_lines: Vec<Line<'_>> = state
        .rendered
        .iter()
        .skip(state.scroll_y)
        .take(height)
        .map(|l| {
            let sliced: String = l.text.chars().skip(state.scroll_x).collect();
            let lno = l
                .line_no
                .map(|n| format!("{n:>5} "))
                .unwrap_or_else(|| "      ".to_string());
            let (prefix, style) = match l.kind {
                TimeTravelLineKind::Context => ("  ", Style::default().fg(Color::White)),
                TimeTravelLineKind::Added => (
                    "+ ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                TimeTravelLineKind::Removed => ("- ", Style::default().fg(Color::Red)),
                TimeTravelLineKind::Info => ("  ", Style::default().fg(Color::Yellow)),
            };
            Line::from(vec![
                Span::styled(lno, Style::default().fg(Color::DarkGray)),
                Span::styled(prefix, style),
                Span::styled(sliced, style),
            ])
        })
        .collect();

    let body = Paragraph::new(body_lines)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .title(ver_header)
                .title_bottom(format!(" {hint} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    frame.render_widget(body, chunks[1]);
}

