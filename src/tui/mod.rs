mod state;
pub use state::*;

use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};

use crate::worker;

// ── Palette ─────────────────────────────────────────────
const BG: Color = Color::Rgb(0x20, 0x22, 0x25);
const SURFACE: Color = Color::Rgb(0x2f, 0x31, 0x36);
const BORDER: Color = Color::Rgb(0x40, 0x44, 0x4b);
const BORDER_FOCUS: Color = Color::Rgb(0x42, 0x57, 0x44);
const TEXT: Color = Color::Rgb(0xdc, 0xdd, 0xde);
const TEXT_DIM: Color = Color::Rgb(0x8e, 0x92, 0x97);
const TEXT_MUTED: Color = Color::Rgb(0x50, 0x54, 0x5c);
const ACCENT: Color = Color::Rgb(0x42, 0x57, 0x44);
const GREEN: Color = Color::Rgb(0x66, 0xbb, 0x6a);
const RED: Color = Color::Rgb(0xef, 0x53, 0x50);
const ORANGE: Color = Color::Rgb(0xff, 0xa7, 0x26);
const BLUE: Color = Color::Rgb(0x42, 0x9b, 0xd4);

/// Run the TUI application.  Blocks until the user presses `q`.
pub fn run_tui(mut state: TuiState) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut progress_rx: Option<std::sync::mpsc::Receiver<worker::ProgressUpdate>> = None;

    if !state.scanned_files.is_empty() {
        let rx = state.start_conversion()?;
        progress_rx = Some(rx);
    }

    loop {
        if let Some(ref rx) = progress_rx {
            while let Ok(update) = rx.try_recv() {
                state.apply_update(update);
            }
        }

        terminal.draw(|f| draw_ui(f, &mut state))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Tab => state.toggle_panel(),
                        KeyCode::Up | KeyCode::Char('k') => state.move_cursor(-1),
                        KeyCode::Down | KeyCode::Char('j') => state.move_cursor(1),
                        KeyCode::Enter if !state.processing => {
                            if !state.files.is_empty() {
                                let rx = state.start_conversion()?;
                                progress_rx = Some(rx);
                            }
                        }
                        KeyCode::Char(' ') => state.toggle_selected_file(),
                        KeyCode::Char('a') => state.select_all(true),
                        KeyCode::Char('n') => state.select_all(false),
                        KeyCode::Char('s') if state.processing => state.stop_conversion(),
                        _ => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

// ── Layout ──────────────────────────────────────────────

fn draw_ui(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(0)])
        .split(outer[1]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(8)])
        .split(body[1]);

    draw_header(f, outer[0], state);
    draw_sidebar(f, body[0], state);
    draw_file_list(f, right[0], state);
    draw_log(f, right[1], state);
    draw_status_bar(f, outer[2], state);
}

// ── Header ──────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect, state: &TuiState) {
    let total = state.files.len();
    let selected = state.files.iter().filter(|e| e.selected).count();

    let left = Span::styled(
        " ◈ pdf2webp ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    );

    let mode_text = if state.processing {
        format!(
            "  CONVERTING  {}/{} files ",
            state.processed_count + state.error_count + state.skipped_count,
            total
        )
    } else if state.finished {
        format!(
            "  COMPLETE  {} ok  {} err  {} skipped ",
            state.processed_count, state.error_count, state.skipped_count
        )
    } else if total > 0 {
        format!("  READY  {} files  {} selected ", total, selected)
    } else {
        "  NO FILES  scan to begin ".to_string()
    };

    let mode_color = if state.processing {
        BLUE
    } else if state.finished {
        GREEN
    } else if total > 0 {
        ACCENT
    } else {
        TEXT_MUTED
    };

    let right = Span::styled(
        mode_text,
        Style::default()
            .fg(BG)
            .bg(mode_color)
            .add_modifier(Modifier::BOLD),
    );

    let left_w = 13u16;
    let right_w = right.content.len() as u16;
    let mid_w = area.width.saturating_sub(left_w + right_w);
    let middle = Span::styled(" ".repeat(mid_w as usize), Style::default().bg(SURFACE));

    let line = Line::from(vec![left, middle, right]);
    let paragraph = Paragraph::new(line).style(Style::default().bg(SURFACE));
    f.render_widget(paragraph, area);
}

// ── Sidebar ─────────────────────────────────────────────

fn draw_sidebar(f: &mut Frame, area: Rect, state: &TuiState) {
    let is_focused = state.selected_panel == Panel::Settings;
    let border_col = if is_focused { BORDER_FOCUS } else { BORDER };

    let max_w = area.width.saturating_sub(4) as usize;
    let trunc = |s: String| -> String {
        if s.len() <= max_w {
            s
        } else {
            format!("…{}", &s[s.len().saturating_sub(max_w.saturating_sub(1))..])
        }
    };

    let src = trunc(state.config.source_path.display().to_string());
    let out = trunc(state.config.output_path.display().to_string());
    let total = state.files.len();
    let sel = state.files.iter().filter(|e| e.selected).count();
    let done = state.processed_count;
    let errs = state.error_count;

    let lines: Vec<Line> = vec![
        Line::from(Span::styled(
            " PATHS",
            Style::default().fg(TEXT_MUTED).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  src  ", Style::default().fg(TEXT_DIM)),
            Span::styled(src, Style::default().fg(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  out  ", Style::default().fg(TEXT_DIM)),
            Span::styled(out, Style::default().fg(TEXT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " CONFIG",
            Style::default().fg(TEXT_MUTED).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  dpi  ", Style::default().fg(TEXT_DIM)),
            Span::styled(state.config.dpi.to_string(), Style::default().fg(TEXT)),
        ]),
        Line::from(vec![
            Span::styled("  fmt  ", Style::default().fg(TEXT_DIM)),
            Span::styled("webp", Style::default().fg(TEXT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " FILES",
            Style::default().fg(TEXT_MUTED).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                format!("  {} total  ", total),
                Style::default().fg(TEXT_DIM),
            ),
            Span::styled(format!("{} selected", sel), Style::default().fg(TEXT)),
        ]),
        Line::from(vec![
            Span::styled(format!("  ✓ {}  ", done), Style::default().fg(GREEN)),
            Span::styled(
                format!("✗ {}", errs),
                Style::default().fg(if errs > 0 { RED } else { TEXT_MUTED }),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " KEYS",
            Style::default().fg(TEXT_MUTED).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  Tab    ", Style::default().fg(ACCENT)),
            Span::styled("switch panel", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  ↑↓ jk  ", Style::default().fg(ACCENT)),
            Span::styled("navigate", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  Space  ", Style::default().fg(ACCENT)),
            Span::styled("toggle file", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  a / n  ", Style::default().fg(ACCENT)),
            Span::styled("all / none", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  Enter  ", Style::default().fg(ACCENT)),
            Span::styled("start", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  s      ", Style::default().fg(ACCENT)),
            Span::styled("stop", Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(vec![
            Span::styled("  q/Esc  ", Style::default().fg(ACCENT)),
            Span::styled("quit", Style::default().fg(TEXT_DIM)),
        ]),
    ];

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_col))
                .title(Span::styled(" ⚙ settings ", Style::default().fg(TEXT_DIM))),
        )
        .style(Style::default().fg(TEXT));

    f.render_widget(paragraph, area);
}

// ── File list ───────────────────────────────────────────

fn draw_file_list(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let is_focused = state.selected_panel == Panel::FileList;
    let border_col = if is_focused { BORDER_FOCUS } else { BORDER };

    let inner_w = area.width.saturating_sub(4) as usize;

    // Empty state
    if state.files.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  no files loaded",
                Style::default().fg(TEXT_MUTED),
            )),
            Line::from(Span::styled(
                "  press Tab → Settings, then Enter to scan",
                Style::default().fg(TEXT_MUTED),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_col))
                .title(Span::styled(" ◉ files ", Style::default().fg(TEXT_DIM))),
        );
        f.render_widget(hint, area);
        return;
    }

    let items: Vec<ListItem> = state
        .files
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let sel_glyph = if entry.selected { "▣" } else { "▢" };
            let sel_style = if entry.selected {
                Style::default().fg(ACCENT)
            } else {
                Style::default().fg(TEXT_MUTED)
            };

            let (status_glyph, status_style) = match &entry.status {
                FileStatus::Done => ("✓", Style::default().fg(GREEN)),
                FileStatus::Error(_) => ("✗", Style::default().fg(RED)),
                FileStatus::Processing => {
                    ("▶", Style::default().fg(BLUE).add_modifier(Modifier::BOLD))
                }
                FileStatus::Skipped => ("○", Style::default().fg(ORANGE)),
                FileStatus::Queued => ("·", Style::default().fg(TEXT_MUTED)),
            };

            let page_suffix =
                if matches!(entry.status, FileStatus::Processing) && entry.total_pages > 0 {
                    format!(" p{}/{}", entry.current_page, entry.total_pages)
                } else {
                    String::new()
                };

            let prefix_len = 6usize;
            let suffix_len = page_suffix.len();
            let max_path = inner_w.saturating_sub(prefix_len + suffix_len + 3);
            let path_display = if entry.relative_path.len() > max_path {
                format!(
                    "…{}",
                    &entry.relative_path[entry
                        .relative_path
                        .len()
                        .saturating_sub(max_path.saturating_sub(1))..]
                )
            } else {
                entry.relative_path.clone()
            };

            let path_style = match &entry.status {
                FileStatus::Done => Style::default().fg(GREEN),
                FileStatus::Error(_) => Style::default().fg(RED),
                FileStatus::Processing => Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                FileStatus::Skipped => Style::default().fg(ORANGE),
                FileStatus::Queued => Style::default().fg(TEXT),
            };

            let highlight = is_focused && i == state.list_cursor;
            let row_bg = if highlight { SURFACE } else { BG };

            let mut spans = vec![
                Span::styled(format!("{sel_glyph} "), sel_style.bg(row_bg)),
                Span::styled(format!("{status_glyph} "), status_style.bg(row_bg)),
                Span::styled(path_display, path_style.bg(row_bg)),
            ];

            if !page_suffix.is_empty() {
                spans.push(Span::styled(
                    page_suffix,
                    Style::default().fg(BLUE).bg(row_bg),
                ));
            }

            if highlight {
                spans.push(Span::styled(
                    " ◀",
                    Style::default().fg(BORDER_FOCUS).bg(row_bg),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = format!(
        " ◉ files  {} total  {} selected ",
        state.files.len(),
        state.files.iter().filter(|e| e.selected).count()
    );

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_col))
                .title(Span::styled(title, Style::default().fg(TEXT_DIM))),
        )
        .highlight_style(Style::default().bg(SURFACE));

    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(state.list_cursor));
    f.render_stateful_widget(list, area, &mut list_state);
}

// ── Log panel ───────────────────────────────────────────

fn draw_log(f: &mut Frame, area: Rect, state: &TuiState) {
    let inner_h = area.height.saturating_sub(2) as usize;
    let start = state.log_lines.len().saturating_sub(inner_h);

    let lines: Vec<Line> = state.log_lines[start..]
        .iter()
        .map(|msg| {
            let style = if msg.starts_with('✓') {
                Style::default().fg(GREEN)
            } else if msg.starts_with('✗') {
                Style::default().fg(RED)
            } else if msg.starts_with('⟳') {
                Style::default().fg(BLUE)
            } else {
                Style::default().fg(TEXT_MUTED)
            };
            Line::from(Span::styled(format!("  {msg}"), style))
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .title(Span::styled(" ≡ log ", Style::default().fg(TEXT_MUTED))),
    );

    f.render_widget(paragraph, area);
}

// ── Status bar ──────────────────────────────────────────

fn draw_status_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let total = state.files.len() as u32;
    let done = state.processed_count + state.error_count + state.skipped_count;
    let ratio = if total > 0 {
        done as f64 / total as f64
    } else {
        0.0
    };

    // Progress bar string
    let bar_w = (area.width.saturating_sub(30)) as usize;
    let filled = (ratio * bar_w as f64).round() as usize;
    let empty = bar_w.saturating_sub(filled);
    let bar_str = format!("{}{}", "█".repeat(filled), "░".repeat(empty));

    // Status message
    let status_msg = if state.finished {
        state
            .summary
            .as_deref()
            .unwrap_or("Conversion complete")
            .to_string()
    } else if state.processing {
        if let Some(entry) = state
            .files
            .iter()
            .find(|e| matches!(e.status, FileStatus::Processing))
        {
            if entry.total_pages > 0 {
                format!(
                    "▶ {}  page {}/{}",
                    entry.relative_path, entry.current_page, entry.total_pages
                )
            } else {
                format!("▶ {}", entry.relative_path)
            }
        } else {
            "Processing…".to_string()
        }
    } else if total > 0 {
        let sel = state.files.iter().filter(|e| e.selected).count();
        format!("{sel} of {total} files selected — press Enter to start")
    } else {
        "Select source folder and scan for PDFs".to_string()
    };

    // Counters
    let pct = (ratio * 100.0) as u32;
    let counter_str = if total > 0 {
        format!(
            "  ✓ {}  ✗ {}  ○ {}  {:>3}%",
            state.processed_count, state.error_count, state.skipped_count, pct
        )
    } else {
        String::new()
    };

    let bar_color = if state.finished {
        GREEN
    } else if state.processing {
        ACCENT
    } else {
        TEXT_MUTED
    };
    let bg_color = if state.processing { SURFACE } else { BG };

    let content = vec![
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(bar_str, Style::default().fg(bar_color)),
            Span::styled(counter_str, Style::default().fg(TEXT_DIM)),
        ]),
        Line::from(Span::styled(
            format!("  {status_msg}"),
            Style::default().fg(if state.processing { TEXT } else { TEXT_DIM }),
        )),
    ];

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        )
        .style(Style::default().bg(bg_color));

    f.render_widget(paragraph, area);
}
