use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Tabs, Wrap, Scrollbar, ScrollbarState},
    Frame,
};

use crate::banner::Banner;
use crate::DownloadStatus;

#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    display_url: &str,
    output_target: &str,
    has_valid_link: bool,
    download_status: DownloadStatus,
    download_error: Option<&str>,
    progress: f64,
    eta_text: Option<&str>,
    split_progress: f64,
    split_note: Option<&str>,
    trans_progress: f64,
    trans_note: Option<&str>,
    debug_lines: &Vec<String>,
    tabs: &Vec<String>,
    selected_tab: usize,
    transcript_text: Option<&str>,
    minutes_text: Option<&str>,
    scroll_logs: &mut u16,
    scroll_trans: &mut u16,
    scroll_minutes: &mut u16,
    logs_lines_count: &mut u16,
    trans_lines_count: &mut u16,
    minutes_lines_count: &mut u16,
    last_viewport_lines: &mut u16,
    glyph_height: usize,
    _debug_max_lines: usize,
) {
    let area = frame.size();
    let centered = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Min(glyph_height as u16 + 6)].as_ref())
        .split(area);

    let block = Block::default().title("Message").borders(Borders::ALL);
    let inner = block.inner(centered[0]);
    frame.render_widget(block, centered[0]);

    let status_lines: Vec<(String, Color)> = Vec::new();

    let status_height = status_lines.len().max(1) as u16;
    let constraints: Vec<Constraint> = if has_valid_link {
        vec![
            Constraint::Length(glyph_height as u16),
            Constraint::Length(status_height),
            Constraint::Length(6),
            Constraint::Min(glyph_height as u16),
            Constraint::Length(1), // footer
        ]
    } else {
        vec![
            Constraint::Length(glyph_height as u16),
            Constraint::Length(status_height),
            Constraint::Min(glyph_height as u16),
            Constraint::Length(1), // footer
        ]
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let header = Banner::new("rip Video").style(Style::new().fg(Color::Yellow)).spacing(0);
    frame.render_widget(header, sections[0]);

    if !status_lines.is_empty() {
        let lines: Vec<Line> = status_lines
            .into_iter()
            .map(|(text, color)| Line::styled(text, Style::new().fg(color)))
            .collect();
        let status_paragraph = Paragraph::new(lines).alignment(Alignment::Center);
        frame.render_widget(status_paragraph, sections[1]);
    }

    if has_valid_link {
        let gauge_area = sections[2];
        let body_area = sections[3];

        // Split gauge_area into three rows for the three pipeline stages
        let gauge_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
            ])
            .split(gauge_area);

        // Download gauge
        let dl_label = if let Some(eta) = eta_text {
            format!("dl: {:.0}% ETA {}", (progress * 100.0).min(100.0), eta)
        } else {
            format!("dl: {:.0}%", (progress * 100.0).min(100.0))
        };
        let dl_gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::LightGreen))
            .use_unicode(true)
            .ratio(progress.min(1.0));
        frame.render_widget(dl_gauge, gauge_rows[0]);
        let dl_line = colored_gauge_label(&dl_label, gauge_rows[0].width, progress);
        let dl_para = Paragraph::new(dl_line).alignment(Alignment::Center);
        frame.render_widget(dl_para, gauge_rows[0]);

        // Split gauge
        let split_label = if let Some(note) = split_note {
            format!("split: {:.0}% ({})", (split_progress * 100.0).min(100.0), note)
        } else {
            format!("split: {:.0}%", (split_progress * 100.0).min(100.0))
        };
        let split_gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Yellow))
            .use_unicode(true)
            .ratio(split_progress.min(1.0));
        frame.render_widget(split_gauge, gauge_rows[1]);
        let split_line = colored_gauge_label(&split_label, gauge_rows[1].width, split_progress);
        let split_para = Paragraph::new(split_line).alignment(Alignment::Center);
        frame.render_widget(split_para, gauge_rows[1]);

        // Transcribe gauge
        let trans_label = if let Some(note) = trans_note {
            format!(
                "transcribe: {:.0}% ({})",
                (trans_progress * 100.0).min(100.0),
                note
            )
        } else {
            format!("transcribe: {:.0}%", (trans_progress * 100.0).min(100.0))
        };
        let trans_gauge = Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan))
            .use_unicode(true)
            .ratio(trans_progress.min(1.0));
        frame.render_widget(trans_gauge, gauge_rows[2]);
        let trans_line = colored_gauge_label(&trans_label, gauge_rows[2].width, trans_progress);
        let trans_para = Paragraph::new(trans_line).alignment(Alignment::Center);
        frame.render_widget(trans_para, gauge_rows[2]);

        // Body: tabs header, content area (URL moved to its own tab)
        let body_constraints = vec![Constraint::Length(3), Constraint::Min(glyph_height as u16)];

        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints(body_constraints)
            .split(body_area);

        // Tabs header
        let tabs_area = split[0];
        let titles: Vec<Line> = tabs.iter().cloned().map(Line::from).collect();
        let tabs_widget = Tabs::new(titles)
            .select(selected_tab)
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().fg(Color::Yellow))
            .block(Block::default().title("Tabs").borders(Borders::ALL));
        frame.render_widget(tabs_widget, tabs_area);

        // Tab content with scrollbar
        let content_area = split[1];
        let (content_title, content_text): (&str, String) = match tabs.get(selected_tab).map(|s| s.as_str()) {
            Some("URL") => {
                let primary = match (download_status, has_valid_link) {
                    (DownloadStatus::Idle, true) => format!("ready: yt-dlp -> {}", output_target),
                    (DownloadStatus::Running, true) => format!("downloader: yt-dlp running -> {}", output_target),
                    (DownloadStatus::Success, true) => format!("download complete -> {}", output_target),
                    (DownloadStatus::Failed, true) => format!(
                        "download failed -> {}: {}",
                        output_target,
                        download_error.unwrap_or("unknown error")
                    ),
                    _ => String::new(),
                };
                let secondary = if matches!(download_status, DownloadStatus::Running) {
                    if let Some(eta) = eta_text { format!("progress: {:.1}% ETA {}", progress * 100.0, eta) }
                    else { format!("progress: {:.1}%", progress * 100.0) }
                } else { String::new() };
                let mut body = String::new();
                if !primary.is_empty() { body.push_str(&primary); body.push('\n'); }
                if !secondary.is_empty() { body.push_str(&secondary); body.push('\n'); }
                body.push_str(display_url);
                ("URL", body)
            }
            Some("Transcription") => (
                "Transcription",
                transcript_text.unwrap_or("(no transcript yet)").to_string(),
            ),
            Some("Minutes") => (
                "Minutes",
                minutes_text.unwrap_or("(minutes not generated)").to_string(),
            ),
            _ => ("Logs", debug_lines.join("\n")),
        };
        let content_block = Block::default().title(content_title).borders(Borders::ALL);
        // Split area to leave 1 column for scrollbar
        let content_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(content_area.width.saturating_sub(1)),
                Constraint::Length(1),
            ])
            .split(content_area);

        // Count lines and clamp scroll per tab
        let inner_height = content_split[0].height.saturating_sub(2);
        let inner_width = content_split[0].width.saturating_sub(2);
        let lines_count = wrapped_lines_count(&content_text, inner_width);
        *last_viewport_lines = inner_height;
        match tabs.get(selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => {
                *trans_lines_count = lines_count;
                if *trans_lines_count > inner_height {
                    let max_scroll = *trans_lines_count - inner_height;
                    if *scroll_trans > max_scroll { *scroll_trans = max_scroll; }
                } else { *scroll_trans = 0; }
            }
            Some("Minutes") => {
                *minutes_lines_count = lines_count;
                if *minutes_lines_count > inner_height {
                    let max_scroll = *minutes_lines_count - inner_height;
                    if *scroll_minutes > max_scroll { *scroll_minutes = max_scroll; }
                } else { *scroll_minutes = 0; }
            }
            _ => {
                *logs_lines_count = lines_count;
                if *logs_lines_count > inner_height {
                    let max_scroll = *logs_lines_count - inner_height;
                    if *scroll_logs > max_scroll { *scroll_logs = max_scroll; }
                } else { *scroll_logs = 0; }
            }
        }

        let y_scroll = match tabs.get(selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => *scroll_trans,
            Some("Minutes") => *scroll_minutes,
            _ => *scroll_logs,
        };
        let content_para = Paragraph::new(content_text)
            .wrap(Wrap { trim: true })
            .block(content_block)
            .alignment(Alignment::Left)
            .style(Style::new().fg(match tabs.get(selected_tab).map(|s| s.as_str()) {
                Some("URL") => Color::White,
                _ => Color::DarkGray,
            }))
            .scroll((y_scroll, 0));
        frame.render_widget(content_para, content_split[0]);

        // Scrollbar for the right column
        let total_lines = lines_count.max(1) as usize;
        let mut sb_state = ScrollbarState::new(total_lines).position(y_scroll as usize);
        let sb = Scrollbar::default()
            .thumb_style(Style::default().fg(Color::Gray))
            .track_style(Style::default().fg(Color::DarkGray))
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"));
        frame.render_stateful_widget(sb, content_split[1], &mut sb_state);

        // Footer hint
        let footer_area = sections[4];
        let footer = Paragraph::new("← → tabs • ↑ ↓ scroll • t=Transcription • m=Minutes • q=Exit")
            .alignment(Alignment::Center)
            .style(Style::new().fg(Color::DarkGray));
        frame.render_widget(footer, footer_area);
    } else {
        let body_area = sections[2];
        let hint = Paragraph::new(
            "go to chrome terminal, find a url for `videomanifest`, and copy a URL",
        )
        .alignment(Alignment::Center)
        .style(Style::new().fg(Color::Red));
        frame.render_widget(hint, body_area);
        // Footer hint
        let footer_area = sections[3];
        let footer = Paragraph::new("← → tabs • ↑ ↓ scroll • t=Transcription • m=Minutes • q=Exit")
            .alignment(Alignment::Center)
            .style(Style::new().fg(Color::DarkGray));
        frame.render_widget(footer, footer_area);
    }
}

// Removed unused chunk_text_to_width; wrapped_lines_count is used instead

fn wrapped_lines_count(text: &str, inner_width: u16) -> u16 {
    let w = inner_width.max(1) as usize;
    let mut total: u16 = 0;
    for line in text.lines() {
        let len = line.chars().count();
        let chunks = ((len + w - 1) / w).max(1);
        total = total.saturating_add(chunks as u16);
    }
    if total == 0 { 1 } else { total }
}

fn colored_gauge_label(label: &str, row_width: u16, ratio: f64) -> Line<'static> {
    let label_chars: Vec<char> = label.chars().collect();
    let label_len = label_chars.len() as u16;
    let width = row_width;
    let start_col = width.saturating_sub(label_len) / 2; // centered start
    let fill_cols = (ratio.max(0.0).min(1.0) * (width as f64)).floor() as u16;

    let overlap = if fill_cols > start_col {
        (fill_cols - start_col).min(label_len)
    } else {
        0
    } as usize;

    let (left, right) = if overlap > 0 {
        let left: String = label_chars.iter().take(overlap).collect();
        let right: String = label_chars.iter().skip(overlap).collect();
        (left, right)
    } else {
        (String::new(), label.to_string())
    };

    let mut spans: Vec<Span> = Vec::new();
    if !left.is_empty() {
        spans.push(Span::styled(left, Style::default().fg(Color::White).bg(Color::Black)));
    }
    if !right.is_empty() {
        spans.push(Span::styled(right, Style::default().fg(Color::White)));
    }
    Line::from(spans)
}
