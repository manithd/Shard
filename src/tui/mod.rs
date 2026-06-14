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
    text::Span,
    widgets::{
        Block, BorderType, Borders, Cell, Gauge, List, ListItem, Paragraph, Row, Table, TableState,
        Wrap,
    },
    Frame, Terminal,
};

use crate::worker;

const ACCENT_GREEN: Color = Color::Rgb(0x5f, 0xa6, 0xf1);
const BG_DARK: Color = Color::Rgb(0x06, 0x06, 0x06);
const PANEL_BG: Color = Color::Rgb(0x21, 0x21, 0x21);
const BORDER: Color = Color::Rgb(0x40, 0x44, 0x4b);
const TEXT: Color = Color::Rgb(0xdc, 0xdd, 0xde);
const TEXT_DIM: Color = Color::Rgb(0x8e, 0x92, 0x97);

/// Run the TUI application.  Blocks until the user presses `q`.
pub fn run_tui(mut state: TuiState) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut progress_rx: Option<std::sync::mpsc::Receiver<worker::ProgressUpdate>> = None;

    // If source is set and files are already scanned (from CLI flags), start with them.
    if !state.scanned_files.is_empty() {
        let rx = state.start_conversion()?;
        progress_rx = Some(rx);
    }

    loop {
        // Drain pending progress updates.
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

fn draw_ui(f: &mut Frame, state: &TuiState) {
    // ── Outer layout: main area + bottom progress bar ──────
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    // ── Body: sidebar + main pane ──────────────────────────
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, body[0], state);
    draw_main_pane(f, body[1], state);
    draw_progress_bar(f, outer[1], state);
}

fn draw_sidebar(f: &mut Frame, area: Rect, state: &TuiState) {
    let is_focused = state.selected_panel == Panel::Settings;
    let border_style = if is_focused {
        Style::default().fg(ACCENT_GREEN)
    } else {
        Style::default().fg(BORDER)
    };

    let total = state.files.len();
    let selected = state.files.iter().filter(|e| e.selected).count();

    let info = format!(
        "Source: {}\n\
         Output: {}\n\
         DPI:    {}\n\
         Files:  {} total, {} selected\n\
         \n\
         \n\
         Controls:\n\
         Tab      switch panel\n\
         ↑↓       navigate list\n\
         Space    toggle file\n\
         a / n    select / deselect all\n\
         Enter    start conversion\n\
         s        stop (while running)\n\
         q / Esc  quit",
        state.config.source_path.display(),
        state.config.output_path.display(),
        state.config.dpi,
        total,
        selected,
    );

    let paragraph = Paragraph::new(info)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .title(" Settings ")
                .title_style(Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD)),
        )
        .style(Style::default().fg(TEXT));

    f.render_widget(paragraph, area);
}

fn draw_main_pane(f: &mut Frame, area: Rect, state: &TuiState) {
    // Split into file list (top) and log (bottom).
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
        .split(area);

    draw_file_list(f, split[0], state);
    draw_log(f, split[1], state);
}

fn draw_file_list(f: &mut Frame, area: Rect, state: &TuiState) {
    let is_focused = state.selected_panel == Panel::FileList;
    let border_style = if is_focused {
        Style::default().fg(ACCENT_GREEN)
    } else {
        Style::default().fg(BORDER)
    };

    let items: Vec<ListItem> = state
        .files
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let (icon, status_style): (&str, Style) = match &entry.status {
                FileStatus::Done => ("✓", Style::default().fg(Color::Rgb(0x66, 0xbb, 0x6a))),
                FileStatus::Error(_) => ("✗", Style::default().fg(Color::Rgb(0xef, 0x53, 0x50))),
                FileStatus::Processing => ("▶", Style::default().fg(Color::Rgb(0x42, 0x57, 0x44))),
                FileStatus::Skipped => ("○", Style::default().fg(Color::Rgb(0xff, 0xa7, 0x26))),
                FileStatus::Queued => (" ", Style::default()),
            };

            let sel = if entry.selected { "☑" } else { "☐" };
            let line = format!("{} {} {}", sel, icon, entry.relative_path);

            let mut style = status_style;
            if is_focused && i == state.list_cursor {
                style = style.bg(PANEL_BG).add_modifier(Modifier::REVERSED);
            }

            ListItem::new(line).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .title(" Files ")
                .title_style(Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_widget(list, area);
}

fn draw_log(f: &mut Frame, area: Rect, state: &TuiState) {
    // Show last N log lines.
    let n = (area.height as usize)
        .saturating_sub(2)
        .min(state.log_lines.len());
    let start = state.log_lines.len().saturating_sub(n);
    let text: String = state.log_lines[start..].join("\n");

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .title(" Log ")
                .title_style(Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD)),
        )
        .style(Style::default().fg(TEXT_DIM));

    f.render_widget(paragraph, area);
}

fn draw_progress_bar(f: &mut Frame, area: Rect, state: &TuiState) {
    let total = state.files.len() as u32;
    let done = state.processed_count + state.error_count + state.skipped_count;
    let ratio = if total > 0 {
        done as f64 / total as f64
    } else {
        0.0
    };

    let label = if state.finished {
        state.summary.as_deref().unwrap_or("Complete").to_string()
    } else if state.processing {
        format!(
            "{} done, {} errors, {} skipped — {}%",
            state.processed_count,
            state.error_count,
            state.skipped_count,
            (ratio * 100.0) as u32
        )
    } else if state.files.is_empty() {
        "No files loaded — use Enter to scan and start".into()
    } else {
        format!(
            "{} / {} files selected — Enter to start",
            state.files.iter().filter(|e| e.selected).count(),
            total
        )
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER)),
        )
        .gauge_style(Style::default().fg(ACCENT_GREEN).bg(PANEL_BG))
        .ratio(ratio)
        .label(label);

    f.render_widget(gauge, area);
}
