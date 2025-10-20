use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, LineGauge, Paragraph, Tabs, Wrap, Scrollbar, ScrollbarState},
    Frame,
};
#[cfg(feature = "markdown")]
use tui_markdown::Markdown;

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
    minutes_progress: f64,
    minutes_note: Option<&str>,
    summary_progress: f64,
    summary_note: Option<&str>,
    debug_lines: &[String],
    tabs: &[String],
    selected_tab: usize,
    transcript_text: Option<&str>,
    minutes_text: Option<&str>,
    summary_text: Option<&str>,
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

    let block = Block::default().title("Rip Video").borders(Borders::ALL);
    let inner = block.inner(centered[0]);
    frame.render_widget(block, centered[0]);

    let status_lines: Vec<(String, Color)> = Vec::new();

    let status_height = status_lines.len().max(1) as u16;
    let constraints: Vec<Constraint> = if has_valid_link {
        vec![
            Constraint::Length(glyph_height as u16),
            Constraint::Length(status_height),
            Constraint::Length(5),
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

        // Split gauge_area into five single-line rows
        let gauge_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(gauge_area);

        // Download gauge
        let dl_label = if let Some(eta) = eta_text {
            format!("dl: {:.0}% ETA {}", (progress * 100.0).min(100.0), eta)
        } else {
            format!("dl: {:.0}%", (progress * 100.0).min(100.0))
        };
        let dl_gauge = LineGauge::default()
            .label(dl_label)
            .gauge_style(Style::default().fg(Color::Red))
            .ratio(progress.min(1.0));
        frame.render_widget(dl_gauge, gauge_rows[0]);

        // Split gauge
        let split_label = if let Some(note) = split_note {
            format!("split: {:.0}% ({})", (split_progress * 100.0).min(100.0), note)
        } else {
            format!("split: {:.0}%", (split_progress * 100.0).min(100.0))
        };
        let split_gauge = LineGauge::default()
            .label(split_label)
            .gauge_style(Style::default().fg(Color::Yellow))
            .ratio(split_progress.min(1.0));
        frame.render_widget(split_gauge, gauge_rows[1]);

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
        let trans_gauge = LineGauge::default()
            .label(trans_label)
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(trans_progress.min(1.0));
        frame.render_widget(trans_gauge, gauge_rows[2]);

        // Minutes gauge (time-to-first-output)
        let minutes_label = if let Some(note) = minutes_note {
            format!(
                "minutes: {:.0}% ({})",
                (minutes_progress * 100.0).min(100.0),
                note
            )
        } else {
            format!("minutes: {:.0}%", (minutes_progress * 100.0).min(100.0))
        };
        let minutes_gauge = LineGauge::default()
            .label(minutes_label)
            .gauge_style(Style::default().fg(Color::Rgb(64, 224, 208)))
            .ratio(minutes_progress.min(1.0));
        frame.render_widget(minutes_gauge, gauge_rows[3]);

        // Summary gauge (warm-up while generating summary)
        let summary_label = if let Some(note) = summary_note {
            format!(
                "summary: {:.0}% ({})",
                (summary_progress * 100.0).min(100.0),
                note
            )
        } else {
            format!("summary: {:.0}%", (summary_progress * 100.0).min(100.0))
        };
        let summary_gauge = LineGauge::default()
            .label(summary_label)
            .gauge_style(Style::default().fg(Color::Rgb(238, 130, 238)))
            .ratio(summary_progress.min(1.0));
        frame.render_widget(summary_gauge, gauge_rows[4]);

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
            Some("Summary") => (
                "Summary",
                summary_text.unwrap_or("(no summary yet)\n\nMake sure Ollama is installed and minutes completed.").to_string(),
            ),
            Some("Q/A") => (
                "Q/A",
                "Q/A tab\n\nThis is a placeholder. Ask questions about the minutes here.".to_string(),
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
        let fudge = inner_height.max(5);
        match tabs.get(selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => {
                *trans_lines_count = lines_count;
                if *trans_lines_count > inner_height {
                    let max_scroll = trans_lines_count
                        .saturating_sub(inner_height)
                        .saturating_add(fudge);
                    if *scroll_trans > max_scroll { *scroll_trans = max_scroll; }
                } else { *scroll_trans = 0; }
            }
            Some("Minutes") => {
                *minutes_lines_count = lines_count;
                if *minutes_lines_count > inner_height {
                    let max_scroll = minutes_lines_count
                        .saturating_sub(inner_height)
                        .saturating_add(fudge);
                    if *scroll_minutes > max_scroll { *scroll_minutes = max_scroll; }
                } else { *scroll_minutes = 0; }
            }
            _ => {
                *logs_lines_count = lines_count;
                if *logs_lines_count > inner_height {
                    let max_scroll = logs_lines_count
                        .saturating_sub(inner_height)
                        .saturating_add(fudge);
                    if *scroll_logs > max_scroll { *scroll_logs = max_scroll; }
                } else { *scroll_logs = 0; }
            }
        }

        let y_scroll = match tabs.get(selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => *scroll_trans,
            Some("Minutes") => *scroll_minutes,
            _ => *scroll_logs,
        };
        let is_minutes_tab = tabs.get(selected_tab).map(|s| s.as_str()) == Some("Minutes");
        let is_summary_tab = tabs.get(selected_tab).map(|s| s.as_str()) == Some("Summary");
        let rendered_markdown;
        // If Minutes tab and markdown feature enabled, render Markdown for content after "done thinking"
        #[cfg(feature = "markdown")]
        if is_minutes_tab {
            // Extract everything after a line containing "done thinking" (case-insensitive)
            let mut after = false;
            let mut post = String::new();
            for raw in content_text.lines() {
                let lower = raw.trim().to_ascii_lowercase();
                if !after {
                    if lower.contains("done thinking") { after = true; continue; }
                } else {
                    post.push_str(raw);
                    post.push('\n');
                }
            }
            if !post.trim().is_empty() {
                // emulate vertical scroll by slicing lines window
                let lines: Vec<&str> = post.lines().collect();
                lines_count = lines.len() as u16;
                let start = (y_scroll as usize).min(lines.len());
                let end = (start + inner_height as usize).min(lines.len());
                let visible = if start < end { lines[start..end].join("\n") } else { String::new() };
                let md = Markdown::new(visible.as_str());
                frame.render_widget(md, content_split[0]);
                rendered_markdown = true;
            }
        }
        #[cfg(not(feature = "markdown"))]
        { rendered_markdown = false; }

        // Build paragraph; for Minutes, gray out "Thinking..." blocks and render simple Markdown (#, ####, **bold**, **** rule)
        let content_para = if is_minutes_tab && !rendered_markdown {
            let mut lines_vec: Vec<Line> = Vec::new();
            let mut thinking = false;
            let view_width = content_split[0].width.saturating_sub(2); // inner block width approx
            for raw in content_text.lines() {
                let t = raw.trim_start();
                let lower = t.to_ascii_lowercase();
                if lower.starts_with("thinking") { thinking = true; }
                if thinking {
                    lines_vec.push(Line::from(Span::styled(raw.to_string(), Style::default().fg(Color::DarkGray))));
                } else {
                    lines_vec.push(render_simple_markdown_line(raw, view_width));
                }
                if lower.starts_with("...done thinking") || lower.starts_with("done thinking") || lower.contains("done thinking") {
                    thinking = false;
                }
            }
            Paragraph::new(lines_vec)
                .wrap(Wrap { trim: true })
                .block(content_block)
                .alignment(Alignment::Left)
                .scroll((y_scroll, 0))
        } else if is_summary_tab {
            Paragraph::new(content_text)
                .wrap(Wrap { trim: true })
                .block(content_block)
                .alignment(Alignment::Left)
                .style(Style::new().fg(Color::White))
                .scroll((y_scroll, 0))
        } else {
            Paragraph::new(content_text)
                .wrap(Wrap { trim: true })
                .block(content_block)
                .alignment(Alignment::Left)
                .style(Style::new().fg(match tabs.get(selected_tab).map(|s| s.as_str()) {
                    Some("URL") => Color::White,
                    _ => Color::DarkGray,
                }))
                .scroll((y_scroll, 0))
        };
        if !rendered_markdown {
            frame.render_widget(content_para, content_split[0]);
        }

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
        return;
    }

    // No valid link: early return after drawing hint
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

// Removed unused chunk_text_to_width; wrapped_lines_count is used instead

fn wrapped_lines_count(text: &str, inner_width: u16) -> u16 {
    let w = inner_width.max(1) as usize;
    let mut total: u16 = 0;
    for line in text.lines() {
        let len = line.chars().count();
        let chunks = len.div_ceil(w).max(1);
        total = total.saturating_add(chunks as u16);
    }
    if total == 0 { 1 } else { total }
}

// removed colored_gauge_label; using LineGauge labels directly

fn render_simple_markdown_line(raw: &str, width: u16) -> Line<'static> {
    let t = raw.trim_end();
    let trimmed = t.trim_start();
    // Horizontal rule if line is only asterisks (*** or ****)
    if !trimmed.is_empty() && trimmed.chars().all(|c| c == '*') && trimmed.len() >= 3 {
        let w = width.max(1) as usize;
        let hr = "─".repeat(w.min(200));
        return Line::from(Span::styled(hr, Style::default().fg(Color::DarkGray)));
    }
    // Headings ####, ###, ##, #
    let mut level = 0usize;
    for ch in trimmed.chars() {
        if ch == '#' { level += 1; } else { break; }
    }
    if level > 0 && trimmed.chars().nth(level) == Some(' ') {
        let content = &trimmed[level + 1..];
        let style = Style::default().fg(Color::White);
        return Line::from(Span::styled(content.to_string(), style));
    }
    // Bold **text**
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0usize;
    let bytes = trimmed.as_bytes();
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            // find closing **
            let mut j = i + 2;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'*' { break; }
                j += 1;
            }
            if j + 1 < bytes.len() {
                let bold_text = &trimmed[i + 2..j];
                spans.push(Span::styled(bold_text.to_string(), Style::default().fg(Color::White)));
                i = j + 2;
                continue;
            }
        }
        // normal char
        let ch = bytes[i] as char;
        spans.push(Span::styled(ch.to_string(), Style::default().fg(Color::White)));
        i += 1;
    }
    Line::from(spans)
}
