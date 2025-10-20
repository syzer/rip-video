use std::{
    env,
    error::Error,
    fs,
    io::{self},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::{Backend, CrosstermBackend}};
// use crate::worker::audio_split::probe_duration_seconds; // moved to worker::transcribe

mod banner;
mod keyboard;
mod ui;
mod worker;

// Banner used within ui module

const GLYPH_HEIGHT: usize = 5;
const DEBUG_MAX_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadStatus {
    Idle,
    Running,
    Success,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Download,
    Split,
    Transcribe,
    Minutes,
    Summary,
}

pub(crate) enum WorkerMessage {
    // Stage-aware progress for multi-phase pipeline
    StageProgress { stage: Stage, ratio: f64, eta: Option<String>, note: Option<String> },
    Status(Result<(), String>),
    DownloadLog(String),
    MinutesChunk(String),
    MinutesDone(Result<(), String>),
    SummaryChunk(String),
    SummaryDone(Result<(), String>),
}

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app_result = run_app(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    app_result?;
    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()> {
    let clipboard_text = env::var("CLIPBOARD_TEXT").unwrap_or_default();
    let trimmed_clipboard = clipboard_text.trim().to_owned();
    let display_url = sanitize_clipboard(&trimmed_clipboard);
    let download_url = trimmed_clipboard.clone();
    let output_target = worker::download::resolve_ytdlp_output();
    let has_valid_link = is_valid_url(&download_url);

    let mut progress = 0.0_f64; // download progress
    let mut progress_synced = false;
    let mut split_progress: f64 = 0.0;
    let mut split_note: Option<String> = None;
    let mut trans_progress: f64 = 0.0;
    let mut trans_note: Option<String> = None;
    let mut minutes_progress: f64 = 0.0;
    let mut minutes_note: Option<String> = None;
    let mut summary_progress: f64 = 0.0;
    let mut summary_note: Option<String> = None;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(120);
    let mut download_status = DownloadStatus::Idle;
    let mut download_error: Option<String> = None;
    let mut download_rx: Option<mpsc::Receiver<WorkerMessage>> = None;
    let mut worker_cancel: Option<Arc<AtomicBool>> = None;
    let mut debug_lines: Vec<String> = Vec::new();
    let mut eta_text: Option<String> = None;
    // Tabs state: start with URL + Logs; add Transcription/Minutes on finish
    let mut tabs: Vec<String> = vec!["URL".to_string(), "Logs".to_string()];
    let mut selected_tab: usize = 0;
    let mut transcript_text: Option<String> = None;
    let mut minutes_text: Option<String> = None;
    let mut summary_text: Option<String> = None;
    // Scroll states per tab
    let mut scroll_logs: u16 = 0;
    let mut scroll_trans: u16 = 0;
    let mut scroll_minutes: u16 = 0;
    // Cached content lengths for scrolling and last viewport height
    let mut logs_lines_count: u16 = 0;
    let mut trans_lines_count: u16 = 0;
    let mut minutes_lines_count: u16 = 0;
    let mut last_viewport_lines: u16 = 0;

    loop {
        handle_download_tick(
            has_valid_link,
            &download_url,
            &output_target,
            &mut download_status,
            &mut download_rx,
            &mut worker_cancel,
            &mut progress,
            &mut progress_synced,
            &mut last_tick,
            tick_rate,
            &mut download_error,
            &mut eta_text,
            &mut split_progress,
            &mut split_note,
            &mut trans_progress,
            &mut trans_note,
            &mut minutes_progress,
            &mut minutes_note,
            &mut summary_progress,
            &mut summary_note,
            &mut debug_lines,
            &mut tabs,
            &mut selected_tab,
            &mut transcript_text,
            &mut minutes_text,
            &mut summary_text,
            &mut scroll_minutes,
        );

        terminal.draw(|frame| {
            ui::render(
                frame,
                display_url.as_str(),
                &output_target,
                has_valid_link,
                download_status,
                download_error.as_deref(),
                progress,
                eta_text.as_deref(),
                split_progress,
                split_note.as_deref(),
                trans_progress,
                trans_note.as_deref(),
                minutes_progress,
                minutes_note.as_deref(),
                summary_progress,
                summary_note.as_deref(),
                &debug_lines,
                &tabs,
                selected_tab,
                transcript_text.as_deref(),
                minutes_text.as_deref(),
                summary_text.as_deref(),
                &mut scroll_logs,
                &mut scroll_trans,
                &mut scroll_minutes,
                &mut logs_lines_count,
                &mut trans_lines_count,
                &mut minutes_lines_count,
                &mut last_viewport_lines,
                GLYPH_HEIGHT,
                DEBUG_MAX_LINES,
            );
        })?;

        if event::poll(Duration::from_millis(200))? && let Event::Key(key) = event::read()? {
                let should_exit = keyboard::handle_key(
                    &key,
                    &tabs,
                    &mut selected_tab,
                    &mut scroll_logs,
                    &mut scroll_trans,
                    &mut scroll_minutes,
                    logs_lines_count,
                    trans_lines_count,
                    minutes_lines_count,
                    last_viewport_lines,
                );
                if should_exit {
                    if let Some(cancel) = worker_cancel.take() {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    break;
                }
        }
    }

    Ok(())
}

pub(crate) fn sanitize_clipboard(input: &str) -> String {
    let trimmed = input.trim();
    if let Some((head, _)) = trimmed.split_once("&altManifestMetadata=") {
        head.trim_end().to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_valid_url(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn handle_download_tick(
    has_valid_link: bool,
    download_url: &str,
    output_target: &str,
    download_status: &mut DownloadStatus,
    download_rx: &mut Option<mpsc::Receiver<WorkerMessage>>,
    worker_cancel: &mut Option<Arc<AtomicBool>>,
    progress: &mut f64,
    progress_synced: &mut bool,
    last_tick: &mut Instant,
    tick_rate: Duration,
    download_error: &mut Option<String>,
    eta_text: &mut Option<String>,
    split_progress: &mut f64,
    split_note: &mut Option<String>,
    trans_progress: &mut f64,
    trans_note: &mut Option<String>,
    minutes_progress: &mut f64,
    minutes_note: &mut Option<String>,
    summary_progress: &mut f64,
    summary_note: &mut Option<String>,
    debug_lines: &mut Vec<String>,
    tabs: &mut Vec<String>,
    selected_tab: &mut usize,
    transcript_text: &mut Option<String>,
    minutes_text: &mut Option<String>,
    summary_text: &mut Option<String>,
    scroll_minutes: &mut u16,
) {
    // Early return to avoid deep indentation
    if !has_valid_link {
        return;
    }

    if matches!(*download_status, DownloadStatus::Idle) {
        let (tx, rx) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = Arc::clone(&cancel_flag);

        let download_link = download_url.to_string();
        let output_target_clone = output_target.to_string();
        thread::spawn(move || {
            worker::download::download_worker(
                download_link,
                output_target_clone,
                tx,
                cancel_for_thread,
            )
        });

        *download_rx = Some(rx);
        *download_status = DownloadStatus::Running;
        *progress = 0.0;
        *progress_synced = false;
        *last_tick = Instant::now();
        *worker_cancel = Some(cancel_flag);
        debug_lines.clear();
        *eta_text = None;
    }

    if let Some(rx) = download_rx.take() {
        let mut keep_rx = true;
        let disconnected = loop {
            match rx.try_recv() {
                Ok(message) => match message {
                    WorkerMessage::StageProgress { stage, ratio, eta, note } => match stage {
                        Stage::Download => {
                            *progress = ratio.clamp(0.0, 1.0);
                            *progress_synced = true;
                            *eta_text = eta;
                        }
                        Stage::Split => {
                            *split_progress = ratio.clamp(0.0, 1.0);
                            *split_note = note;
                        }
                        Stage::Transcribe => {
                            *trans_progress = ratio.clamp(0.0, 1.0);
                            *trans_note = note;
                        }
                        Stage::Minutes => {
                            *minutes_progress = ratio.clamp(0.0, 1.0);
                            *minutes_note = note;
                        }
                        Stage::Summary => {
                            *summary_progress = ratio.clamp(0.0, 1.0);
                            *summary_note = note;
                        }
                    },
                    WorkerMessage::Status(result) => {
                        *download_rx = None;
                        *worker_cancel = None;
                        match result {
                            Ok(()) => {
                                *progress = 1.0;
                                *split_progress = 1.0;
                                *trans_progress = 1.0;
                                *download_status = DownloadStatus::Success;
                                *download_error = None;
                                *eta_text = None;
                                if fs::metadata("transcript.txt").is_err() {
                                    continue;
                                }
                                *transcript_text = fs::read_to_string("transcript.txt").ok();
                                if !tabs.iter().any(|t| t == "Transcription") {
                                    tabs.push("Transcription".to_string());
                                }
                                if !tabs.iter().any(|t| t == "Minutes") {
                                    tabs.push("Minutes".to_string());
                                }
                                match worker::minutes::start_minutes_worker() {
                                    Ok((mrx, cancel_new)) => {
                                        *minutes_text = Some(String::new());
                                        *download_rx = Some(mrx);
                                        *worker_cancel = Some(cancel_new);
                                        if let Some(idx) = tabs.iter().position(|t| t == "Minutes") {
                                            *selected_tab = idx;
                                        }
                                        *scroll_minutes = 0;
                                        keep_rx = false;
                                        break false;
                                    }
                                    Err(msg) => {
                                        *minutes_text = Some(msg);
                                        if let Some(idx) = tabs.iter().position(|t| t == "Minutes") {
                                            *selected_tab = idx;
                                        }
                                        *scroll_minutes = 0;
                                        keep_rx = false;
                                        break false;
                                    }
                                }
                            }
                            Err(err) => {
                                *progress = 0.0;
                                *split_progress = 0.0;
                                *trans_progress = 0.0;
                                *download_status = DownloadStatus::Failed;
                                *download_error = Some(err);
                                *eta_text = None;
                            }
                        }
                    }
                    WorkerMessage::MinutesChunk(chunk) => match minutes_text.as_mut() {
                        Some(s) => s.push_str(&chunk),
                        None => *minutes_text = Some(chunk),
                    },
                    WorkerMessage::MinutesDone(res) => {
                        *download_rx = None;
                        *worker_cancel = None;
                        if res.is_ok() {
                            if let Some(pos) = tabs.iter().position(|t| t == "Q/A") {
                                tabs.remove(pos);
                            }
                            if let Some(pos) = tabs.iter().position(|t| t == "Summary") {
                                tabs.remove(pos);
                            }
                            if let Some(min_idx) = tabs.iter().position(|t| t == "Minutes") {
                                tabs.insert(min_idx + 1, "Summary".to_string());
                            }
                            match worker::summary::start_summary_worker() {
                                Ok((srx, cancel_new)) => {
                                    *summary_text = Some(String::new());
                                    *download_rx = Some(srx);
                                    *worker_cancel = Some(cancel_new);
                                    keep_rx = false;
                                    break false;
                                }
                                Err(msg) => {
                                    *summary_text = Some(msg);
                                }
                            }
                            tabs.push("Q/A".to_string());
                            if let Some(qa_idx) = tabs.iter().position(|t| t == "Q/A") {
                                *selected_tab = qa_idx;
                            }
                        } else {
                            keep_rx = false;
                            break false;
                        }
                    }
                    WorkerMessage::DownloadLog(line) => {
                        debug_lines.push(line);
                        if debug_lines.len() > DEBUG_MAX_LINES {
                            let overflow = debug_lines.len() - DEBUG_MAX_LINES;
                            debug_lines.drain(0..overflow);
                        }
                    }
                    WorkerMessage::SummaryChunk(chunk) => match summary_text.as_mut() {
                        Some(s) => s.push_str(&chunk),
                        None => *summary_text = Some(chunk),
                    },
                    WorkerMessage::SummaryDone(_res) => {
                        *download_rx = None;
                        *worker_cancel = None;
                        keep_rx = false;
                        break false;
                    }
                },
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };

        if keep_rx && !disconnected {
            *download_rx = Some(rx);
        }
        if disconnected && matches!(*download_status, DownloadStatus::Running) {
            *download_rx = None;
            *worker_cancel = None;
            *download_status = DownloadStatus::Failed;
            *download_error = Some("downloader process disconnected".to_string());
            *eta_text = None;
        }
    }

    if matches!(*download_status, DownloadStatus::Running)
        && !*progress_synced
        && last_tick.elapsed() >= tick_rate
    {
        *progress = (*progress + 0.02) % 1.0;
        *last_tick = Instant::now();
    }
}
