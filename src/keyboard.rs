use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

#[allow(clippy::too_many_arguments)]
pub fn handle_key(
    key: &KeyEvent,
    tabs: &[String],
    selected_tab: &mut usize,
    scroll_logs: &mut u16,
    scroll_trans: &mut u16,
    scroll_minutes: &mut u16,
    logs_lines_count: u16,
    trans_lines_count: u16,
    minutes_lines_count: u16,
    last_viewport_lines: u16,
) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
            return true;
        }
        KeyCode::Left => {
            if *selected_tab > 0 {
                *selected_tab -= 1;
            }
        }
        KeyCode::Right => {
            if *selected_tab + 1 < tabs.len() {
                *selected_tab += 1;
            }
        }
        KeyCode::Char('t') => {
            if let Some(idx) = tabs.iter().position(|t| t == "Transcription") {
                *selected_tab = idx;
            }
        }
        KeyCode::Char('u') => {
            if let Some(idx) = tabs.iter().position(|t| t == "URL") {
                *selected_tab = idx;
            }
        }
        KeyCode::Char('m') => {
            if let Some(idx) = tabs.iter().position(|t| t == "Minutes") {
                *selected_tab = idx;
            }
        }
        KeyCode::Up => match tabs.get(*selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => if *scroll_trans > 0 { *scroll_trans -= 1; },
            Some("Minutes") => if *scroll_minutes > 0 { *scroll_minutes -= 1; },
            _ => if *scroll_logs > 0 { *scroll_logs -= 1; },
        },
        KeyCode::Down => match tabs.get(*selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => {
                let max = trans_lines_count.saturating_sub(last_viewport_lines);
                if *scroll_trans < max { *scroll_trans += 1; }
            }
            Some("Minutes") => {
                let max = minutes_lines_count.saturating_sub(last_viewport_lines);
                if *scroll_minutes < max { *scroll_minutes += 1; }
            }
            _ => {
                let max = logs_lines_count.saturating_sub(last_viewport_lines);
                if *scroll_logs < max { *scroll_logs += 1; }
            }
        },
        KeyCode::PageUp => {
            let step = last_viewport_lines.max(1);
            match tabs.get(*selected_tab).map(|s| s.as_str()) {
                Some("Transcription") => { *scroll_trans = scroll_trans.saturating_sub(step); }
                Some("Minutes") => { *scroll_minutes = scroll_minutes.saturating_sub(step); }
                _ => { *scroll_logs = scroll_logs.saturating_sub(step); }
            }
        }
        KeyCode::PageDown => {
            let step = last_viewport_lines.max(1);
            match tabs.get(*selected_tab).map(|s| s.as_str()) {
                Some("Transcription") => {
                    let max = trans_lines_count.saturating_sub(last_viewport_lines);
                    *scroll_trans = (*scroll_trans + step).min(max);
                }
                Some("Minutes") => {
                    let max = minutes_lines_count.saturating_sub(last_viewport_lines);
                    *scroll_minutes = (*scroll_minutes + step).min(max);
                }
                _ => {
                    let max = logs_lines_count.saturating_sub(last_viewport_lines);
                    *scroll_logs = (*scroll_logs + step).min(max);
                }
            }
        }
        KeyCode::Home => match tabs.get(*selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => *scroll_trans = 0,
            Some("Minutes") => *scroll_minutes = 0,
            _ => *scroll_logs = 0,
        },
        KeyCode::End => match tabs.get(*selected_tab).map(|s| s.as_str()) {
            Some("Transcription") => {
                *scroll_trans = trans_lines_count.saturating_sub(last_viewport_lines);
            }
            Some("Minutes") => {
                *scroll_minutes = minutes_lines_count.saturating_sub(last_viewport_lines);
            }
            _ => {
                *scroll_logs = logs_lines_count.saturating_sub(last_viewport_lines);
            }
        },
        _ => {}
    }

    false
}
